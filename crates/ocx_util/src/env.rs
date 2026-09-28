// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The process-environment accessor — the one seam every crate reads the environment through.

use std::path::PathBuf;

/// The test-only environment override table; `__testing` because `cfg(test)` does not cross crates.
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

    /// Locks [`OVERRIDES`], recovering from poison so one panicked test cannot fail every later one.
    fn overrides() -> std::sync::MutexGuard<'static, HashMap<String, Option<String>>> {
        OVERRIDES.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// `Some(None)` is an explicit removal; `None` means no override, so fall through to the process env.
    pub(super) fn get(key: &str) -> Option<Option<String>> {
        match overrides().get(key).cloned() {
            None if is_hermetic() => Some(None),
            value => value,
        }
    }

    /// Whether an [`EnvLock::hermetic`] guard is live; a reader that bypasses [`super::var`] must check it.
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

    /// Serialises env-touching tests and injects overrides without `std::env::set_var`; cleared on drop.
    pub struct EnvLock {
        _guard: MutexGuard<'static, ()>,
    }

    impl EnvLock {
        fn acquire() -> Self {
            // Poison is safe to recover: `reset` clears whatever a panicked test left.
            let guard = TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
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

        /// Makes the table the whole environment until drop: unset keys read absent and [`super::current_dir`] answers `cwd`.
        ///
        /// Readers that bypass [`super::var`] are not covered — see [`is_hermetic`].
        pub fn hermetic(&self, cwd: impl Into<PathBuf>) {
            *CWD.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(cwd.into());
            HERMETIC.store(true, Ordering::SeqCst);
        }

        /// Points `OCX_HOME` at a fresh empty directory (bind the guard), so `$OCX_HOME/ocx.toml` cannot pick up the developer's real file.
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

    /// Acquires the process-wide environment lock for the current test.
    pub fn lock() -> EnvLock {
        EnvLock::acquire()
    }
}

#[cfg(target_os = "windows")]
pub const PATH_SEPARATOR: &str = ";";

#[cfg(not(target_os = "windows"))]
pub const PATH_SEPARATOR: &str = ":";

/// Process working directory, through the seam tests override.
pub fn current_dir() -> std::io::Result<PathBuf> {
    #[cfg(any(test, feature = "__testing"))]
    if let Some(cwd) = overrides::cwd() {
        return Ok(cwd);
    }
    std::env::current_dir()
}

/// The current user's home directory.
///
/// `std::env::home_dir`, never `dirs::home_dir`, which ignores `%USERPROFILE%` and hands one invocation two homes.
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
        // The key alone, never the value: this reader carries credentials (`CI_JOB_TOKEN`) into durable CI logs.
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

/// Validates that `key` matches the POSIX environment-variable name grammar (`[A-Za-z_][A-Za-z0-9_]*`).
///
/// Every shell and CI emitter gates keys here, since value escaping leaves the key slot open to injection (CWE-77).
/// It also gates `${self.env.KEY}` tokens: narrowing it refuses tokens published packages carry.
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

/// Returns `true` when `key` falls in the reserved `OCX_*` / `__OCX_*` namespace.
///
/// The one gate stopping every env surface from setting `OCX_*`, or a checked-in file could reconfigure resolution.
pub fn is_reserved_ocx_key(key: &str) -> bool {
    // Case-insensitive: on Windows `ocx_offline` lands in the same slot as `OCX_OFFLINE`.
    let upper = key.to_ascii_uppercase();
    upper.starts_with("OCX_") || upper.starts_with("__OCX_")
}
