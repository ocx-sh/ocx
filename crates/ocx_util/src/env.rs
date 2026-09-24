// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The process-environment accessor — the one seam every crate reads the
//! environment through.
//!
//! Domain-free by construction (`env_accessor_is_domain_free`): it names
//! nothing outside `utility`, so the crate split can lift it into `ocx_util`
//! without dragging `config`, `package` or `oci` behind it. The bag of
//! `OCX_*`-specific helpers (`keys`, `Env`, `OcxConfigView`, the composed
//! child environment) stays in `crate::env`, which reads *through* this
//! module like everyone else.

use std::path::PathBuf;

/// The test-only environment override table.
///
/// Gated `#[cfg(any(test, feature = "__testing"))]` rather than plain
/// `#[cfg(test)]`: `cfg(test)` does not cross a crate boundary, and once this
/// module is `ocx_util::env` the tests that write the table live in other
/// crates. Those crates list `ocx_util = { workspace = true, features =
/// ["__testing"] }` under `[dev-dependencies]`, which resolver v3 keeps out of
/// the normal build — so the table costs a release build nothing, and a
/// production `use` of it does not compile. That compiler refusal is the
/// tripwire; there is no scanner (plan DEC-16, D-065 withdrawn).
#[cfg(any(test, feature = "__testing"))]
pub mod overrides {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{LazyLock, Mutex, MutexGuard};

    use tempfile::TempDir;

    static TEST_LOCK: Mutex<()> = Mutex::new(());
    static OVERRIDES: LazyLock<Mutex<HashMap<String, Option<String>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
    /// Set by [`EnvLock::hermetic`]: a key with no override reads as absent
    /// instead of falling through to the process environment.
    static HERMETIC: AtomicBool = AtomicBool::new(false);
    /// The working directory [`super::current_dir`] answers while hermetic.
    static CWD: Mutex<Option<PathBuf>> = Mutex::new(None);

    /// Locks [`OVERRIDES`], recovering from a poisoned mutex.
    ///
    /// A test that panics while holding this lock (e.g. a Specify-phase test
    /// exercising an `unimplemented!()`/`todo!()` stub) must not poison the map
    /// for every other env-touching test still to run in the same process —
    /// overrides are always fully cleared on both acquire and drop, so the
    /// poisoned data itself is never load-bearing.
    fn overrides() -> std::sync::MutexGuard<'static, HashMap<String, Option<String>>> {
        OVERRIDES.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Called by [`super::var`] under the same cfg as this module.
    ///
    /// Returns:
    /// - `Some(Some(v))` — key is overridden with value `v`
    /// - `Some(None)` — key is explicitly removed (treat as not present)
    /// - `None` — key has no override (fall through to `std::env::var`)
    pub(super) fn get(key: &str) -> Option<Option<String>> {
        match overrides().get(key).cloned() {
            None if is_hermetic() => Some(None),
            value => value,
        }
    }

    /// Whether an [`EnvLock::hermetic`] guard is live — every environment
    /// read then answers from the override table alone.
    ///
    /// `pub` for the one reader outside this module that cannot route through
    /// [`super::var`]: a third-party resolver (`dirs::config_dir`) that reads
    /// the process environment itself, and whose caller must answer from the
    /// table instead while this is `true`.
    pub fn is_hermetic() -> bool {
        HERMETIC.load(Ordering::SeqCst)
    }

    /// The directory [`super::current_dir`] reports while hermetic.
    pub(super) fn cwd() -> Option<PathBuf> {
        CWD.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    fn reset() {
        overrides().clear();
        HERMETIC.store(false, Ordering::SeqCst);
        *CWD.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    /// A guard that serialises environment-touching tests and provides safe
    /// injection of environment variable overrides without calling
    /// `std::env::set_var` / `std::env::remove_var`.
    ///
    /// Acquire with [`lock()`].  Use [`EnvLock::set`] and [`EnvLock::remove`] to
    /// inject values; they are visible to any code that reads env vars through
    /// [`super::var`].  All overrides are cleared when the guard is dropped.
    pub struct EnvLock {
        _guard: MutexGuard<'static, ()>,
    }

    impl EnvLock {
        fn acquire() -> Self {
            // Recover from a poisoned mutex: a prior test that panicked while
            // holding this lock (e.g. a Specify-phase test exercising an
            // `unimplemented!()`/`todo!()` stub) must not cascade into every
            // subsequent env-touching test run concurrently in the same process.
            // The guarded state (`OVERRIDES`) is unconditionally cleared right
            // below, so recovering the poisoned data is safe.
            let guard = TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            // Clear any stale overrides left by a previously panicked test.
            reset();
            Self { _guard: guard }
        }

        /// Injects `value` for `key`.  Visible to [`super::var`].
        pub fn set(&self, key: impl Into<String>, value: impl Into<String>) {
            overrides().insert(key.into(), Some(value.into()));
        }

        /// Marks `key` as removed.  [`super::var`] will return `None` for it.
        pub fn remove(&self, key: impl Into<String>) {
            overrides().insert(key.into(), None);
        }

        /// Makes the table the **whole** environment until this guard drops:
        /// a key not [`set`](Self::set) reads as absent rather than falling
        /// through to the process environment, [`super::current_dir`]
        /// answers `cwd`, and [`super::home_dir`] answers from `HOME`
        /// (`USERPROFILE` on Windows) in the table.
        ///
        /// The in-process CLI seam's no-ambient-read guarantee. Readers that
        /// bypass [`super::var`] are not covered — see [`is_hermetic`].
        pub fn hermetic(&self, cwd: impl Into<PathBuf>) {
            *CWD.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(cwd.into());
            HERMETIC.store(true, Ordering::SeqCst);
        }

        /// Points `OCX_HOME` at a fresh empty directory and returns its
        /// [`TempDir`] guard (bind it for the test's lifetime).
        ///
        /// `ConfigLoader::project_path` Tier 4 falls back to
        /// `$OCX_HOME/ocx.toml` (default `~/.ocx/ocx.toml`). Tests that assert
        /// "no project source resolves" must sandbox that tier or they pick up
        /// the developer's real `~/.ocx/ocx.toml` and fail only on machines
        /// that happen to have one (green on clean CI, red locally).
        pub fn isolate_project_home(&self) -> TempDir {
            let home = TempDir::new().expect("create temp OCX_HOME");
            self.set("OCX_HOME", home.path().to_str().expect("temp path is utf-8"));
            home
        }
    }

    impl Drop for EnvLock {
        fn drop(&mut self) {
            reset();
        }
    }

    /// Acquires the environment lock for the current test.
    ///
    /// Holds a process-wide mutex that prevents other env-touching tests from
    /// running concurrently.  All overrides injected via the returned [`EnvLock`]
    /// are automatically cleared on drop.
    pub fn lock() -> EnvLock {
        EnvLock::acquire()
    }
}

#[cfg(target_os = "windows")]
pub const PATH_SEPARATOR: &str = ";";

#[cfg(not(target_os = "windows"))]
pub const PATH_SEPARATOR: &str = ":";

/// Process working directory.
///
/// Thin wrapper around [`std::env::current_dir`] so call sites route through
/// the OCX env layer instead of the std library directly. Keeps the
/// abstraction boundary consistent (and gives us a single seam for future
/// test injection without touching every consumer).
pub fn current_dir() -> std::io::Result<PathBuf> {
    #[cfg(any(test, feature = "__testing"))]
    if let Some(cwd) = overrides::cwd() {
        return Ok(cwd);
    }
    std::env::current_dir()
}

/// The current user's home directory.
///
/// One resolver, because two of them disagreed. `std::env::home_dir` (stable
/// since 1.85), never `dirs::home_dir`: on Windows the former reads
/// `%USERPROFILE%` first and only asks the OS for the registered profile path
/// when that is unset, while `dirs` calls `SHGetKnownFolderPath`
/// unconditionally — so a sandbox, container or CI runner that overrides
/// `%USERPROFILE%` handed one `ocx` invocation two different home directories.
/// They also disagree on Unix over an empty `pw_dir` (#381).
///
/// `setup::home_env_from_environment` deliberately stays on its own resolver.
/// It answers a different question — the *login shell's* home, for writing
/// `.bashrc`/`.zshrc` — and reads `$HOME` first for exactly that reason.
pub fn home_dir() -> Option<PathBuf> {
    #[cfg(any(test, feature = "__testing"))]
    if overrides::is_hermetic() {
        let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        return var(key).filter(|home| !home.is_empty()).map(PathBuf::from);
    }
    std::env::home_dir()
}

pub fn var(key: impl AsRef<str>) -> Option<String> {
    #[cfg(any(test, feature = "__testing"))]
    match overrides::get(key.as_ref()) {
        Some(Some(val)) => return Some(val),
        Some(None) => return None,
        None => {}
    }
    match std::env::var(key.as_ref()) {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => None,
        // The key alone, never the value: this reader is on the credential
        // path (`CI_JOB_TOKEN`, `OCX_ANNOUNCE_GIT_TOKEN`), and a CI job log is
        // durable and read by more parties than the process environment.
        Err(std::env::VarError::NotUnicode(_)) => {
            log::warn!("Environment variable '{}' is not valid UTF-8", key.as_ref());
            None
        }
    }
}

pub fn flag(key: impl AsRef<str>, default: bool) -> bool {
    let key = key.as_ref();
    match var(key)
        .map(|value| super::boolean_string::BooleanString::try_from(value.as_str()))
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
/// Lives here, in the one env accessor, so every consumer that writes env-var
/// keys — the `shell` emitters and the `ci` flavors — validates through one
/// source of truth.
///
/// This is also what the interpolation scanner validates the `KEY` segment of
/// `${self.env.KEY}` against (`package::metadata::template::scanner::scan`),
/// so a change here changes what the token grammar accepts: widening this
/// predicate admits new `${self.env.*}` tokens, and narrowing it turns tokens
/// published packages already carry into refusals.
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

/// Returns `true` when `key` falls in the `OCX_*` / `__OCX_*` namespace ocx
/// reserves for its own resolution-affecting configuration.
///
/// The single gate behind the rule that no env surface — project `[env]`,
/// `[group.<name>.env]`, `ocx exec --env`, the forwarded `OCX_ENV`
/// payload, **package metadata**, and `ocx package create` — can set an `OCX_*`
/// key. Without it a checked-in file could set `OCX_DEFAULT_REGISTRY`,
/// `OCX_INDEX`, `OCX_MIRRORS`, `OCX_PATCHES`, `OCX_OFFLINE` or
/// `OCX_ALLOW_YANKED` and reconfigure how ocx itself resolves — and
/// `Env::apply_ocx_config` would forward the result to every child. Config
/// governing its own governance is a materialized vulnerability class, not a
/// theoretical one.
///
/// The surfaces split on **how** they refuse. The four user-authored ones and
/// `ocx package create` reject outright; package metadata is only *skipped*, in
/// `PackageManager::resolve_env_with_attribution`, because an already-published
/// artifact must keep resolving.
///
/// Matched case-insensitively: Windows environment names are case-insensitive,
/// so a lowercase `ocx_offline` would land in the same slot as `OCX_OFFLINE`.
///
/// Distinct from [`is_valid_env_key`], which enforces the POSIX *name grammar*.
/// Both gates apply — grammar first, then namespace.
pub fn is_reserved_ocx_key(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    upper.starts_with("OCX_") || upper.starts_with("__OCX_")
}
