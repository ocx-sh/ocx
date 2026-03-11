// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, MutexGuard};

static TEST_LOCK: Mutex<()> = Mutex::new(());
static OVERRIDES: LazyLock<Mutex<HashMap<String, Option<String>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Called by [`crate::env::var`] in `#[cfg(test)]`.
///
/// Returns:
/// - `Some(Some(v))` — key is overridden with value `v`
/// - `Some(None)` — key is explicitly removed (treat as not present)
/// - `None` — key has no override (fall through to `std::env::var`)
pub(crate) fn get_override(key: &str) -> Option<Option<String>> {
    OVERRIDES.lock().unwrap().get(key).cloned()
}

/// A guard that serialises environment-touching tests and provides safe
/// injection of environment variable overrides without calling
/// `std::env::set_var` / `std::env::remove_var`.
///
/// Acquire with [`lock()`].  Use [`EnvLock::set`] and [`EnvLock::remove`] to
/// inject values; they are visible to any code that reads env vars through
/// [`crate::env::var`].  All overrides are cleared when the guard is dropped.
pub struct EnvLock {
    _guard: MutexGuard<'static, ()>,
}

impl EnvLock {
    fn acquire() -> Self {
        let guard = TEST_LOCK.lock().unwrap();
        // Clear any stale overrides left by a previously panicked test.
        OVERRIDES.lock().unwrap().clear();
        Self { _guard: guard }
    }

    /// Injects `value` for `key`.  Visible to [`crate::env::var`].
    pub fn set(&self, key: impl Into<String>, value: impl Into<String>) {
        OVERRIDES.lock().unwrap().insert(key.into(), Some(value.into()));
    }

    /// Marks `key` as removed.  [`crate::env::var`] will return `None` for it.
    pub fn remove(&self, key: impl Into<String>) {
        OVERRIDES.lock().unwrap().insert(key.into(), None);
    }
}

impl Drop for EnvLock {
    fn drop(&mut self) {
        OVERRIDES.lock().unwrap().clear();
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
