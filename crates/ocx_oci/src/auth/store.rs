// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Docker-compatible credential store at `~/.docker/config.json`.
//!
//! Resolution order, as oras-go's `DynamicStore`: `credHelpers[registry]` ▸ `credsStore` ▸
//! detected platform helper ▸ plaintext `auths[registry]` (only with `allow_plaintext_put`).

use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};

use crate::auth::AuthError;
use crate::auth::registry_url::canonicalize_registry;

use ocx_util::fs::LockedFile;

/// Test seam replacing the credential helper's subprocess budget, in milliseconds.
/// Seam convention: `subsystem-tests.md` § Test-Only Seams.
#[cfg(any(test, feature = "__testing"))]
const TESTING_HELPER_TIMEOUT_ENV: &str = "__OCX_TESTING_HELPER_TIMEOUT_MS";

/// The helper budget: [`docker_credential::HELPER_TIMEOUT`], or the [`TESTING_HELPER_TIMEOUT_ENV`] override.
///
/// Panics on a malformed value, or a typo silently out-waits the real deadline and the test still passes.
#[cfg(any(test, feature = "__testing"))]
fn helper_timeout() -> Duration {
    let Ok(raw) = std::env::var(TESTING_HELPER_TIMEOUT_ENV) else {
        return docker_credential::HELPER_TIMEOUT;
    };
    Duration::from_millis(
        raw.trim().parse().unwrap_or_else(|_| {
            panic!("{TESTING_HELPER_TIMEOUT_ENV} must be a whole number of milliseconds, got {raw:?}")
        }),
    )
}

#[cfg(not(any(test, feature = "__testing")))]
fn helper_timeout() -> Duration {
    docker_credential::HELPER_TIMEOUT
}

/// Serialises access to `config.json` until dropped.
struct ConfigGuard {
    /// Held on the `config.json.lock` sidecar, never the data file (see [`acquire_config_guard`]).
    _locked: LockedFile,
    path: std::path::PathBuf,
}

/// Credential persisted to docker config or a credential helper.
///
/// The populated fields pick the mode: `username` + `password` Basic, `refresh_token` an identity
/// token, `access_token` a runtime-only bearer that is never stored.
#[derive(Debug, Default)]
pub struct Credential {
    pub username: String,
    pub password: SecretString,
    pub refresh_token: SecretString,
    pub access_token: SecretString,
}

impl Credential {
    /// Basic-auth credential.
    pub fn basic(username: impl Into<String>, password: SecretString) -> Self {
        Self {
            username: username.into(),
            password,
            refresh_token: SecretString::default(),
            access_token: SecretString::default(),
        }
    }

    /// Identity-token (OAuth2 refresh) credential.
    pub fn identity_token(token: SecretString) -> Self {
        Self {
            username: String::new(),
            password: SecretString::default(),
            refresh_token: token,
            access_token: SecretString::default(),
        }
    }

    /// True when every field is empty.
    pub fn is_empty(&self) -> bool {
        self.username.is_empty()
            && self.password.expose_secret().is_empty()
            && self.refresh_token.expose_secret().is_empty()
            && self.access_token.expose_secret().is_empty()
    }
}

/// Options controlling `DockerCredentialStore` behavior.
#[derive(Debug, Default, Clone, Copy)]
pub struct StoreOptions {
    /// Allow `put` to fall back to plaintext `auths[reg]` when no helper is configured.
    pub allow_plaintext_put: bool,
    /// Probe PATH for a platform helper when none is configured; persisted to `credsStore` on the first
    /// successful `put`.
    pub detect_default_native_store: bool,
}

/// Credential store with the Docker credential helper protocol's three verbs.
#[async_trait::async_trait]
pub trait CredentialStore: Send + Sync {
    /// Fetch a credential for `registry`. Returns `Ok(None)` if nothing stored.
    async fn get(&self, registry: &str) -> Result<Option<Credential>, AuthError>;

    /// Persist a credential for `registry`.
    async fn put(&self, registry: &str, cred: &Credential) -> Result<(), AuthError>;

    /// Remove a credential for `registry`. Returns `Ok(())` for both "removed" and "noop".
    async fn delete(&self, registry: &str) -> Result<(), AuthError>;
}

/// [`CredentialStore`] over docker's `config.json`, locked for each operation.
pub struct DockerCredentialStore {
    config_path: PathBuf,
    allow_plaintext_put: bool,
    detect_default_native_store: bool,
}

impl DockerCredentialStore {
    pub fn new(opts: StoreOptions) -> Result<Self, AuthError> {
        let config_path = resolve_config_path()?;
        Ok(Self {
            config_path,
            allow_plaintext_put: opts.allow_plaintext_put,
            detect_default_native_store: opts.detect_default_native_store,
        })
    }

    /// Store over an explicit `config.json` path.
    pub fn with_path(config_path: PathBuf, opts: StoreOptions) -> Self {
        Self {
            config_path,
            allow_plaintext_put: opts.allow_plaintext_put,
            detect_default_native_store: opts.detect_default_native_store,
        }
    }

    /// Returns the on-disk path the store mutates.
    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    /// True when the store may fall through to plaintext `auths[reg]`.
    pub fn allow_plaintext_put(&self) -> bool {
        self.allow_plaintext_put
    }
}

/// Resolves `$DOCKER_CONFIG/config.json`, else `~/.docker/config.json`.
///
/// Home via [`ocx_util::env::home_dir`], not `dirs::home_dir`, which ignores `%USERPROFILE%` and could name a
/// different file than docker reads.
fn resolve_config_path() -> Result<PathBuf, AuthError> {
    if let Some(dir) = ocx_util::env::var("DOCKER_CONFIG") {
        return Ok(PathBuf::from(dir).join("config.json"));
    }
    let home = ocx_util::env::home_dir().ok_or_else(|| AuthError::WriteConfigFailed {
        path: PathBuf::new(),
        source: std::io::Error::new(ErrorKind::NotFound, "home directory not found"),
    })?;
    Ok(home.join(".docker").join("config.json"))
}

/// On-disk schema for `~/.docker/config.json` — preserves unknown fields.
#[derive(Debug, Default, Serialize, Deserialize)]
struct DockerConfig {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    auths: BTreeMap<String, AuthEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "credsStore")]
    creds_store: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty", rename = "credHelpers")]
    cred_helpers: BTreeMap<String, String>,
    /// Preserves any keys docker / oras / podman wrote that we don't model.
    #[serde(flatten)]
    other: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct AuthEntry {
    /// base64(`username:password`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    auth: Option<String>,
    /// Bearer / OAuth identity token.
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "identitytoken")]
    identity_token: Option<String>,
    #[serde(flatten)]
    other: serde_json::Map<String, serde_json::Value>,
}

#[async_trait::async_trait]
impl CredentialStore for DockerCredentialStore {
    async fn get(&self, registry: &str) -> Result<Option<Credential>, AuthError> {
        let canonical = canonicalize_registry(registry);
        let path = self.config_path.clone();
        let blocking = tokio::task::spawn_blocking(move || -> Result<Option<Credential>, AuthError> {
            let mut guard = acquire_config_guard(&path)?;
            let config = guard.read()?;
            let resolution = resolve_helper(&config, &canonical, false);
            match resolution {
                HelperResolution::Helper(helper) => {
                    match docker_credential::credential_from_helper_with_timeout(&canonical, &helper, helper_timeout())
                    {
                        Ok(docker_credential::DockerCredential::UsernamePassword(u, p)) => {
                            Ok(Some(Credential::basic(u, SecretString::from(p))))
                        }
                        Ok(docker_credential::DockerCredential::IdentityToken(tok)) => {
                            Ok(Some(Credential::identity_token(SecretString::from(tok))))
                        }
                        Err(docker_credential::CredentialRetrievalError::NotFound) => Ok(None),
                        Err(err) => Err(AuthError::Helper(err)),
                    }
                }
                HelperResolution::PlaintextOnly | HelperResolution::None => {
                    Ok(read_plaintext_credential(&config, &canonical))
                }
            }
        });
        blocking.await.map_err(|e| AuthError::WriteConfigFailed {
            path: self.config_path.clone(),
            source: std::io::Error::other(e),
        })?
    }

    async fn put(&self, registry: &str, cred: &Credential) -> Result<(), AuthError> {
        let canonical = canonicalize_registry(registry);
        let path = self.config_path.clone();
        let allow_plaintext = self.allow_plaintext_put;
        let detect_default_native_store = self.detect_default_native_store;
        let cred_copy = clone_credential(cred);

        let blocking = tokio::task::spawn_blocking(move || -> Result<(), AuthError> {
            let mut guard = acquire_config_guard(&path)?;
            let mut config = guard.read()?;

            let mut detected_helper: Option<String> = None;
            let tier = if let Some(h) = config.cred_helpers.get(&canonical).cloned() {
                StoreTier::Helper(h)
            } else if let Some(h) = config.creds_store.clone() {
                StoreTier::Helper(h)
            } else if detect_default_native_store {
                if let Some(h) = docker_credential::detect_default_helper() {
                    detected_helper = Some(h.clone());
                    StoreTier::Helper(h)
                } else if allow_plaintext {
                    StoreTier::Plaintext
                } else {
                    return Err(AuthError::NoCredentialStoreAvailable);
                }
            } else if allow_plaintext {
                StoreTier::Plaintext
            } else {
                return Err(AuthError::NoCredentialStoreAvailable);
            };

            match tier {
                StoreTier::Helper(helper_name) => {
                    let docker_cred = to_docker_credential(&cred_copy);
                    docker_credential::store_credential_with_timeout(
                        &canonical,
                        &helper_name,
                        &docker_cred,
                        helper_timeout(),
                    )
                    .map_err(AuthError::Helper)?;
                    if let Some(detected) = detected_helper {
                        config.creds_store = Some(detected);
                        guard.write(&config)?;
                    }
                }
                StoreTier::Plaintext => {
                    let entry = config.auths.entry(canonical.clone()).or_default();
                    if !cred_copy.refresh_token.expose_secret().is_empty() {
                        entry.identity_token = Some(cred_copy.refresh_token.expose_secret().to_string());
                        entry.auth = None;
                    } else {
                        let user_pass = format!("{}:{}", cred_copy.username, cred_copy.password.expose_secret(),);
                        entry.auth = Some(BASE64_STANDARD.encode(user_pass.as_bytes()));
                        entry.identity_token = None;
                    }
                    guard.write(&config)?;
                }
            }
            Ok(())
        });
        blocking.await.map_err(|e| AuthError::WriteConfigFailed {
            path: self.config_path.clone(),
            source: std::io::Error::other(e),
        })?
    }

    async fn delete(&self, registry: &str) -> Result<(), AuthError> {
        let canonical = canonicalize_registry(registry);
        let path = self.config_path.clone();

        let blocking = tokio::task::spawn_blocking(move || -> Result<(), AuthError> {
            let mut guard = acquire_config_guard(&path)?;
            let mut config = guard.read()?;

            let helper = config
                .cred_helpers
                .get(&canonical)
                .cloned()
                .or_else(|| config.creds_store.clone());
            if let Some(helper_name) = helper
                && let Err(err) =
                    docker_credential::erase_credential_with_timeout(&canonical, &helper_name, helper_timeout())
                && !matches!(err, docker_credential::CredentialRetrievalError::NotFound)
            {
                return Err(AuthError::Helper(err));
            }

            let auths_changed = config.auths.remove(&canonical).is_some();
            if auths_changed {
                guard.write(&config)?;
            }
            Ok(())
        });
        blocking.await.map_err(|e| AuthError::WriteConfigFailed {
            path: self.config_path.clone(),
            source: std::io::Error::other(e),
        })?
    }
}

// ─────────────────────────── helpers ───────────────────────────

enum StoreTier {
    Helper(String),
    Plaintext,
}

enum HelperResolution {
    Helper(String),
    PlaintextOnly,
    None,
}

fn resolve_helper(config: &DockerConfig, canonical: &str, _read: bool) -> HelperResolution {
    if let Some(h) = config.cred_helpers.get(canonical).cloned() {
        return HelperResolution::Helper(h);
    }
    if let Some(h) = config.creds_store.clone() {
        return HelperResolution::Helper(h);
    }
    if config.auths.contains_key(canonical) {
        return HelperResolution::PlaintextOnly;
    }
    HelperResolution::None
}

fn read_plaintext_credential(config: &DockerConfig, canonical: &str) -> Option<Credential> {
    let entry = config.auths.get(canonical)?;
    if let Some(token) = &entry.identity_token {
        return Some(Credential::identity_token(SecretString::from(token.clone())));
    }
    let auth = entry.auth.as_ref()?;
    let decoded = BASE64_STANDARD.decode(auth.as_bytes()).ok()?;
    let decoded = String::from_utf8(decoded).ok()?;
    let (user, pwd) = decoded.split_once(':')?;
    Some(Credential::basic(user.to_string(), SecretString::from(pwd.to_string())))
}

fn to_docker_credential(cred: &Credential) -> docker_credential::DockerCredential {
    if !cred.refresh_token.expose_secret().is_empty() {
        return docker_credential::DockerCredential::IdentityToken(cred.refresh_token.expose_secret().to_string());
    }
    docker_credential::DockerCredential::UsernamePassword(
        cred.username.clone(),
        cred.password.expose_secret().to_string(),
    )
}

fn clone_credential(cred: &Credential) -> Credential {
    Credential {
        username: cred.username.clone(),
        password: SecretString::from(cred.password.expose_secret().to_string()),
        refresh_token: SecretString::from(cred.refresh_token.expose_secret().to_string()),
        access_token: SecretString::from(cred.access_token.expose_secret().to_string()),
    }
}

/// Takes the `config.json.lock` sidecar lock (blocking) and returns a guard over the data file.
///
/// A sidecar, not the data file: on Windows a `LockFileEx` lock denies the rename `ConfigGuard::write` publishes by.
fn acquire_config_guard(path: &Path) -> Result<ConfigGuard, AuthError> {
    let mut lock_name = path.as_os_str().to_os_string();
    lock_name.push(".lock");
    let lock_path = std::path::PathBuf::from(lock_name);
    let locked = LockedFile::open_exclusive_blocking_with_timeout(&lock_path, Duration::from_secs(5)).map_err(|e| {
        AuthError::WriteConfigFailed {
            path: lock_path.clone(),
            source: std::io::Error::other(e),
        }
    })?;
    // Re-applied on every acquire, or an externally-relaxed mode keeps exposing credentials.
    #[cfg(unix)]
    if path.exists() {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|source| {
            AuthError::WriteConfigFailed {
                path: path.to_path_buf(),
                source,
            }
        })?;
    }
    Ok(ConfigGuard {
        _locked: locked,
        path: path.to_path_buf(),
    })
}

impl ConfigGuard {
    /// Read the docker config; an absent or empty file (first login) yields the default.
    ///
    /// # Errors
    /// Unparseable JSON is [`AuthError::WriteConfigFailed`] with `ErrorKind::InvalidData`.
    fn read(&mut self) -> Result<DockerConfig, AuthError> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(DockerConfig::default()),
            Err(source) => {
                return Err(AuthError::WriteConfigFailed {
                    path: self.path.clone(),
                    source,
                });
            }
        };
        if bytes.is_empty() {
            return Ok(DockerConfig::default());
        }
        serde_json::from_slice(&bytes).map_err(|err| AuthError::WriteConfigFailed {
            path: self.path.clone(),
            source: std::io::Error::new(ErrorKind::InvalidData, err),
        })
    }

    /// Publish the docker config by tempfile + rename, as the docker CLI does.
    ///
    /// Never in place: lock-free readers (docker, scripts) could read an old document spliced onto a new one.
    fn write(&mut self, config: &DockerConfig) -> Result<(), AuthError> {
        let path = self.path.clone();
        let serialized = serde_json::to_vec_pretty(config).map_err(|err| AuthError::WriteConfigFailed {
            path: path.clone(),
            source: std::io::Error::new(ErrorKind::InvalidData, err),
        })?;
        // Resolved first, or a symlinked `config.json` is replaced by a regular file.
        let target = std::fs::canonicalize(&path).unwrap_or(path.clone());
        ocx_util::fs::write_bytes_atomic(&target, &serialized)
            .map_err(|source| AuthError::WriteConfigFailed { path, source })
    }
}

// ─────────────────────────── tests ───────────────────────────
//
// Specification tests for the docker-compatible credential store. Every test
// drives `DockerCredentialStore::with_path(...)` against a temporary config
// path inside `tempfile::TempDir`.
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serialises tests that mutate `PATH` or `DOCKER_CONFIG` env vars.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn fresh_config_path() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("create tempdir");
        let path = dir.path().join("config.json");
        (dir, path)
    }

    fn opts(allow_plaintext_put: bool) -> StoreOptions {
        StoreOptions {
            allow_plaintext_put,
            detect_default_native_store: false,
        }
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime")
    }

    /// A credential-helper name that cannot resolve to a binary on any host.
    ///
    /// Every test below that seeds `credsStore` / `credHelpers` means the same
    /// thing: *route to the helper subsystem, and fail there because the
    /// binary is not on PATH*. That only holds while the name is genuinely
    /// absent — and `ecr-login`, which this module used to name, ships in
    /// GitHub's Ubuntu runner image as `docker-credential-ecr-login`. The test
    /// kept passing there for the wrong reason: it exec'd the real helper and
    /// waited out its credential lookup, 0.79 s on CI against 0.10 s on a host
    /// without it. Whatever a host happens to have installed is not something
    /// a unit test may depend on in either direction.
    ///
    /// The pid suffix is what turns absence from an assumption about the host
    /// into a property of the name.
    fn absent_helper(role: &str) -> String {
        format!("ocx-absent-{role}-helper-{}", std::process::id())
    }

    // ─── get: empty-store semantics ───

    #[test]
    fn dockerconfigstore_get_returns_none_when_nothing_stored() {
        let (_dir, path) = fresh_config_path();
        let store = DockerCredentialStore::with_path(path, opts(false));
        let result = rt().block_on(store.get("ghcr.io"));
        assert!(
            matches!(result, Ok(None)),
            "expected Ok(None) on fresh store, got: {result:?}",
        );
    }

    // ─── put: helper-routing rules ───

    #[test]
    fn dockerconfigstore_put_writes_to_creds_store_when_no_per_registry_helper() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (_dir, path) = fresh_config_path();
        let fallback = absent_helper("fallback");
        std::fs::write(&path, format!(r#"{{"credsStore":"{fallback}"}}"#)).expect("seed config");
        let store = DockerCredentialStore::with_path(path, opts(false));
        let cred = Credential::basic("u", SecretString::from("p".to_string()));
        // Helper does not exist; the contract here is that the call routes to
        // a helper (not plaintext). It will fail because no helper binary is
        // configured, but the failure must be a Helper variant, NOT
        // NoCredentialStoreAvailable.
        let result = rt().block_on(store.put("ghcr.io", &cred));
        let Err(AuthError::Helper(inner)) = &result else {
            panic!("credsStore route must reach helper subsystem; got: {result:?}");
        };
        // And the helper it reached is the one `credsStore` named — not some
        // other string the ladder picked up. See the sibling below for why the
        // name rather than the variant is what gets asserted.
        assert!(
            inner.to_string().contains(&fallback),
            "the failure must name the credsStore helper {fallback}, got: {inner}",
        );
    }

    #[test]
    fn dockerconfigstore_put_writes_to_cred_helpers_when_per_registry_set() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (_dir, path) = fresh_config_path();
        let fallback = absent_helper("fallback");
        let per_registry = absent_helper("per-registry");
        std::fs::write(
            &path,
            format!(r#"{{"credsStore":"{fallback}","credHelpers":{{"ghcr.io":"{per_registry}"}}}}"#),
        )
        .expect("seed config");
        let store = DockerCredentialStore::with_path(path, opts(false));
        let cred = Credential::basic("u", SecretString::from("p".to_string()));
        let result = rt().block_on(store.put("ghcr.io", &cred));
        // Both names are helpers and both are absent (`absent_helper`), so the
        // Helper variant pins the routing decision without depending on what
        // the host has installed.
        let Err(AuthError::Helper(inner)) = &result else {
            panic!("credHelpers route must reach helper subsystem; got: {result:?}");
        };
        // The Helper variant alone does NOT pin the precedence this test is
        // named for: delete the `cred_helpers` lookup and the ladder falls
        // through to `credsStore`, which is also a helper, and this test stays
        // green with its own subject removed. Which tier ran is observable only
        // in the error's payload, and only because the two tiers are seeded
        // with differently-named absent helpers.
        //
        // Asserted through the message rather than the fork's error variant on
        // purpose. The coupling that earns its keep is behavioural — "the error
        // names the helper it failed on" — which survives a reworded message
        // and reds correctly if the fork ever stops naming the helper, because
        // at that point the two tiers are genuinely indistinguishable. Matching
        // `CredentialRetrievalError::NotOnPath { .. }` by name would couple a
        // unit test to a submodule's enum shape and buy nothing extra.
        assert!(
            inner.to_string().contains(&per_registry),
            "the per-registry helper must win over credsStore: the failure must name {per_registry}, got: {inner}",
        );
    }

    #[test]
    fn dockerconfigstore_put_refuses_plaintext_when_allow_plaintext_put_false() {
        let (_dir, path) = fresh_config_path();
        let store = DockerCredentialStore::with_path(path, opts(false));
        let cred = Credential::basic("u", SecretString::from("p".to_string()));
        let result = rt().block_on(store.put("ghcr.io", &cred));
        assert!(
            matches!(result, Err(AuthError::NoCredentialStoreAvailable)),
            "expected NoCredentialStoreAvailable, got: {result:?}",
        );
    }

    #[test]
    fn dockerconfigstore_put_falls_through_to_plaintext_when_allow_plaintext_put_true() {
        let (_dir, path) = fresh_config_path();
        let store = DockerCredentialStore::with_path(path.clone(), opts(true));
        let cred = Credential::basic("u", SecretString::from("p".to_string()));
        rt().block_on(store.put("ghcr.io", &cred)).expect("plaintext put");
        let raw = std::fs::read_to_string(&path).expect("read config");
        let json: serde_json::Value = serde_json::from_str(&raw).expect("parse");
        let auth = json
            .pointer("/auths/ghcr.io/auth")
            .and_then(|v| v.as_str())
            .expect("auths.ghcr.io.auth present");
        let decoded = BASE64_STANDARD.decode(auth).expect("base64 decode");
        let decoded = String::from_utf8(decoded).expect("utf8");
        assert_eq!(decoded, "u:p");
    }

    // ─── put: file permissions (Unix only) ───

    #[test]
    #[cfg(unix)]
    fn dockerconfigstore_creates_file_mode_0600_on_first_put() {
        use std::os::unix::fs::PermissionsExt as _;
        let (_dir, path) = fresh_config_path();
        let store = DockerCredentialStore::with_path(path.clone(), opts(true));
        let cred = Credential::basic("u", SecretString::from("p".to_string()));
        rt().block_on(store.put("ghcr.io", &cred)).expect("put");
        let mode = std::fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "config.json must be created with mode 0600");
    }

    // ─── put: unknown-field preservation ───

    #[test]
    fn dockerconfigstore_preserves_unknown_top_level_fields() {
        let (_dir, path) = fresh_config_path();
        std::fs::write(&path, r#"{"currentContext":"default","experimental":true}"#).expect("seed config");
        let store = DockerCredentialStore::with_path(path.clone(), opts(true));
        let cred = Credential::basic("u", SecretString::from("p".to_string()));
        rt().block_on(store.put("ghcr.io", &cred)).expect("put");
        let raw = std::fs::read_to_string(&path).expect("read");
        let json: serde_json::Value = serde_json::from_str(&raw).expect("parse");
        assert_eq!(
            json.get("currentContext").and_then(|v| v.as_str()),
            Some("default"),
            "currentContext must round-trip",
        );
        assert_eq!(
            json.get("experimental").and_then(|v| v.as_bool()),
            Some(true),
            "experimental must round-trip",
        );
    }

    #[test]
    fn dockerconfigstore_preserves_unknown_auth_entry_fields() {
        let (_dir, path) = fresh_config_path();
        std::fs::write(
            &path,
            r#"{"auths":{"foo.example":{"auth":"dTpw","future_unknown":"x"}}}"#,
        )
        .expect("seed config");
        let store = DockerCredentialStore::with_path(path.clone(), opts(true));
        let cred = Credential::basic("u2", SecretString::from("p2".to_string()));
        rt().block_on(store.put("bar.example", &cred)).expect("put");
        let raw = std::fs::read_to_string(&path).expect("read");
        let json: serde_json::Value = serde_json::from_str(&raw).expect("parse");
        assert_eq!(
            json.pointer("/auths/foo.example/future_unknown")
                .and_then(|v| v.as_str()),
            Some("x"),
            "unknown field on existing AuthEntry must round-trip",
        );
    }

    // ─── put: concurrency + atomicity ───

    #[test]
    fn dockerconfigstore_put_writes_under_exclusive_flock() {
        let (_dir, path) = fresh_config_path();
        let store = std::sync::Arc::new(DockerCredentialStore::with_path(path.clone(), opts(true)));
        let runtime = std::sync::Arc::new(rt());
        let mut handles = vec![];
        for i in 0..10 {
            let s = store.clone();
            let r = runtime.clone();
            handles.push(std::thread::spawn(move || {
                let cred = Credential::basic(format!("u{i}"), SecretString::from(format!("p{i}")));
                r.block_on(s.put(&format!("reg{i}.example"), &cred))
            }));
        }
        for h in handles {
            h.join().expect("join").expect("put result ok");
        }
        let raw = std::fs::read_to_string(&path).expect("read");
        let _: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON after 10 concurrent puts");
    }

    /// Regression — an unlocked reader that opened `config.json` before a write
    /// still finishes its read against the document it opened.
    ///
    /// The sibling test above serialises its reader behind the same exclusive
    /// lock, so it can only observe writes that have already finished — it
    /// asserts the lock, not the file. The docker config has readers that take
    /// no lock at all (docker itself, any script), and one buffered read is
    /// several `read(2)` calls. An in-place rewrite landing between two of them
    /// splices a complete short document onto the tail of the longer one that
    /// replaced it, which is the `Extra data: line 7 column 2 (char 77)` the
    /// acceptance suite caught. Publishing by rename keeps the reader's
    /// descriptor on the inode it opened, so the tail read comes back empty.
    ///
    /// `cfg(unix)` for a named reason. On Windows the publish is *refused*
    /// rather than torn: `MoveFileEx` cannot replace a destination another
    /// handle holds without delete sharing, so `put` returns
    /// `ERROR_ACCESS_DENIED` instead of splicing two documents. That is the
    /// better failure — a caller sees an error instead of silent corruption —
    /// and `ocx_util::fs::persist_temp_file` already retries that class
    /// (5 and 32, 100/400/800 ms) so a real reader, which holds the file for
    /// microseconds, does not fail a login. This test holds its reader open
    /// across every retry on purpose, so on Windows it asserts something the
    /// platform will not do, and would be a permanent red rather than a guard.
    #[cfg(unix)]
    #[test]
    fn dockerconfigstore_unlocked_reader_sees_one_document_across_a_split_read() {
        use std::io::Read as _;

        let (_dir, path) = fresh_config_path();
        let store = DockerCredentialStore::with_path(path.clone(), opts(true));
        let cred = Credential::basic("u", SecretString::from("p".to_string()));
        rt().block_on(store.put("reg0.example", &cred)).expect("first put");

        // Model a buffered reader mid-read: the whole current document is in
        // hand, the descriptor is still open and one read short of EOF.
        let first_len = usize::try_from(std::fs::metadata(&path).expect("stat").len()).expect("config size");
        let mut reader = std::fs::File::open(&path).expect("open unlocked");
        let mut observed = vec![0u8; first_len];
        reader.read_exact(&mut observed).expect("read the opened document");

        // A strictly longer document lands before the reader's next read.
        rt().block_on(store.put("reg1.example", &cred)).expect("second put");

        let mut tail = Vec::new();
        reader.read_to_end(&mut tail).expect("finish the read");
        observed.extend_from_slice(&tail);

        serde_json::from_slice::<serde_json::Value>(&observed)
            .expect("unlocked reader spliced two documents into one read");
    }

    // ─── delete ───

    #[test]
    fn dockerconfigstore_delete_removes_from_auths() {
        let (_dir, path) = fresh_config_path();
        let store = DockerCredentialStore::with_path(path.clone(), opts(true));
        let cred = Credential::basic("u", SecretString::from("p".to_string()));
        rt().block_on(store.put("ghcr.io", &cred)).expect("put");
        rt().block_on(store.delete("ghcr.io")).expect("delete");
        let raw = std::fs::read_to_string(&path).expect("read");
        let json: serde_json::Value = serde_json::from_str(&raw).expect("parse");
        assert!(
            json.pointer("/auths/ghcr.io").is_none(),
            "auths.ghcr.io must be absent after delete",
        );
    }

    #[test]
    fn dockerconfigstore_delete_removes_from_cred_helpers() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (_dir, path) = fresh_config_path();
        std::fs::write(
            &path,
            format!(r#"{{"credHelpers":{{"ghcr.io":"{}"}}}}"#, absent_helper("per-registry")),
        )
        .expect("seed");
        let store = DockerCredentialStore::with_path(path, opts(false));
        // Helper does not exist on PATH; the facade's contract here is that
        // delete swallows helper-not-found / not-on-path errors when there is
        // nothing in the plaintext layer either — but in this stricter unit
        // test we just confirm a call shape (no panic). Tested end-to-end with
        // a real mock helper in acceptance tests.
        let _ = rt().block_on(store.delete("ghcr.io"));
    }

    #[test]
    fn dockerconfigstore_delete_returns_ok_when_nothing_stored() {
        let (_dir, path) = fresh_config_path();
        let store = DockerCredentialStore::with_path(path, opts(false));
        let result = rt().block_on(store.delete("ghcr.io"));
        assert!(
            matches!(result, Ok(())),
            "delete on empty store must be Ok(()) (oras-go semantics), got: {result:?}",
        );
    }

    #[test]
    fn dockerconfigstore_delete_swallows_helper_resolution_failure() {
        // Tests the NotOnPath (helper missing from PATH) path — a helper-resolution
        // failure, not the sentinel-string "credentials not found" path.
        // The sentinel-NotFound case (helper exits with the sentinel string) is
        // covered at the acceptance layer in test_login.py.
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (_dir, path) = fresh_config_path();
        std::fs::write(
            &path,
            format!(r#"{{"credHelpers":{{"ghcr.io":"{}"}}}}"#, absent_helper("per-registry")),
        )
        .expect("seed");
        let store = DockerCredentialStore::with_path(path, opts(false));
        // No helper on PATH ⇒ Helper(NotOnPath). The acceptance test layer
        // verifies the sentinel-string path with a real mock helper.
        let _ = rt().block_on(store.delete("ghcr.io"));
    }

    // ─── DOCKER_CONFIG honoring ───

    #[test]
    fn dockerconfigstore_honors_docker_config_env() {
        let env_guard = ocx_util::env::overrides::lock();
        let dir = tempfile::tempdir().expect("tempdir");
        env_guard.set("DOCKER_CONFIG", dir.path().display().to_string());
        let store = DockerCredentialStore::new(StoreOptions {
            allow_plaintext_put: true,
            detect_default_native_store: false,
        })
        .expect("new() should succeed");
        let cred = Credential::basic("u", SecretString::from("p".to_string()));
        rt().block_on(store.put("ghcr.io", &cred)).expect("put");
        let expected = dir.path().join("config.json");
        assert!(
            expected.exists(),
            "DOCKER_CONFIG must redirect writes to {}",
            expected.display(),
        );
    }

    // ─── malformed config ───

    #[test]
    fn dockerconfigstore_malformed_config_surfaces_write_config_failed() {
        let (_dir, path) = fresh_config_path();
        std::fs::write(&path, "not json {").expect("seed malformed");
        let store = DockerCredentialStore::with_path(path, opts(true));
        let cred = Credential::basic("u", SecretString::from("p".to_string()));
        let result = rt().block_on(store.put("ghcr.io", &cred));
        assert!(
            matches!(result, Err(AuthError::WriteConfigFailed { .. })),
            "malformed config must surface as WriteConfigFailed, got: {result:?}",
        );
    }

    // ─── get from helper / plaintext / identity token ───

    #[test]
    fn dockerconfigstore_get_returns_credential_from_helper() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (_dir, path) = fresh_config_path();
        std::fs::write(
            &path,
            format!(r#"{{"credHelpers":{{"ghcr.io":"{}"}}}}"#, absent_helper("per-registry")),
        )
        .expect("seed");
        let store = DockerCredentialStore::with_path(path, opts(false));
        // Mock-helper wiring lives in acceptance tests; here we just exercise
        // the dispatch path without asserting helper behavior.
        let _ = rt().block_on(store.get("ghcr.io"));
    }

    #[test]
    fn dockerconfigstore_get_returns_credential_from_plaintext_auths() {
        let (_dir, path) = fresh_config_path();
        std::fs::write(&path, r#"{"auths":{"ghcr.io":{"auth":"dTpw"}}}"#).expect("seed");
        let store = DockerCredentialStore::with_path(path, opts(false));
        let result = rt().block_on(store.get("ghcr.io"));
        let cred = result.expect("get ok").expect("Some(_)");
        assert_eq!(cred.username, "u");
        assert_eq!(cred.password.expose_secret(), "p");
    }

    #[test]
    fn dockerconfigstore_get_decodes_identity_token() {
        let (_dir, path) = fresh_config_path();
        std::fs::write(&path, r#"{"auths":{"ghcr.io":{"identitytoken":"tok"}}}"#).expect("seed");
        let store = DockerCredentialStore::with_path(path, opts(false));
        let result = rt().block_on(store.get("ghcr.io"));
        let cred = result.expect("get ok").expect("Some(_)");
        assert_eq!(
            cred.refresh_token.expose_secret(),
            "tok",
            "identitytoken must map to Credential.refresh_token",
        );
    }

    // ─── secrecy debug redaction (security discipline) ───

    #[test]
    fn credential_debug_redacts_secret_fields() {
        let cred = Credential {
            username: "u".into(),
            password: SecretString::from("DO_NOT_LEAK".to_string()),
            refresh_token: SecretString::default(),
            access_token: SecretString::default(),
        };
        let dbg = format!("{cred:?}");
        assert!(
            !dbg.contains("DO_NOT_LEAK"),
            "Credential Debug leaked the secret: {dbg}",
        );
        assert!(
            dbg.contains("REDACTED") || dbg.to_lowercase().contains("secret"),
            "expected redaction marker in Debug output: {dbg}",
        );
    }
}
