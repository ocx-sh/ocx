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
    /// Path to the local index directory. Mirrors `--index`.
    pub const OCX_INDEX: &str = "OCX_INDEX";
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
    pub config: Option<PathBuf>,
    pub project: Option<PathBuf>,
    pub index: Option<PathBuf>,
}

impl OcxConfigView {
    pub fn new(self_exe: impl Into<PathBuf>) -> Self {
        Self {
            self_exe: self_exe.into(),
            offline: false,
            remote: false,
            config: None,
            project: None,
            index: None,
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

    /// Removes the named key from this environment. No-op if absent.
    pub fn remove(&mut self, key: &str) {
        self.vars.remove(&EnvKey::new(key));
    }

    /// Materializes resolution-affecting OCX configuration onto this env so a
    /// child ocx process sees the same policy the parent saw.
    ///
    /// Always sets [`keys::OCX_BINARY_PIN`]. Sets [`keys::OCX_OFFLINE`] /
    /// [`keys::OCX_REMOTE`] only when the corresponding flag is true so the
    /// child env stays minimal. Sets [`keys::OCX_CONFIG`] /
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
    /// the running process, so a clean child env that has had
    /// [`super::package_manager::launcher::emplace_pathext`] applied still
    /// resolves `.cmd` launchers correctly before the child is spawned.
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
            // ourselves so `.cmd` launchers emplaced by `emplace_pathext` are
            // found even when the running process has a different PATHEXT.
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
        use std::path::PathBuf;

        // Parse PATHEXT from our child env. Fall back to a sensible Windows
        // default when absent so bare `foo` can still resolve `foo.exe`.
        let pathext_str = self
            .get("PATHEXT")
            .and_then(|v| v.to_str())
            .unwrap_or(".COM;.EXE;.BAT;.CMD")
            .to_string();

        let extensions: Vec<&str> = pathext_str
            .split(';')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();

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

/// Parses `OCX_INSECURE_REGISTRIES` into a list of registry hostnames.
pub fn insecure_registries() -> Vec<String> {
    string("OCX_INSECURE_REGISTRIES", String::new())
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
    /// env, not from the running process. We simulate the scenario by placing a
    /// `.cmd`-named file in a temp directory and pointing PATH at it with a
    /// PATHEXT that includes `.CMD`.
    #[cfg(windows)]
    #[test]
    fn resolve_command_windows_uses_child_env_pathext() {
        use std::fs;

        let dir = tempfile::tempdir().unwrap();
        let launcher = dir.path().join("my_tool.cmd");
        fs::write(&launcher, "@echo off\r\necho hello\r\n").unwrap();

        let mut env = Env::clean();
        env.set("PATH", dir.path().to_str().unwrap());
        // PATHEXT lists .CMD — must resolve `my_tool` → `my_tool.cmd`.
        env.set("PATHEXT", ".EXE;.CMD");

        let resolved = env.resolve_command("my_tool");
        assert_eq!(
            resolved.file_name().unwrap().to_str().unwrap().to_ascii_lowercase(),
            "my_tool.cmd",
            "resolve_command must find my_tool.cmd via child env PATHEXT"
        );
    }
}
