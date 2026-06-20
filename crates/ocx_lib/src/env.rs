// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

use crate::{log, utility};

/// Canonical names for `OCX_*` environment variables read or written by ocx.
///
/// Single source of truth so spawn helpers, config loaders, docs, and tests
/// reference the same string in one place.
pub mod keys {
    /// Absolute path to the running `ocx` executable. Set on every subprocess
    /// spawn so child ocx invocations (e.g. via generated entrypoint launchers)
    /// pin to the same binary instead of whatever `$PATH` happens to resolve.
    /// Named `_PIN` to make the pin semantics explicit — the value is
    /// the specific binary that was running when the package was installed.
    pub const OCX_BINARY_PIN: &str = "OCX_BINARY_PIN";
    /// Boolean — disables network access when truthy. Mirrors `--offline`.
    pub const OCX_OFFLINE: &str = "OCX_OFFLINE";
    /// Boolean — freezes tag resolution to the local index when truthy:
    /// an unpinned (tag-only) reference missing from the local index errors
    /// instead of being fetched and committed. Digest-pinned content still
    /// fetches over the network. Mirrors `--frozen`.
    pub const OCX_FROZEN: &str = "OCX_FROZEN";
    /// Boolean — uses the remote index by default when truthy. Mirrors `--remote`.
    pub const OCX_REMOTE: &str = "OCX_REMOTE";
    /// Path to an explicit configuration file. Mirrors `--config`.
    pub const OCX_CONFIG: &str = "OCX_CONFIG";
    /// Boolean — when truthy, skip the discovered config-tier chain
    /// (system / user / `$OCX_HOME`). Explicit `--config` / [`OCX_CONFIG`]
    /// paths still load.
    pub const OCX_NO_CONFIG: &str = "OCX_NO_CONFIG";
    /// Path to an explicit project `ocx.toml` (project-tier toolchain config).
    /// Mirrors `--project`.
    pub const OCX_PROJECT: &str = "OCX_PROJECT";
    /// Boolean — when truthy, skip the CWD walk and the [`OCX_PROJECT`]
    /// env var. Explicit `--project` paths still load.
    pub const OCX_NO_PROJECT: &str = "OCX_NO_PROJECT";
    /// Boolean — when truthy, select the global toolchain
    /// (`$OCX_HOME/ocx.toml`) as the in-effect project file instead of
    /// discovering one via the CWD walk. Mirrors `--global`.
    pub const OCX_GLOBAL: &str = "OCX_GLOBAL";
    /// Path to the local index directory. Mirrors `--index`.
    pub const OCX_INDEX: &str = "OCX_INDEX";
    /// Path to the active patch snapshot file (`patches.snapshot.json`).
    ///
    /// When set, the compose overlay prefers the snapshot's pinned digests
    /// over live tag lookups for companions — enabling reproducible builds
    /// without a network round-trip.  Written by `ocx patch freeze`.
    /// Resolution-affecting → forwarded to child ocx processes via
    /// [`Env::apply_ocx_config`] so launchers apply the same frozen tier.
    pub const OCX_PATCH_SNAPSHOT: &str = "OCX_PATCH_SNAPSHOT";
    /// JSON object mapping an upstream registry host to its mirror `url`
    /// (e.g. `{"ghcr.io":"https://company.jfrog.io/ghcr-remote"}`).
    /// Resolution-affecting → forwarded to child ocx processes. A single JSON
    /// object is used (not a comma/`=` list) because mirror values are
    /// structured URLs with no delimiter-safe separator.
    pub const OCX_MIRRORS: &str = "OCX_MIRRORS";
    /// JSON object encoding the resolved `[patches]` config
    /// (`ResolvedPatchConfig`). Resolution-affecting → forwarded to child ocx
    /// processes so launchers re-apply the same patch tier (C5 across process
    /// boundaries). Forwarded only when `[patches]` is configured; absent
    /// otherwise. Mirrors `OCX_MIRRORS` in forwarding semantics.
    pub const OCX_PATCHES: &str = "OCX_PATCHES";
    /// Boolean — when truthy, `ocx self setup` writes the env shims but does
    /// NOT modify any shell profile. Mirrors `--no-modify-path`. Not a
    /// resolution-affecting flag (not forwarded to child ocx); the opt-out is
    /// not remembered between runs.
    pub const OCX_NO_MODIFY_PATH: &str = "OCX_NO_MODIFY_PATH";
}

/// Resolution-affecting policy snapshot, taken from the running ocx's parsed
/// `ContextOptions`. `Env::apply_ocx_config` writes this onto a child env so a
/// spawned ocx subprocess sees the same policy the parent saw, even when the
/// child env was built via [`Env::clean`].
///
/// Only carries flags that change *what* a child ocx resolves. Presentation
/// flags (`--log-level`, `--format`, `--color`) are intentionally absent —
/// generated launchers must remain opaque to the surrounding tool, so outer
/// presentation choices never propagate via env. See
/// `website/src/docs/reference/env-composition.md` for the full forwarding
/// rule.
#[derive(Debug, Clone)]
pub struct OcxConfigView {
    /// Absolute path to the running ocx executable.
    pub self_exe: PathBuf,
    pub offline: bool,
    pub remote: bool,
    /// When true, tag→digest resolution may only consult the local index; an
    /// unpinned (tag-only) miss errors instead of walking the source chain.
    /// Digest-pinned content still fetches. Resolution-affecting → forwarded
    /// as [`keys::OCX_FROZEN`] so a child ocx applies the same freeze.
    pub frozen: bool,
    pub config: Option<PathBuf>,
    pub project: Option<PathBuf>,
    /// When true, the global toolchain (`$OCX_HOME/ocx.toml`) is the
    /// in-effect project file. Resolution-affecting → forwarded as
    /// [`keys::OCX_GLOBAL`] so a child ocx selects the same tier.
    ///
    /// **Forwarding-coverage rationale (W2-P3, F6).** `OCX_GLOBAL` is
    /// forwarded by [`Env::apply_ocx_config`] like every other
    /// resolution-affecting flag, and that contract is pinned by the unit
    /// test `apply_ocx_config_sets_ocx_global_when_set` (the sole
    /// `OCX_*`-landing path). No *acceptance-level* end-to-end
    /// "child ocx inherits `OCX_GLOBAL`" test is exercised on purpose: under
    /// strict isolation (adr_global_toolchain_tier.md §Decision 4) the only
    /// path that re-enters `ocx` from a `run`/`exec` spawn is a generated
    /// entrypoint launcher (`ocx launcher exec`), an OCI-tier primitive that
    /// reads `metadata.json` and never consults `ocx.toml`/global project
    /// resolution. There is therefore no observable child-side resolution
    /// difference to assert; fabricating a recursive `ocx --global run`
    /// child to create one would test a contrived path strict isolation
    /// forbids by design. The block-tier "resolution flag forwarded"
    /// contract is fully pinned at the unit layer.
    pub global: bool,
    pub index: Option<PathBuf>,
    /// Per-upstream-host registry mirrors, as `(host, url)` pairs. Resolution-
    /// affecting → forwarded to child ocx processes via
    /// [`Env::apply_ocx_config`] as the single JSON object [`keys::OCX_MIRRORS`].
    /// The resolved map merges `[mirrors]` config with the inherited
    /// `OCX_MIRRORS` env, env winning per-host key.
    pub mirrors: Vec<(String, String)>,
    /// Resolved `[patches]` site-tier config. Resolution-affecting → forwarded
    /// to child ocx processes via [`Env::apply_ocx_config`] as a JSON object
    /// in [`keys::OCX_PATCHES`] so launchers apply the same patch tier (C5).
    /// `None` when no `[patches]` registry is configured.
    pub patches: Option<crate::config::patch::ResolvedPatchConfig>,
    /// Path to the active patch snapshot file. Resolution-affecting →
    /// forwarded to child ocx processes via [`Env::apply_ocx_config`] as
    /// [`keys::OCX_PATCH_SNAPSHOT`] so launchers resolve the same frozen
    /// companion digests. `None` when no snapshot is active.
    pub patch_snapshot: Option<PathBuf>,
}

impl OcxConfigView {
    pub fn new(self_exe: impl Into<PathBuf>) -> Self {
        Self {
            self_exe: self_exe.into(),
            offline: false,
            remote: false,
            frozen: false,
            config: None,
            project: None,
            global: false,
            index: None,
            mirrors: Vec::new(),
            patches: None,
            patch_snapshot: None,
        }
    }
}

#[cfg(target_os = "windows")]
pub const PATH_SEPARATOR: &str = ";";

#[cfg(not(target_os = "windows"))]
pub const PATH_SEPARATOR: &str = ":";

/// Case-normalizing wrapper for environment variable keys.
///
/// On Windows, environment variable names are case-insensitive, so we
/// normalize to uppercase using the native wide-char representation.
/// On other platforms, keys are left as-is.
///
/// Keys are stored as `OsString` to preserve non-UTF-8 environment
/// variable names (possible on Unix).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct EnvKey(OsString);

impl EnvKey {
    fn new(key: impl Into<OsString>) -> Self {
        let key = key.into();
        #[cfg(windows)]
        let key = {
            use std::os::windows::ffi::{OsStrExt, OsStringExt};
            let upper: Vec<u16> = key.encode_wide().map(|c| wide_to_upper(c)).collect();
            OsString::from_wide(&upper)
        };
        Self(key)
    }
}

/// Uppercase a single UTF-16 code unit in the ASCII range.
///
/// Full Unicode case-folding is not needed — Windows environment variable
/// names are conventionally ASCII. This matches the kernel behavior for
/// env var lookups (case-insensitive in the ASCII range).
#[cfg(windows)]
fn wide_to_upper(c: u16) -> u16 {
    if (b'a' as u16..=b'z' as u16).contains(&c) {
        c - 0x20
    } else {
        c
    }
}

#[derive(Clone)]
pub struct Env {
    vars: HashMap<EnvKey, OsString>,
}

impl Default for Env {
    fn default() -> Self {
        Self::new()
    }
}

impl Env {
    pub fn new() -> Self {
        Self {
            vars: std::env::vars_os().map(|(k, v)| (EnvKey::new(k), v)).collect(),
        }
    }

    pub fn clean() -> Self {
        Self { vars: HashMap::new() }
    }

    pub fn get(&self, key: &str) -> Option<&OsStr> {
        self.vars.get(&EnvKey::new(key)).map(|s| s.as_os_str())
    }

    pub fn set(&mut self, key: impl Into<OsString>, value: impl Into<OsString>) {
        self.vars.insert(EnvKey::new(key), value.into());
    }

    pub fn add_path(&mut self, key: impl Into<OsString>, value: impl Into<OsString>) {
        let key = EnvKey::new(key);
        let value = value.into();
        match self.vars.get(&key).filter(|v| !v.is_empty()) {
            Some(existing) => {
                let mut new_value = value;
                new_value.push(PATH_SEPARATOR);
                new_value.push(existing);
                self.vars.insert(key, new_value);
            }
            None => {
                self.vars.insert(key, value);
            }
        }
    }

    /// Borrowing iterator over `(key, value)` pairs.
    ///
    /// Lets a caller feed this env to `Command::envs` without consuming or
    /// cloning the whole map (`IntoIterator` is by-value). Order is
    /// unspecified (backed by a `HashMap`).
    pub fn iter(&self) -> impl Iterator<Item = (&OsStr, &OsStr)> {
        self.vars.iter().map(|(k, v)| (k.0.as_os_str(), v.as_os_str()))
    }

    /// Removes the named key from this environment. No-op if absent.
    pub fn remove(&mut self, key: &str) {
        self.vars.remove(&EnvKey::new(key));
    }

    /// Materializes resolution-affecting OCX configuration onto this env so a
    /// child ocx process sees the same policy the parent saw.
    ///
    /// Always sets [`keys::OCX_BINARY_PIN`]. Sets [`keys::OCX_OFFLINE`] /
    /// [`keys::OCX_REMOTE`] / [`keys::OCX_FROZEN`] / [`keys::OCX_GLOBAL`] only
    /// when the corresponding flag is true so the child env stays minimal. Sets
    /// [`keys::OCX_CONFIG`] /
    /// [`keys::OCX_INDEX`] only when the parent had an explicit value;
    /// otherwise removes any inherited setting so a stale parent-shell export
    /// cannot beat the outer ocx's parsed state.
    ///
    /// Does **not** propagate presentation flags (`--log-level`, `--format`,
    /// `--color`) — those are user-facing surface and must not leak into a
    /// launcher's child stream. Idempotent.
    pub fn apply_ocx_config(&mut self, cfg: &OcxConfigView) {
        self.set(keys::OCX_BINARY_PIN, cfg.self_exe.as_os_str());
        if cfg.offline {
            self.set(keys::OCX_OFFLINE, "1");
        } else {
            self.remove(keys::OCX_OFFLINE);
        }
        if cfg.remote {
            self.set(keys::OCX_REMOTE, "1");
        } else {
            self.remove(keys::OCX_REMOTE);
        }
        if cfg.frozen {
            self.set(keys::OCX_FROZEN, "1");
        } else {
            self.remove(keys::OCX_FROZEN);
        }
        if cfg.global {
            self.set(keys::OCX_GLOBAL, "1");
        } else {
            self.remove(keys::OCX_GLOBAL);
        }
        match &cfg.config {
            Some(path) => self.set(keys::OCX_CONFIG, path.as_os_str()),
            None => self.remove(keys::OCX_CONFIG),
        }
        match &cfg.project {
            Some(path) => self.set(keys::OCX_PROJECT, path.as_os_str()),
            None => self.remove(keys::OCX_PROJECT),
        }
        match &cfg.index {
            Some(path) => self.set(keys::OCX_INDEX, path.as_os_str()),
            None => self.remove(keys::OCX_INDEX),
        }
        match encode_mirrors(&cfg.mirrors) {
            Some(json) => self.set(keys::OCX_MIRRORS, json),
            None => self.remove(keys::OCX_MIRRORS),
        }
        match crate::config::patch::encode_patches(cfg.patches.as_ref()) {
            Some(json) => self.set(keys::OCX_PATCHES, json),
            None => self.remove(keys::OCX_PATCHES),
        }
        match &cfg.patch_snapshot {
            Some(path) => self.set(keys::OCX_PATCH_SNAPSHOT, path.as_os_str()),
            None => self.remove(keys::OCX_PATCH_SNAPSHOT),
        }
    }

    /// Applies resolved environment entries to this environment.
    ///
    /// This is the bridge from [`entry::Entry`](crate::package::metadata::env::entry::Entry)
    /// (the canonical resolved env var) to the process environment used by `exec`.
    pub fn apply_entries(&mut self, entries: &[crate::package::metadata::env::entry::Entry]) {
        use crate::package::metadata::env::modifier::ModifierKind;
        for entry in entries {
            match entry.kind {
                ModifierKind::Path => self.add_path(&entry.key, &entry.value),
                ModifierKind::Constant => self.set(&entry.key, &entry.value),
            }
        }
    }

    /// Resolve a command name to a full executable path using PATH
    /// (and PATHEXT on Windows).
    ///
    /// On Windows, `PATHEXT` is read from *this* environment rather than from
    /// the running process, so a clean child env still resolves the native
    /// `<name>.exe` launcher shim correctly before the child is spawned (the
    /// fallback default `.COM;.EXE;.BAT;.CMD` always advertises `.EXE`).
    ///
    /// Falls back to the bare command name if resolution fails, letting
    /// the OS handle it — `CreateProcessW` can still find `.exe` files.
    pub fn resolve_command(&self, command: impl AsRef<OsStr>) -> PathBuf {
        let command = command.as_ref();
        // cwd is only used by `which` when the command contains a path
        // separator (e.g. `./hello`).  For bare names it is ignored.
        let cwd = std::env::current_dir().unwrap_or_else(|e| {
            log::debug!("Could not determine current directory: {}", e);
            PathBuf::new()
        });

        #[cfg(windows)]
        {
            // On Windows, `which_in` internally reads PATHEXT from the real
            // process environment via `RealSys::env_windows_path_ext()`, not
            // from our child `Env`. We therefore probe the child's PATHEXT
            // ourselves so the native `<name>.exe` launcher shim is found
            // even when the running process has a different PATHEXT.
            if let Some(found) = self.resolve_command_windows(command, &cwd) {
                return found;
            }
        }
        #[cfg(not(windows))]
        if let Ok(path) = which::which_in(command, self.get("PATH"), cwd) {
            return path;
        }

        log::warn!(
            "Could not resolve '{}' via PATH, falling back to OS lookup.",
            command.to_string_lossy()
        );
        PathBuf::from(command)
    }

    /// Windows-only: resolve `command` by probing this env's PATH with each
    /// extension from this env's PATHEXT, in order.
    ///
    /// Returns `None` when the command cannot be found (caller logs + falls
    /// back to the bare name). Reads PATH and PATHEXT exclusively from this
    /// `Env`, not the running process — that is the entire point of this
    /// method vs. delegating to `which_in` which reads `std::env::var_os`.
    #[cfg(windows)]
    fn resolve_command_windows(&self, command: &OsStr, cwd: &std::path::Path) -> Option<PathBuf> {
        // Parse PATHEXT from our child env. Fall back to a sensible Windows
        // default when absent so bare `foo` can still resolve `foo.exe`.
        let pathext_str = self
            .get("PATHEXT")
            .and_then(|v| v.to_str())
            .unwrap_or(".COM;.EXE;.BAT;.CMD")
            .to_string();

        let mut extensions: Vec<&str> = pathext_str
            .split(';')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();

        // The OCX launcher shim is always `<name>.exe` post-`.cmd` cutover
        // (adr_windows_exe_shim.md). A hardened or customized child PATHEXT may
        // omit `.EXE` entirely (e.g. `PATHEXT=.BAT;.CMD`); the cutover removed
        // the PATHEXT inject/warn safety net on the premise that `.EXE` is
        // unconditionally resolvable, so guarantee it is probed here regardless
        // of the child PATHEXT. Probe order still respects the user's listed
        // extensions; `.exe` is appended only when absent (case-insensitive).
        if !extensions.iter().any(|ext| ext.eq_ignore_ascii_case(".EXE")) {
            extensions.push(".EXE");
        }

        // For each extension, ask `which_in` if `command + ext` is found.
        // `which_in` on a name-with-extension will not try to append further
        // extensions — it just probes the PATH directories for that exact name.
        let path = self.get("PATH");
        for ext in &extensions {
            let mut candidate = command.to_os_string();
            candidate.push(ext);
            if let Ok(found) = which::which_in(&candidate, path, cwd) {
                return Some(found);
            }
        }

        // Also try the bare name (covers fully-qualified paths and commands
        // that already carry an extension like `foo.exe`).
        which::which_in(command, path, cwd).ok()
    }
}

impl IntoIterator for Env {
    type Item = (OsString, OsString);
    type IntoIter = std::vec::IntoIter<(OsString, OsString)>;

    fn into_iter(self) -> Self::IntoIter {
        self.vars
            .into_iter()
            .map(|(k, v)| (k.0, v))
            .collect::<Vec<_>>()
            .into_iter()
    }
}

/// Process working directory.
///
/// Thin wrapper around [`std::env::current_dir`] so call sites route through
/// the OCX env layer instead of the std library directly. Keeps the
/// abstraction boundary consistent (and gives us a single seam for future
/// test injection without touching every consumer).
pub fn current_dir() -> std::io::Result<PathBuf> {
    std::env::current_dir()
}

pub fn var(key: impl AsRef<str>) -> Option<String> {
    #[cfg(test)]
    match crate::test::env::get_override(key.as_ref()) {
        Some(Some(val)) => return Some(val),
        Some(None) => return None,
        None => {}
    }
    match std::env::var(key.as_ref()) {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(os_str)) => {
            log::warn!("Environment variable '{}' is not valid: {:?}", key.as_ref(), os_str);
            None
        }
    }
}

pub fn flag(key: impl AsRef<str>, default: bool) -> bool {
    let key = key.as_ref();
    match var(key)
        .map(|value| utility::boolean_string::BooleanString::try_from(value.as_str()))
        .transpose()
    {
        Ok(Some(boolean)) => boolean.into(),
        Ok(None) => default,
        Err(error) => {
            log::warn!("Environment variable '{}' has invalid boolean value: {}", key, error);
            default
        }
    }
}

/// Returns `true` when running in a CI environment.
///
/// Checks the `CI` environment variable, which is set by GitHub Actions, GitLab CI,
/// CircleCI, Travis CI, Jenkins, and most other CI providers.
pub fn is_ci() -> bool {
    flag("CI", false)
}

pub fn string(key: impl AsRef<str>, default: String) -> String {
    if let Some(value) = var(key) {
        if value.is_empty() { default } else { value }
    } else {
        default
    }
}

/// Validates that `key` matches the POSIX environment-variable name grammar
/// (`[A-Za-z_][A-Za-z0-9_]*`).
///
/// Reject anything else. Without this gate, attacker-controlled package
/// metadata could inject syntax through the *key* slot of an emitted
/// assignment line that value-escaping does not protect against:
///
/// - shell emitters (`Shell::export_path` / `export_constant` / `unset`) — a
///   key like `KEY="; rm -rf $HOME; X` would break out of `export KEY=VAL`;
/// - CI exporters — a key containing a newline (`KEY\nINJECTED=evil`) would
///   inject a second variable into the GitHub `$GITHUB_ENV` file (CWE-77), and
///   non-identifier charsets corrupt the GitLab JSON-lines key field.
///
/// Lives in `crate::env` so every consumer that writes env-var keys — the
/// `shell` emitters and the `ci` flavors — validates through one source of
/// truth.
pub fn is_valid_env_key(key: &str) -> bool {
    if key.is_empty() {
        return false;
    }
    let mut bytes = key.bytes();
    let Some(first) = bytes.next() else { return false };
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return false;
    }
    bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Parses `OCX_INSECURE_REGISTRIES` into a list of registry hostnames.
pub fn insecure_registries() -> Vec<String> {
    string("OCX_INSECURE_REGISTRIES", String::new())
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Serializes a `(host, url)` mirror list into the single JSON object written
/// to [`keys::OCX_MIRRORS`].
///
/// Returns `None` for an empty list so [`Env::apply_ocx_config`] removes any
/// inherited value rather than setting an empty object.
fn encode_mirrors(mirrors: &[(String, String)]) -> Option<String> {
    if mirrors.is_empty() {
        return None;
    }
    let object: serde_json::Map<String, serde_json::Value> = mirrors
        .iter()
        .map(|(host, url)| (host.clone(), serde_json::Value::String(url.clone())))
        .collect();
    match serde_json::to_string(&serde_json::Value::Object(object)) {
        Ok(json) => Some(json),
        Err(e) => {
            log::warn!("failed to encode OCX_MIRRORS: {e}");
            None
        }
    }
}

/// Parses [`keys::OCX_MIRRORS`] (a JSON object of upstream-host → mirror url)
/// into a list of `(host, url)` pairs.
///
/// An absent or empty value yields an empty list. A present-but-broken value is
/// a hard error: silently degrading a forwarded `OCX_MIRRORS` to an identity map
/// would route reads to the firewall-blocked origin instead of the mirror — the
/// exact failure mode replace semantics exist to prevent.
///
/// # Errors
///
/// Returns [`MirrorConfigError::MalformedEnvJson`] when the value is not valid
/// JSON, and [`MirrorConfigError::NonStringEnvValue`] (naming the offending host)
/// when a per-host value is not a string.
///
/// [`MirrorConfigError::MalformedEnvJson`]: crate::config::mirror::MirrorConfigError::MalformedEnvJson
/// [`MirrorConfigError::NonStringEnvValue`]: crate::config::mirror::MirrorConfigError::NonStringEnvValue
pub fn mirrors() -> Result<Vec<(String, String)>, crate::config::mirror::MirrorConfigError> {
    use crate::config::mirror::MirrorConfigError;

    let Some(raw) = var(keys::OCX_MIRRORS) else {
        return Ok(Vec::new());
    };
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    let object = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&raw)
        .map_err(|source| MirrorConfigError::MalformedEnvJson { source })?;
    let mut pairs = Vec::with_capacity(object.len());
    for (host, value) in object {
        let url = value
            .as_str()
            .ok_or_else(|| MirrorConfigError::NonStringEnvValue { host: host.clone() })?;
        pairs.push((host, url.to_string()));
    }
    Ok(pairs)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `OCX_NO_MODIFY_PATH` is read through `flag` (the same `BooleanString`
    /// path as `--remote`/`--offline`), so both `=1` and `=true` set it true and
    /// an unset var is the `false` default (contract 8, item 7).
    #[test]
    fn ocx_no_modify_path_flag_is_truthy_for_one_and_true() {
        let env = crate::test::env::lock();

        env.set(keys::OCX_NO_MODIFY_PATH, "1");
        assert!(
            flag(keys::OCX_NO_MODIFY_PATH, false),
            "OCX_NO_MODIFY_PATH=1 must be true"
        );

        env.set(keys::OCX_NO_MODIFY_PATH, "true");
        assert!(
            flag(keys::OCX_NO_MODIFY_PATH, false),
            "OCX_NO_MODIFY_PATH=true must be true"
        );

        env.remove(keys::OCX_NO_MODIFY_PATH);
        assert!(
            !flag(keys::OCX_NO_MODIFY_PATH, false),
            "unset OCX_NO_MODIFY_PATH must fall back to the false default"
        );
    }

    #[test]
    fn env_key_case_insensitive() {
        // On Windows: keys are normalized to uppercase, so "Path" == "PATH" == "path".
        // On Unix: keys are case-sensitive, so each is distinct.
        let lower = EnvKey::new("path");
        let upper = EnvKey::new("PATH");
        let mixed = EnvKey::new("Path");

        #[cfg(windows)]
        {
            assert_eq!(lower, upper);
            assert_eq!(lower, mixed);
            assert_eq!(upper, mixed);
        }

        #[cfg(not(windows))]
        {
            assert_ne!(lower, upper);
            assert_ne!(lower, mixed);
            assert_ne!(upper, mixed);
        }
    }

    // ── is_valid_env_key (shared key validator) ──────────────────────────

    #[test]
    fn is_valid_env_key_accepts_identifiers() {
        assert!(is_valid_env_key("FOO"));
        assert!(is_valid_env_key("_x"));
        assert!(is_valid_env_key("A1"));
        assert!(is_valid_env_key("_OCX_INTERNAL"));
        assert!(is_valid_env_key("PATH"));
    }

    #[test]
    fn is_valid_env_key_rejects_empty() {
        assert!(!is_valid_env_key(""));
    }

    #[test]
    fn is_valid_env_key_rejects_leading_digit() {
        assert!(!is_valid_env_key("1A"));
    }

    #[test]
    fn is_valid_env_key_rejects_space() {
        assert!(!is_valid_env_key("A B"));
    }

    #[test]
    fn is_valid_env_key_rejects_newline() {
        // A newline in the key slot is the CI key-injection vector
        // (GitHub `$GITHUB_ENV` second-variable injection, CWE-77).
        assert!(!is_valid_env_key("A\nB"));
        assert!(!is_valid_env_key("A\rB"));
    }

    #[test]
    fn is_valid_env_key_rejects_equals() {
        // `=` would split the key/value framing of an assignment line.
        assert!(!is_valid_env_key("A=B"));
    }

    #[test]
    fn env_get_set_roundtrip() {
        let mut env = Env::clean();
        env.set("MY_VAR", "hello");
        assert_eq!(env.get("MY_VAR").unwrap(), "hello");
    }

    #[test]
    fn env_add_path_prepends() {
        let mut env = Env::clean();
        env.set("PATH", "/usr/bin");
        env.add_path("PATH", "/opt/bin");
        let path = env.get("PATH").unwrap().to_str().unwrap();
        assert!(path.starts_with("/opt/bin"));
        assert!(path.ends_with("/usr/bin"));
        assert!(path.contains(PATH_SEPARATOR));
    }

    #[test]
    fn env_add_path_to_empty() {
        let mut env = Env::clean();
        env.add_path("PATH", "/opt/bin");
        assert_eq!(env.get("PATH").unwrap(), "/opt/bin");
    }

    #[cfg(windows)]
    mod windows {
        use super::*;
        use std::os::windows::ffi::OsStringExt;

        #[test]
        fn env_key_ascii_uppercase_only() {
            // Non-ASCII characters are NOT uppercased — only a-z.
            // German ü (U+00FC) should stay as-is, not become Ü.
            let key = EnvKey::new("myVar_ü");
            let expected = EnvKey::new("MYVAR_ü");
            assert_eq!(key, expected);
        }

        #[test]
        fn env_key_preserves_unpaired_surrogates() {
            // Construct an OsString with an unpaired high surrogate (0xD800).
            // This is invalid Unicode but valid WTF-16, which Windows allows.
            let wide: Vec<u16> = vec![0xD800, b'a' as u16, b'b' as u16];
            let key_os = OsString::from_wide(&wide);

            let env_key = EnvKey::new(key_os);

            // The unpaired surrogate must survive; ASCII a/b become A/B.
            let expected_wide: Vec<u16> = vec![0xD800, b'A' as u16, b'B' as u16];
            let expected = EnvKey(OsString::from_wide(&expected_wide));
            assert_eq!(env_key, expected);
        }

        #[test]
        fn env_key_preserves_surrogate_pair() {
            // U+1F600 (😀) encoded as a surrogate pair: 0xD83D 0xDE00
            let wide: Vec<u16> = vec![b'h' as u16, 0xD83D, 0xDE00, b'i' as u16];
            let key_os = OsString::from_wide(&wide);

            let env_key = EnvKey::new(key_os);

            // Surrogate pair must survive intact; h→H, i→I.
            let expected_wide: Vec<u16> = vec![b'H' as u16, 0xD83D, 0xDE00, b'I' as u16];
            let expected = EnvKey(OsString::from_wide(&expected_wide));
            assert_eq!(env_key, expected);
        }

        #[test]
        fn env_key_bmp_non_ascii() {
            // CJK character U+4E16 (世) — single BMP code unit, not ASCII.
            let wide: Vec<u16> = vec![0x4E16, b'x' as u16];
            let key_os = OsString::from_wide(&wide);

            let env_key = EnvKey::new(key_os);

            // 世 stays as-is, x→X.
            let expected_wide: Vec<u16> = vec![0x4E16, b'X' as u16];
            let expected = EnvKey(OsString::from_wide(&expected_wide));
            assert_eq!(env_key, expected);
        }

        #[test]
        fn env_get_case_insensitive() {
            let mut env = Env::clean();
            env.set("Path", "C:\\Windows");
            assert_eq!(env.get("PATH").unwrap(), "C:\\Windows");
            assert_eq!(env.get("path").unwrap(), "C:\\Windows");
        }
    }

    #[test]
    fn apply_entries_constant() {
        use crate::package::metadata::env::{entry::Entry, modifier::ModifierKind};

        let mut env = Env::clean();
        let entries = vec![Entry {
            key: "JAVA_HOME".to_string(),
            value: "/opt/java".to_string(),
            kind: ModifierKind::Constant,
        }];
        env.apply_entries(&entries);
        assert_eq!(env.get("JAVA_HOME").unwrap(), "/opt/java");
    }

    #[test]
    fn apply_entries_path_prepends() {
        use crate::package::metadata::env::{entry::Entry, modifier::ModifierKind};

        let mut env = Env::clean();
        env.set("PATH", "/usr/bin");
        let entries = vec![Entry {
            key: "PATH".to_string(),
            value: "/opt/bin".to_string(),
            kind: ModifierKind::Path,
        }];
        env.apply_entries(&entries);
        let path = env.get("PATH").unwrap().to_str().unwrap();
        assert!(path.starts_with("/opt/bin"));
        assert!(path.ends_with("/usr/bin"));
    }

    #[test]
    fn apply_entries_mixed() {
        use crate::package::metadata::env::{entry::Entry, modifier::ModifierKind};

        let mut env = Env::clean();
        let entries = vec![
            Entry {
                key: "HOME".to_string(),
                value: "/home/user".to_string(),
                kind: ModifierKind::Constant,
            },
            Entry {
                key: "PATH".to_string(),
                value: "/opt/bin".to_string(),
                kind: ModifierKind::Path,
            },
        ];
        env.apply_entries(&entries);
        assert_eq!(env.get("HOME").unwrap(), "/home/user");
        assert_eq!(env.get("PATH").unwrap(), "/opt/bin");
    }

    // ── apply_ocx_config ─────────────────────────────────────────────────

    fn view(self_exe: &str) -> OcxConfigView {
        OcxConfigView::new(std::path::PathBuf::from(self_exe))
    }

    #[test]
    fn apply_ocx_config_sets_binary_and_skips_unset_flags() {
        let mut env = Env::clean();
        env.apply_ocx_config(&view("/abs/ocx"));
        assert_eq!(env.get(keys::OCX_BINARY_PIN).unwrap(), "/abs/ocx");
        assert!(env.get(keys::OCX_OFFLINE).is_none());
        assert!(env.get(keys::OCX_REMOTE).is_none());
        assert!(env.get(keys::OCX_CONFIG).is_none());
        assert!(env.get(keys::OCX_INDEX).is_none());
    }

    #[test]
    fn apply_ocx_config_writes_resolution_flags_when_set() {
        let mut cfg = view("/abs/ocx");
        cfg.offline = true;
        cfg.remote = true;
        cfg.config = Some("/cfg.toml".into());
        cfg.index = Some("/idx".into());

        let mut env = Env::clean();
        env.apply_ocx_config(&cfg);
        assert_eq!(env.get(keys::OCX_OFFLINE).unwrap(), "1");
        assert_eq!(env.get(keys::OCX_REMOTE).unwrap(), "1");
        assert_eq!(env.get(keys::OCX_CONFIG).unwrap(), "/cfg.toml");
        assert_eq!(env.get(keys::OCX_INDEX).unwrap(), "/idx");
    }

    #[test]
    fn apply_ocx_config_sets_ocx_frozen_when_set() {
        // `--frozen` is resolution-affecting, so `apply_ocx_config` MUST
        // forward it to a child ocx as `OCX_FROZEN=1` when set, and clear any
        // inherited value when unset so a stale parent-shell export cannot beat
        // the outer ocx's parsed state. Mirrors the OCX_OFFLINE/REMOTE/GLOBAL
        // contract.
        let mut cfg = view("/abs/ocx");
        cfg.frozen = true;
        let mut env = Env::clean();
        env.apply_ocx_config(&cfg);
        assert_eq!(
            env.get(keys::OCX_FROZEN).unwrap(),
            "1",
            "cfg.frozen=true must forward OCX_FROZEN=1 to the child env"
        );

        // Unset: a stale inherited OCX_FROZEN must be cleared.
        let mut env = Env::clean();
        env.set(keys::OCX_FROZEN, "1");
        env.apply_ocx_config(&view("/abs/ocx"));
        assert!(
            env.get(keys::OCX_FROZEN).is_none(),
            "cfg.frozen=false must clear any inherited OCX_FROZEN"
        );
    }

    #[test]
    fn apply_ocx_config_sets_ocx_global_when_set() {
        // W2-P3 (adr_global_toolchain_tier.md §Decision 2, C2.2): `--global`
        // is resolution-affecting, so `apply_ocx_config` MUST forward it to a
        // child ocx as `OCX_GLOBAL=1` when set, and remove any inherited
        // value when unset (so a stale parent-shell export cannot beat the
        // outer ocx's parsed state). This plumbing is REAL (not a stub) — the
        // guard PASSES now, pinning the contract against future regression.
        let mut cfg = view("/abs/ocx");
        cfg.global = true;
        let mut env = Env::clean();
        env.apply_ocx_config(&cfg);
        assert_eq!(
            env.get(keys::OCX_GLOBAL).unwrap(),
            "1",
            "cfg.global=true must forward OCX_GLOBAL=1 to the child env"
        );
        // OCX_GLOBAL travels with the resolution-affecting set, never the
        // presentation set (which is never forwarded — it would leak into
        // entrypoint child streams).
        for presentation in ["OCX_LOG", "OCX_LOG_CONSOLE", "OCX_FORMAT", "OCX_COLOR"] {
            assert!(
                env.get(presentation).is_none(),
                "presentation key `{presentation}` must never ride along with OCX_GLOBAL"
            );
        }

        // Unset: a stale inherited OCX_GLOBAL must be cleared so the outer
        // ocx's parsed state (global=false) wins.
        let mut env = Env::clean();
        env.set(keys::OCX_GLOBAL, "1");
        env.apply_ocx_config(&view("/abs/ocx"));
        assert!(
            env.get(keys::OCX_GLOBAL).is_none(),
            "cfg.global=false must clear any inherited OCX_GLOBAL"
        );
    }

    #[test]
    fn apply_ocx_config_overwrites_inherited_stale_values() {
        // Outer ocx parses with offline=false, remote=false; inherited env
        // carries stale OCX_OFFLINE=1 / OCX_REMOTE=1 from a prior shell
        // export. The outer's parsed state must win — child ocx must NOT see
        // the stale flags.
        let mut env = Env::clean();
        env.set(keys::OCX_OFFLINE, "1");
        env.set(keys::OCX_REMOTE, "1");
        env.set(keys::OCX_CONFIG, "/stale.toml");
        env.set(keys::OCX_INDEX, "/stale-idx");

        env.apply_ocx_config(&view("/abs/ocx"));
        assert!(
            env.get(keys::OCX_OFFLINE).is_none(),
            "stale OCX_OFFLINE must be cleared"
        );
        assert!(env.get(keys::OCX_REMOTE).is_none(), "stale OCX_REMOTE must be cleared");
        assert!(env.get(keys::OCX_CONFIG).is_none(), "stale OCX_CONFIG must be cleared");
        assert!(env.get(keys::OCX_INDEX).is_none(), "stale OCX_INDEX must be cleared");
    }

    #[test]
    fn apply_ocx_config_never_sets_presentation_keys() {
        // Presentation flags (--log-level, --format, --color) must not
        // propagate via env — they would leak into a launcher's child stream.
        // The view does not even carry them, but assert the corresponding
        // canonical keys are absent regardless. `OCX_LOG` and `OCX_LOG_CONSOLE`
        // are the real env vars consumed by `LogSettings::build_env_filter`;
        // `OCX_FORMAT` / `OCX_COLOR` are the canonical names that would bind
        // to `--format` / `--color` if those ever gained env counterparts.
        let mut env = Env::clean();
        env.apply_ocx_config(&view("/abs/ocx"));
        for forbidden in ["OCX_LOG", "OCX_LOG_CONSOLE", "OCX_FORMAT", "OCX_COLOR"] {
            assert!(
                env.get(forbidden).is_none(),
                "presentation key `{forbidden}` must never be set by apply_ocx_config",
            );
        }
    }

    #[test]
    fn clean_env_carries_no_ocx_keys() {
        // `Env::clean()` returns an empty map; nothing inherited from the
        // running process. Authoritative source for OCX_* on a child env is
        // `apply_ocx_config`, not the parent shell.
        let env = Env::clean();
        for key in [
            keys::OCX_BINARY_PIN,
            keys::OCX_OFFLINE,
            keys::OCX_REMOTE,
            keys::OCX_CONFIG,
            keys::OCX_INDEX,
        ] {
            assert!(env.get(key).is_none(), "Env::clean must not contain `{key}`");
        }
    }

    // ── resolve_command ──────────────────────────────────────────────────

    // ── Step 3.1 specification tests: OCX_MIRRORS round-trip ──────────────────

    /// `mirrors()` parses what `apply_ocx_config`/`encode_mirrors` emits —
    /// a basic round-trip for the simplest case.
    ///
    /// Traces: plan Testing Strategy — "`OCX_MIRRORS` JSON round-trip
    /// (mirrors() parses what apply_ocx_config/encode_mirrors emits)"; ADR
    /// review A3.
    #[test]
    fn ocx_mirrors_json_roundtrip_basic() {
        let env = crate::test::env::lock();
        let input = vec![("ghcr.io".to_string(), "https://corp.jfrog.io/ghcr-remote".to_string())];
        // encode_mirrors is private; drive it through apply_ocx_config so we
        // test the public contract (encode → set in env → mirrors() parses).
        let mut cfg = OcxConfigView::new(std::path::PathBuf::from("/abs/ocx"));
        cfg.mirrors = input.clone();
        let mut child_env = Env::clean();
        child_env.apply_ocx_config(&cfg);

        // Retrieve the encoded value and inject it via the test env override
        // so mirrors() reads it.
        let encoded = child_env
            .get(keys::OCX_MIRRORS)
            .expect("OCX_MIRRORS must be set when mirrors is non-empty")
            .to_str()
            .expect("OCX_MIRRORS must be valid UTF-8")
            .to_string();
        env.set(keys::OCX_MIRRORS, encoded);

        let parsed = mirrors().expect("well-formed OCX_MIRRORS must parse");
        assert_eq!(parsed.len(), 1, "parsed mirrors must have one entry");
        assert_eq!(parsed[0].0, "ghcr.io");
        assert_eq!(parsed[0].1, "https://corp.jfrog.io/ghcr-remote");
    }

    /// `mirrors()` round-trip including a `localhost:5000` host key and a url
    /// with a query string — specifically tests the delimiter-safety guarantee.
    ///
    /// Traces: plan Testing Strategy — "including a `localhost:5000` host key
    /// and a url with a query string"; ADR review A3 (JSON not comma/`=`).
    #[test]
    fn ocx_mirrors_json_roundtrip_localhost_and_query_string() {
        let env = crate::test::env::lock();
        let input = vec![
            (
                "localhost:5000".to_string(),
                "https://corp.mirror.io/proxy?region=eu".to_string(),
            ),
            ("ghcr.io".to_string(), "https://corp.jfrog.io/ghcr-remote".to_string()),
        ];
        let mut cfg = OcxConfigView::new(std::path::PathBuf::from("/abs/ocx"));
        cfg.mirrors = input.clone();
        let mut child_env = Env::clean();
        child_env.apply_ocx_config(&cfg);

        let encoded = child_env
            .get(keys::OCX_MIRRORS)
            .expect("OCX_MIRRORS must be set when mirrors is non-empty")
            .to_str()
            .expect("OCX_MIRRORS must be valid UTF-8")
            .to_string();
        env.set(keys::OCX_MIRRORS, encoded);

        let mut parsed = mirrors().expect("well-formed OCX_MIRRORS must parse");
        // Sort for deterministic comparison (HashMap iteration order may differ).
        parsed.sort_by(|a, b| a.0.cmp(&b.0));

        assert_eq!(parsed.len(), 2, "parsed mirrors must have two entries");

        // Find each entry regardless of order.
        let localhost = parsed.iter().find(|(h, _)| h == "localhost:5000");
        let ghcr = parsed.iter().find(|(h, _)| h == "ghcr.io");

        assert!(localhost.is_some(), "localhost:5000 entry must survive round-trip");
        assert_eq!(
            localhost.unwrap().1,
            "https://corp.mirror.io/proxy?region=eu",
            "url with query string must survive verbatim"
        );
        assert!(ghcr.is_some(), "ghcr.io entry must survive round-trip");
    }

    /// Empty mirrors list → `encode_mirrors` returns `None` → `apply_ocx_config`
    /// removes any stale `OCX_MIRRORS` value.
    ///
    /// Traces: ADR — empty list means no mirror configured, must remove the key.
    #[test]
    fn empty_mirrors_removes_ocx_mirrors_key() {
        let mut cfg = OcxConfigView::new(std::path::PathBuf::from("/abs/ocx"));
        cfg.mirrors = vec![];

        let mut env = Env::clean();
        // Pre-set a stale value to confirm it is removed.
        env.set(keys::OCX_MIRRORS, r#"{"ghcr.io":"https://old.corp/remote"}"#);
        env.apply_ocx_config(&cfg);

        assert!(
            env.get(keys::OCX_MIRRORS).is_none(),
            "empty mirrors must remove OCX_MIRRORS from the child env"
        );
    }

    /// Malformed JSON in `OCX_MIRRORS` → `mirrors()` is a HARD error.
    ///
    /// Silently degrading a forwarded mirror map to an identity map would route
    /// reads to the firewall-blocked origin instead of the mirror — the exact
    /// anti-goal replace semantics exist to prevent. So a present-but-broken
    /// value must abort, not warn-and-empty.
    ///
    /// Traces: review Cluster-1 fail-loud — malformed forwarded `OCX_MIRRORS`
    /// is a hard error on both parent and child paths.
    #[test]
    fn malformed_ocx_mirrors_is_hard_error() {
        use crate::config::mirror::MirrorConfigError;

        let env = crate::test::env::lock();
        env.set(keys::OCX_MIRRORS, "this is not valid json {{{");

        let result = mirrors();
        assert!(
            matches!(result, Err(MirrorConfigError::MalformedEnvJson { .. })),
            "malformed OCX_MIRRORS must yield MalformedEnvJson, got: {result:?}"
        );
    }

    /// A non-string per-host value in `OCX_MIRRORS` → `mirrors()` is a HARD error
    /// naming the offending host. A silent `filter_map` drop would degrade the
    /// map for that host, so the entry must abort instead.
    ///
    /// Traces: review Cluster-1 fail-loud — non-string env value names the host.
    #[test]
    fn non_string_ocx_mirrors_value_is_hard_error() {
        use crate::config::mirror::MirrorConfigError;

        let env = crate::test::env::lock();
        // ghcr.io maps to a number, not a string url.
        env.set(keys::OCX_MIRRORS, r#"{"ghcr.io":42}"#);

        let result = mirrors();
        assert!(
            matches!(result, Err(MirrorConfigError::NonStringEnvValue { ref host }) if host == "ghcr.io"),
            "non-string OCX_MIRRORS value must yield NonStringEnvValue naming ghcr.io, got: {result:?}"
        );
    }

    // ── OCX_PATCHES forwarding via apply_ocx_config ──────────────────────────

    /// `apply_ocx_config` with `patches = None` does NOT set `OCX_PATCHES` and
    /// removes any stale inherited value.
    ///
    /// Traces: Phase 1 — "No `[patches]` -> apply_ocx_config does NOT set
    /// OCX_PATCHES (no-op / byte-identical env)"; stub manifest — "forwarded
    /// ONLY when present (mirror OCX_MIRRORS exactly)".
    #[test]
    fn apply_ocx_config_does_not_set_ocx_patches_when_patches_none() {
        let cfg = view("/abs/ocx");
        // patches is None by default in OcxConfigView::new()
        assert!(cfg.patches.is_none());

        let mut env = Env::clean();
        // Pre-set a stale value to confirm it is removed.
        env.set(
            keys::OCX_PATCHES,
            r#"{"registry":"stale","path_template":"x","required":true}"#,
        );
        env.apply_ocx_config(&cfg);

        assert!(
            env.get(keys::OCX_PATCHES).is_none(),
            "patches=None must remove any stale OCX_PATCHES from the child env"
        );
    }

    /// `apply_ocx_config` with `patches = Some(resolved)` sets `OCX_PATCHES` to
    /// a non-empty JSON string.
    ///
    /// Traces: Phase 1 — "OCX_PATCHES round-trip: an OcxConfigView carrying
    /// resolved patches -> apply_ocx_config sets OCX_PATCHES to JSON".
    #[test]
    fn apply_ocx_config_sets_ocx_patches_when_patches_some() {
        let mut cfg = view("/abs/ocx");
        cfg.patches = Some(crate::config::patch::ResolvedPatchConfig {
            system_required: false,
            registry: "corp.example.com/patches".to_string(),
            path_template: "{registry}/{repository}".to_string(),
            required: true,
        });

        let mut env = Env::clean();
        env.apply_ocx_config(&cfg);

        let raw = env
            .get(keys::OCX_PATCHES)
            .expect("OCX_PATCHES must be set when patches is Some")
            .to_str()
            .expect("OCX_PATCHES must be valid UTF-8");
        // Must be parseable JSON containing the registry.
        assert!(
            raw.contains("corp.example.com/patches"),
            "OCX_PATCHES JSON must contain the registry; got: {raw}"
        );
    }

    /// OCX_PATCHES full round-trip through `apply_ocx_config` then
    /// `patches_from_env`: child env carries the same `ResolvedPatchConfig`.
    ///
    /// Traces: Phase 1 — "OCX_PATCHES round-trip: an OcxConfigView carrying
    /// resolved patches -> apply_ocx_config sets OCX_PATCHES to JSON -> parsing
    /// OCX_PATCHES back yields the same resolved patches"; block-tier requirement
    /// C5.
    #[test]
    fn apply_ocx_config_ocx_patches_round_trip_via_patches_from_env() {
        use crate::config::patch::patches_from_env;

        let env_guard = crate::test::env::lock();

        let original = crate::config::patch::ResolvedPatchConfig {
            system_required: false,
            registry: "internal.company.com/ocx-patches".to_string(),
            path_template: "{registry}/{repository}".to_string(),
            required: true,
        };

        let mut cfg = OcxConfigView::new(std::path::PathBuf::from("/abs/ocx"));
        cfg.patches = Some(original.clone());

        let mut child_env = Env::clean();
        child_env.apply_ocx_config(&cfg);

        // Inject the encoded OCX_PATCHES from child_env into the test env override
        // so patches_from_env() can read it.
        let encoded = child_env
            .get(keys::OCX_PATCHES)
            .expect("OCX_PATCHES must be set when patches is Some")
            .to_str()
            .expect("OCX_PATCHES must be valid UTF-8")
            .to_string();
        env_guard.set(keys::OCX_PATCHES, encoded);

        let parsed = patches_from_env()
            .expect("well-formed OCX_PATCHES must parse")
            .expect("non-empty OCX_PATCHES must yield Some(resolved)");

        assert_eq!(
            parsed, original,
            "patches_from_env must recover the same ResolvedPatchConfig forwarded by apply_ocx_config"
        );
    }

    // ── OCX_PATCH_SNAPSHOT forwarding via apply_ocx_config ───────────────────

    /// `apply_ocx_config` with `patch_snapshot = Some(path)` must set
    /// `OCX_PATCH_SNAPSHOT` to that path on the child env.
    ///
    /// Traceability: Phase 5B spec test 3 — OCX_PATCH_SNAPSHOT forwarded when Some.
    ///
    /// NOTE: This test PASSES against the current code because `apply_ocx_config`
    /// already forwards `patch_snapshot` (the implementation is already in place).
    /// The test is included here as a pinning guard to prevent regression and to
    /// satisfy the spec test contract.
    #[test]
    fn apply_ocx_config_sets_ocx_patch_snapshot_when_some() {
        let mut cfg = view("/abs/ocx");
        cfg.patch_snapshot = Some(std::path::PathBuf::from("/project/patches.snapshot.json"));

        let mut env = Env::clean();
        env.apply_ocx_config(&cfg);

        let value = env
            .get(keys::OCX_PATCH_SNAPSHOT)
            .expect("OCX_PATCH_SNAPSHOT must be set when patch_snapshot is Some");
        assert_eq!(
            value.to_str().unwrap(),
            "/project/patches.snapshot.json",
            "OCX_PATCH_SNAPSHOT must equal the configured path"
        );
    }

    /// `apply_ocx_config` with `patch_snapshot = None` must remove any stale
    /// `OCX_PATCH_SNAPSHOT` value from the child env, so the outer ocx's state
    /// (no snapshot) wins over any inherited value.
    ///
    /// Traceability: Phase 5B spec test 3 — OCX_PATCH_SNAPSHOT removed when None.
    ///
    /// NOTE: This test PASSES against the current code. Included as a regression guard.
    #[test]
    fn apply_ocx_config_removes_ocx_patch_snapshot_when_none() {
        // Build a view with patch_snapshot = None (the default).
        let cfg = view("/abs/ocx");
        assert!(cfg.patch_snapshot.is_none());

        let mut env = Env::clean();
        // Pre-set a stale value to confirm it is cleared.
        env.set(keys::OCX_PATCH_SNAPSHOT, "/stale/patches.snapshot.json");
        env.apply_ocx_config(&cfg);

        assert!(
            env.get(keys::OCX_PATCH_SNAPSHOT).is_none(),
            "patch_snapshot=None must remove any stale OCX_PATCH_SNAPSHOT from the child env"
        );
    }

    /// `resolve_command` must find a well-known binary that exists on PATH.
    #[cfg(unix)]
    #[test]
    fn resolve_command_finds_sh_on_unix() {
        let env = Env::new();
        let resolved = env.resolve_command("sh");
        // On any Unix system `sh` must exist somewhere on PATH.
        assert!(
            resolved.exists(),
            "resolve_command(\"sh\") must find a real path; got {}",
            resolved.display()
        );
    }

    /// When the command is not on PATH, `resolve_command` returns the bare
    /// name unchanged (OS fallback path).
    #[test]
    fn resolve_command_falls_back_to_bare_name_when_not_found() {
        let mut env = Env::clean();
        // Empty PATH — nothing can be found.
        env.set("PATH", "");
        #[cfg(windows)]
        env.set("PATHEXT", ".EXE;.CMD");
        let resolved = env.resolve_command("__ocx_definitely_missing_binary__");
        assert_eq!(
            resolved,
            std::path::PathBuf::from("__ocx_definitely_missing_binary__"),
            "missing binary must fall back to bare name"
        );
    }

    /// On Windows, `resolve_command_windows` must consult PATHEXT from this
    /// env, not from the running process. We simulate the scenario by placing
    /// a `.exe`-named file (the native launcher shim's on-disk shape) in a
    /// temp directory and pointing PATH at it with a PATHEXT that includes
    /// `.EXE`.
    #[cfg(windows)]
    #[test]
    fn resolve_command_windows_uses_child_env_pathext() {
        use std::fs;

        let dir = tempfile::tempdir().unwrap();
        let launcher = dir.path().join("my_tool.exe");
        fs::write(&launcher, b"MZ").unwrap();

        let mut env = Env::clean();
        env.set("PATH", dir.path().to_str().unwrap());
        // PATHEXT lists .EXE — must resolve `my_tool` → `my_tool.exe`.
        env.set("PATHEXT", ".EXE;.CMD");

        let resolved = env.resolve_command("my_tool");
        assert_eq!(
            resolved.file_name().unwrap().to_str().unwrap().to_ascii_lowercase(),
            "my_tool.exe",
            "resolve_command must find my_tool.exe via child env PATHEXT"
        );
    }

    /// Regression: a hardened or customized child PATHEXT may omit `.EXE`
    /// entirely (e.g. `PATHEXT=.BAT;.CMD`). The native launcher shim is always
    /// `<name>.exe`, so `resolve_command_windows` must still probe `.exe` even
    /// when the child PATHEXT does not list it — otherwise an installed `.exe`
    /// entrypoint silently becomes "command not found" (the cutover removed the
    /// PATHEXT inject/warn net that previously masked this).
    #[cfg(windows)]
    #[test]
    fn resolve_command_windows_probes_exe_when_pathext_omits_it() {
        use std::fs;

        let dir = tempfile::tempdir().unwrap();
        let launcher = dir.path().join("my_tool.exe");
        fs::write(&launcher, b"MZ").unwrap();

        let mut env = Env::clean();
        env.set("PATH", dir.path().to_str().unwrap());
        // PATHEXT deliberately omits .EXE — `my_tool` must still resolve to
        // `my_tool.exe` via the always-probed `.exe` fallback.
        env.set("PATHEXT", ".BAT;.CMD");

        let resolved = env.resolve_command("my_tool");
        assert_eq!(
            resolved.file_name().unwrap().to_str().unwrap().to_ascii_lowercase(),
            "my_tool.exe",
            "resolve_command must probe .exe even when child PATHEXT omits it"
        );
    }
}
