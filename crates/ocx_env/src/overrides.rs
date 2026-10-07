// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The test-only environment override table; `__testing` because `cfg(test)` does not cross crates.
//!
//! A declared variable is set through its static, so a misspelled name is a compile error:
//!
//! ```
//! let env = ocx_env::overrides::lock();
//! env.set(&ocx_env::OCX_HOME, "/tmp/ocx");
//! assert_eq!(ocx_env::OCX_HOME.get().as_deref(), Some("/tmp/ocx"));
//! ```
//!
//! ```compile_fail,E0425
//! let env = ocx_env::overrides::lock();
//! env.set(&ocx_env::OCX_HOEM, "/tmp/ocx");
//! ```
//!
//! A name the registry does not declare takes [`EnvLock::set_raw`]; handed a declared one it panics.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError};

use tempfile::TempDir;

use crate::{EnvVar, Retired};

static TEST_LOCK: Mutex<()> = Mutex::new(());
static OVERRIDES: LazyLock<Mutex<HashMap<String, Option<String>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
/// Set by [`EnvLock::hermetic`]: a key with no override reads as absent
/// instead of falling through to the process environment.
static HERMETIC: AtomicBool = AtomicBool::new(false);
/// The working directory [`crate::current_dir`] answers while hermetic.
static CWD: Mutex<Option<PathBuf>> = Mutex::new(None);
/// The retired-name table [`EnvLock::retire`] swapped in for [`crate::RETIRED`].
static RETIRED: Mutex<Option<&'static [Retired]>> = Mutex::new(None);

/// Locks [`OVERRIDES`], recovering from poison so one panicked test cannot fail every later one.
fn overrides() -> MutexGuard<'static, HashMap<String, Option<String>>> {
    OVERRIDES.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `Some(None)` is an explicit removal; `None` means no override, so fall through to the process env.
pub(crate) fn get(key: &str) -> Option<Option<String>> {
    match overrides().get(key).cloned() {
        None if is_hermetic() => Some(None),
        value => value,
    }
}

/// Every override, removals included, for [`crate::snapshot`].
pub(crate) fn entries() -> Vec<(String, Option<String>)> {
    overrides()
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

/// Whether an [`EnvLock::hermetic`] guard is live; a reader that bypasses the seam must check it.
pub fn is_hermetic() -> bool {
    HERMETIC.load(Ordering::SeqCst)
}

/// The directory [`crate::current_dir`] reports while hermetic.
pub(crate) fn cwd() -> Option<PathBuf> {
    CWD.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

/// The table a live [`EnvLock::retire`] injected.
pub(crate) fn retired() -> Option<&'static [Retired]> {
    *RETIRED.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `key` itself, or a panic naming it when a declaration owns that name.
fn refuse_declared(key: String) -> String {
    assert!(
        crate::all().all(|var| var.name != key),
        "'{key}' is a declared variable: pass its static to `set` / `remove`"
    );
    key
}

fn reset() {
    overrides().clear();
    HERMETIC.store(false, Ordering::SeqCst);
    *CWD.lock().unwrap_or_else(PoisonError::into_inner) = None;
    *RETIRED.lock().unwrap_or_else(PoisonError::into_inner) = None;
}

/// Serialises env-touching tests and injects overrides without `std::env::set_var`; cleared on drop.
pub struct EnvLock {
    _guard: MutexGuard<'static, ()>,
}

impl EnvLock {
    fn acquire() -> Self {
        // Poison is safe to recover: `reset` clears whatever a panicked test left.
        let guard = TEST_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        reset();
        Self { _guard: guard }
    }

    /// Injects `value` for the declared `var`, visible to every reader of the seam.
    ///
    /// A secret goes through [`crate::SecretVar::declaration`].
    pub fn set(&self, var: &'static EnvVar, value: impl Into<String>) {
        overrides().insert(var.name.to_owned(), Some(value.into()));
    }

    /// Marks the declared `var` as removed: every reader of the seam sees it unset.
    pub fn remove(&self, var: &'static EnvVar) {
        overrides().insert(var.name.to_owned(), None);
    }

    /// [`Self::set`] for a name the registry does not declare: a foreign tool's variable or a test fixture's.
    ///
    /// # Panics
    ///
    /// When `key` is a declared name, so no declared variable is reachable through an unchecked string.
    pub fn set_raw(&self, key: impl Into<String>, value: impl Into<String>) {
        let key = refuse_declared(key.into());
        overrides().insert(key, Some(value.into()));
    }

    /// [`Self::remove`] for a name the registry does not declare.
    ///
    /// # Panics
    ///
    /// When `key` is a declared name.
    pub fn remove_raw(&self, key: impl Into<String>) {
        let key = refuse_declared(key.into());
        overrides().insert(key, None);
    }

    /// Makes the table the whole environment until drop: unset keys read absent and [`crate::current_dir`] answers `cwd`.
    ///
    /// Readers that bypass the seam are not covered — see [`is_hermetic`].
    pub fn hermetic(&self, cwd: impl Into<PathBuf>) {
        *CWD.lock().unwrap_or_else(PoisonError::into_inner) = Some(cwd.into());
        HERMETIC.store(true, Ordering::SeqCst);
    }

    /// Replaces [`crate::RETIRED`] with `table` for every reader until drop, so a caller can test a retirement the build has none of.
    pub fn retire(&self, table: &'static [Retired]) {
        *RETIRED.lock().unwrap_or_else(PoisonError::into_inner) = Some(table);
    }

    /// Points `OCX_HOME` at a fresh empty directory (bind the guard), so `$OCX_HOME/ocx.toml` cannot pick up the developer's real file.
    pub fn isolate_project_home(&self) -> TempDir {
        let home = TempDir::new().expect("create temp OCX_HOME");
        self.set(&crate::OCX_HOME, home.path().to_str().expect("temp path is utf-8"));
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
