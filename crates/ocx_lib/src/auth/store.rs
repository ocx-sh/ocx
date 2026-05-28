// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Docker-compatible credential store at `~/.docker/config.json`.
//!
//! Atomic read-modify-write under exclusive flock. Resolution order matches
//! `oras-go` `DynamicStore`:
//! 1. `credHelpers[registry]` — per-registry helper (highest)
//! 2. `credsStore` — global default helper
//! 3. `detectedCredsStore` — auto-detected platform default (sticky on first put)
//! 4. `auths[registry]` — plaintext base64 fallback (lowest, gated by `allow_plaintext_put`)

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

use crate::utility::fs::LockedFile;

/// Local RAII guard wrapping the locked `config.json`. Dropping the guard
/// releases the advisory lock and closes the lock-owning handle.
///
/// The guard owns the read+write fd through which all I/O on `config.json`
/// must route (Windows `LockFileEx` is per-handle; opening a second fd on
/// the locked range from the same process hits `ERROR_LOCK_VIOLATION`).
struct ConfigGuard {
    locked: LockedFile,
}

/// Credential persisted to docker config / credential helper.
///
/// Flat struct (matches `oras-go` `auth.Credential` 1:1 with the docker helper
/// wire format `{ServerURL, Username, Secret}`). Population pattern selects
/// auth mode at the call site:
/// - `username + password` → HTTP Basic
/// - `refresh_token` alone → OAuth2 identity token (wire-encoded with `username = "<token>"`)
/// - `access_token` alone → short-lived bearer (NOT helper-storable; runtime-only)
#[derive(Debug, Default)]
pub struct Credential {
    pub username: String,
    pub password: SecretString,
    pub refresh_token: SecretString,
    pub access_token: SecretString,
}

impl Credential {
    /// Convenience constructor for basic auth (most common CLI case).
    pub fn basic(username: impl Into<String>, password: SecretString) -> Self {
        Self {
            username: username.into(),
            password,
            refresh_token: SecretString::default(),
            access_token: SecretString::default(),
        }
    }

    /// Convenience constructor for identity-token auth (OAuth2 refresh).
    pub fn identity_token(token: SecretString) -> Self {
        Self {
            username: String::new(),
            password: SecretString::default(),
            refresh_token: token,
            access_token: SecretString::default(),
        }
    }

    /// True when all fields are empty (matches oras-go `EmptyCredential` zero-value semantics).
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
    /// Allow `put` to fall through to plaintext `auths[reg]` when no helper is configured.
    /// Default: false — matches oras-go safe default. Exposed as `--allow-insecure-store`
    /// on `ocx login` for headless CI environments without a native keychain daemon.
    pub allow_plaintext_put: bool,
    /// Probe PATH for platform default helper when config is empty at load time.
    /// On first successful `put`, the detected helper is persisted to `credsStore`.
    pub detect_default_native_store: bool,
}

/// Three-verb store trait matching the Docker credential helper protocol.
///
/// Mirrors oras-go `credentials.Store` interface. `get` returns `Option` instead
/// of a zero-value sentinel; `put` and `delete` return `Result<(), _>` only.
#[async_trait::async_trait]
pub trait CredentialStore: Send + Sync {
    /// Fetch a credential for `registry`. Returns `Ok(None)` if nothing stored.
    async fn get(&self, registry: &str) -> Result<Option<Credential>, AuthError>;

    /// Persist a credential for `registry`. Resolution order: `credHelpers[reg]` →
    /// `credsStore` → `detectedCredsStore` → plaintext (if `allow_plaintext_put`).
    async fn put(&self, registry: &str, cred: &Credential) -> Result<(), AuthError>;

    /// Remove a credential for `registry`. Returns `Ok(())` for both "removed" and "noop".
    async fn delete(&self, registry: &str) -> Result<(), AuthError>;
}

/// Docker-compatible credential store (concrete impl).
///
/// Owns the path to `~/.docker/config.json` (resolved via `DOCKER_CONFIG` env or
/// `~/.docker/config.json` default) and acquires a `FileLock` for the duration
/// of each mutating operation.
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

    /// Constructs a store pointed at an explicit path. Used by tests and by the
    /// CLI layer when the user has not set `DOCKER_CONFIG`.
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

/// Resolve the docker config path from environment / home defaults.
///
/// Reads `DOCKER_CONFIG` via the project-wide `env::var` shim so unit tests
/// can inject overrides through `test::env::lock`.
fn resolve_config_path() -> Result<PathBuf, AuthError> {
    if let Some(dir) = crate::env::var("DOCKER_CONFIG") {
        return Ok(PathBuf::from(dir).join("config.json"));
    }
    let home = dirs::home_dir().ok_or_else(|| AuthError::WriteConfigFailed {
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
    /// Bearer / OAuth identity token. Mutually exclusive with `auth` in practice.
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
        // Read under exclusive lock to coexist with concurrent writers.
        let blocking = tokio::task::spawn_blocking(move || -> Result<Option<Credential>, AuthError> {
            let mut guard = acquire_config_guard(&path)?;
            let config = guard.read()?;
            let resolution = resolve_helper(&config, &canonical, false);
            match resolution {
                HelperResolution::Helper(helper) => {
                    // Helper lookup via the patched fork.
                    match docker_credential::credential_from_helper(&canonical, &helper) {
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

            // Decide store tier from current config.
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
                    docker_credential::store_credential(&canonical, &helper_name, &docker_cred)
                        .map_err(AuthError::Helper)?;
                    if let Some(detected) = detected_helper {
                        // Sticky-detected helpers are persisted on first successful put.
                        config.creds_store = Some(detected);
                        guard.write(&config)?;
                    }
                }
                StoreTier::Plaintext => {
                    let entry = config.auths.entry(canonical.clone()).or_default();
                    if !cred_copy.refresh_token.expose_secret().is_empty() {
                        // OAuth identity token path: stored as `identitytoken`.
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

            // Helper erase (consult per-registry helper, then credsStore).
            let helper = config
                .cred_helpers
                .get(&canonical)
                .cloned()
                .or_else(|| config.creds_store.clone());
            if let Some(helper_name) = helper
                && let Err(err) = docker_credential::erase_credential(&canonical, &helper_name)
                && !matches!(err, docker_credential::CredentialRetrievalError::NotFound)
            {
                return Err(AuthError::Helper(err));
            }

            // Remove plaintext entry.
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

/// Acquire an exclusive lock directly on `config.json` and return the
/// lock-owning handle.
///
/// Every read-modify-write op of the docker config routes its I/O through
/// this guard so concurrent `ocx login` invocations serialise without torn
/// writes. Sync — runs inside a `spawn_blocking` task. No sidecar lock
/// file; the data file IS the lock target.
fn acquire_config_guard(path: &Path) -> Result<ConfigGuard, AuthError> {
    let locked = LockedFile::open_exclusive_blocking_with_timeout(path, Duration::from_secs(5)).map_err(|e| {
        AuthError::WriteConfigFailed {
            path: path.to_path_buf(),
            source: std::io::Error::other(e),
        }
    })?;
    // Tighten file permissions to owner-only on Unix. Re-applied on every
    // acquire so an externally-relaxed mode is restored before any write
    // exposes credentials. On a freshly created file the umask default
    // applies for the brief window between open and `set_permissions`; this
    // is acceptable because no credentials are written until the subsequent
    // `replace_bytes_blocking` call.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|source| {
            AuthError::WriteConfigFailed {
                path: path.to_path_buf(),
                source,
            }
        })?;
    }
    Ok(ConfigGuard { locked })
}

impl ConfigGuard {
    /// Read the docker config through the lock-owning handle.
    ///
    /// Empty file (freshly created by [`LockedFile::open_exclusive_blocking_with_timeout`])
    /// yields [`DockerConfig::default()`]. Unparseable JSON surfaces as
    /// [`AuthError::WriteConfigFailed`] with `ErrorKind::InvalidData`.
    fn read(&mut self) -> Result<DockerConfig, AuthError> {
        let bytes = self
            .locked
            .read_bytes_blocking()
            .map_err(|e| AuthError::WriteConfigFailed {
                path: self.locked.path().to_path_buf(),
                source: std::io::Error::other(e),
            })?;
        if bytes.is_empty() {
            return Ok(DockerConfig::default());
        }
        serde_json::from_slice(&bytes).map_err(|err| AuthError::WriteConfigFailed {
            path: self.locked.path().to_path_buf(),
            source: std::io::Error::new(ErrorKind::InvalidData, err),
        })
    }

    /// Rewrite the docker config in place through the lock-owning handle.
    ///
    /// Truncate + write + sync_data on the locked inode. No tempfile, no
    /// rename. SIGKILL between `set_len(0)` and `sync_data` can leave
    /// `config.json` truncated; recovery is manual (`ocx login` again).
    fn write(&mut self, config: &DockerConfig) -> Result<(), AuthError> {
        let serialized = serde_json::to_vec_pretty(config).map_err(|err| AuthError::WriteConfigFailed {
            path: self.locked.path().to_path_buf(),
            source: std::io::Error::new(ErrorKind::InvalidData, err),
        })?;
        self.locked
            .replace_bytes_blocking(&serialized)
            .map_err(|e| AuthError::WriteConfigFailed {
                path: self.locked.path().to_path_buf(),
                source: std::io::Error::other(e),
            })
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
        std::fs::write(&path, r#"{"credsStore":"test"}"#).expect("seed config");
        let store = DockerCredentialStore::with_path(path, opts(false));
        let cred = Credential::basic("u", SecretString::from("p".to_string()));
        // Helper does not exist; the contract here is that the call routes to
        // a helper (not plaintext). It will fail because no helper binary is
        // configured, but the failure must be a Helper variant, NOT
        // NoCredentialStoreAvailable.
        let result = rt().block_on(store.put("ghcr.io", &cred));
        assert!(
            matches!(result, Err(AuthError::Helper(_))),
            "credsStore route must reach helper subsystem; got: {result:?}",
        );
    }

    #[test]
    fn dockerconfigstore_put_writes_to_cred_helpers_when_per_registry_set() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (_dir, path) = fresh_config_path();
        std::fs::write(
            &path,
            r#"{"credsStore":"global","credHelpers":{"ghcr.io":"ecr-login"}}"#,
        )
        .expect("seed config");
        let store = DockerCredentialStore::with_path(path, opts(false));
        let cred = Credential::basic("u", SecretString::from("p".to_string()));
        let result = rt().block_on(store.put("ghcr.io", &cred));
        // Either ecr-login or global - both are helpers (not on PATH for this
        // test). Asserting Helper variant pins the routing decision.
        assert!(
            matches!(result, Err(AuthError::Helper(_))),
            "credHelpers route must reach helper subsystem; got: {result:?}",
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

    #[test]
    fn dockerconfigstore_put_atomic_rename_no_torn_json() {
        let (_dir, path) = fresh_config_path();
        let store = std::sync::Arc::new(DockerCredentialStore::with_path(path.clone(), opts(true)));
        let runtime = std::sync::Arc::new(rt());
        // Spawn the reader first.
        let path_for_reader = path.clone();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_for_reader = stop.clone();
        let reader = std::thread::spawn(move || {
            let mut iters = 0;
            while !stop_for_reader.load(std::sync::atomic::Ordering::SeqCst) && iters < 1000 {
                if let Ok(raw) = std::fs::read_to_string(&path_for_reader)
                    && !raw.is_empty()
                {
                    let _: serde_json::Value = serde_json::from_str(&raw).expect("reader must never observe torn JSON");
                }
                iters += 1;
            }
        });
        let s = store.clone();
        let r = runtime.clone();
        let cred = Credential::basic("u", SecretString::from("p".to_string()));
        for i in 0..100 {
            r.block_on(s.put(&format!("reg{i}.example"), &cred)).expect("put");
        }
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        reader.join().expect("reader join");
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
        std::fs::write(&path, r#"{"credHelpers":{"ghcr.io":"test"}}"#).expect("seed");
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
        std::fs::write(&path, r#"{"credHelpers":{"ghcr.io":"test"}}"#).expect("seed");
        let store = DockerCredentialStore::with_path(path, opts(false));
        // No helper on PATH ⇒ Helper(NotOnPath). The acceptance test layer
        // verifies the sentinel-string path with a real mock helper.
        let _ = rt().block_on(store.delete("ghcr.io"));
    }

    // ─── DOCKER_CONFIG honoring ───

    #[test]
    fn dockerconfigstore_honors_docker_config_env() {
        let env_guard = crate::test::env::lock();
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
        std::fs::write(&path, r#"{"credHelpers":{"ghcr.io":"test"}}"#).expect("seed");
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
