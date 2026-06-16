// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Store-wide GC lock (`$OCX_STATE_DIR/gc.lock`).
//!
//! Provides mutual exclusion between the `clean` operation (exclusive) and
//! mutators that add new content objects to the store (install/pull — shared).
//!
//! ## Lock semantics
//!
//! - **Exclusive** (`GcLock::acquire_exclusive`): held by `ocx clean` while it
//!   scans and deletes objects. Blocks until no shared holder remains, or until
//!   `OCX_GC_LOCK_TIMEOUT` (default 120 s) elapses → `TempFail` (75).
//! - **Shared** (`GcLock::acquire_shared`): held briefly by **object-adding
//!   mutators** (install/pull) while they publish a new CAS object. `uninstall`
//!   and `select` do NOT acquire the shared lock — they add no new CAS content,
//!   so their concurrency window is covered by three-state ref-liveness probing
//!   and the mtime grace window rather than by this lock.
//!   On timeout (default 10 s) the mutator proceeds without the lock (debug log
//!   only) — a stuck `clean` never blocks installs.
//!
//! ## Ordering
//!
//! Callers MUST acquire the GC lock **before** any L1 object-store lock (the
//! per-digest `TempStore` lock held by `acquire_temp_dir`). Reverse order
//!  deadlocks. The acquire methods enforce this by operating at the state-dir
//! level, which is coarser than any object-level lock.
//!
//! ## Cross-instance scope
//!
//! The lock file resides in `$OCX_STATE_DIR`, which is per-instance (never
//! shared). This lock therefore provides same-instance cross-process
//! serialization only. Cross-instance safety in a shared-store setup relies on
//! content-addressing, mtime grace, and the optional shared-roots ledger —
//! not on this lock.
//!
//! ## NFS caveat
//!
//! `flock(2)` degrades silently on NFS. When `OCX_NETWORK_FS=warn` (default)
//! and the state zone is on a network filesystem, a `warn!` log is emitted.
//! Under `refuse`, `clean` exits 81 before attempting the lock at all.

use std::path::PathBuf;
use std::time::Duration;

use crate::env;
use crate::log;
use crate::utility::fs::LockedFile;

/// Default timeout for the exclusive GC lock (clean operation).
///
/// Configurable via `OCX_GC_LOCK_TIMEOUT` (seconds).
pub const DEFAULT_EXCLUSIVE_TIMEOUT: Duration = Duration::from_secs(120);

/// Default timeout for shared GC lock acquisition (mutators).
///
/// On expiry the mutator proceeds without the lock (debug log); a stuck clean
/// never blocks an install.
pub const DEFAULT_SHARED_TIMEOUT: Duration = Duration::from_secs(10);

/// RAII guard wrapping a `LockedFile` with GC-lock semantics.
///
/// Dropping this guard releases the underlying advisory lock.
pub struct GcLock {
    _inner: LockedFile,
    path: PathBuf,
}

impl std::fmt::Debug for GcLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GcLock").field("path", &self.path).finish()
    }
}

impl GcLock {
    /// Acquires an **exclusive** GC lock at `state_dir/gc.lock`.
    ///
    /// Used by `clean`. Blocks until all shared holders release or until
    /// `timeout` elapses (default: [`DEFAULT_EXCLUSIVE_TIMEOUT`]).
    ///
    /// # Errors
    ///
    /// Returns `Err(crate::Error::…)` on I/O failure or
    /// `Err(crate::Error::GcLockTimeout)` (classifies to `TempFail`, 75) on
    /// timeout.
    ///
    /// Convenience wrapper over [`Self::acquire_exclusive_with_timeout`]; `clean`
    /// uses the timeout form so it can honour `OCX_GC_LOCK_TIMEOUT`.
    #[allow(dead_code)] // public API; clean uses the configurable-timeout form.
    pub async fn acquire_exclusive(state_dir: &std::path::Path) -> crate::Result<Self> {
        Self::acquire_exclusive_with_timeout(state_dir, DEFAULT_EXCLUSIVE_TIMEOUT).await
    }

    /// Acquires an exclusive GC lock with a caller-supplied timeout.
    pub async fn acquire_exclusive_with_timeout(state_dir: &std::path::Path, timeout: Duration) -> crate::Result<Self> {
        let path = gc_lock_path(state_dir);
        match LockedFile::open_exclusive_with_timeout(path.clone(), timeout).await {
            Ok(inner) => Ok(Self { _inner: inner, path }),
            // A timeout means another process holds the lock (or a shared holder
            // could not be drained). Surface a TempFail-classified error.
            Err(error) if is_timeout(&error) => Err(crate::Error::GcLockTimeout {
                path,
                timeout_secs: timeout.as_secs(),
            }),
            Err(error) => Err(error),
        }
    }

    /// Acquires a **shared** GC lock at `state_dir/gc.lock`.
    ///
    /// Used by object-adding mutators (install/pull) — the operations that write
    /// new CAS content that clean might otherwise race with. `uninstall` and
    /// `select` add no new CAS content; their concurrency window is covered by
    /// three-state liveness and the mtime grace window, not by this lock.
    ///
    /// On timeout the caller proceeds without the lock (a stuck `clean` must not
    /// block installs). The shared timeout is fixed at [`DEFAULT_SHARED_TIMEOUT`]
    /// — never externally configurable, so a stuck clean can never block an
    /// install indefinitely.
    ///
    /// Returns `Ok(Some(guard))` when the lock was acquired, `Ok(None)` on
    /// timeout (caller proceeds without the lock).
    ///
    /// # Errors
    ///
    /// Returns `Err` only on hard I/O failure (not on timeout).
    pub async fn acquire_shared(state_dir: &std::path::Path) -> crate::Result<Option<Self>> {
        Self::acquire_shared_with_timeout(state_dir, DEFAULT_SHARED_TIMEOUT).await
    }

    /// Acquires a shared GC lock with a caller-supplied timeout.
    pub async fn acquire_shared_with_timeout(
        state_dir: &std::path::Path,
        timeout: Duration,
    ) -> crate::Result<Option<Self>> {
        let path = gc_lock_path(state_dir);
        match LockedFile::open_shared_create_with_timeout(path.clone(), timeout).await {
            Ok(inner) => Ok(Some(Self { _inner: inner, path })),
            // A shared acquirer that cannot get the lock within the (short)
            // timeout proceeds WITHOUT it — a stuck `clean` must never block an
            // install. Only a debug log, never an error.
            Err(error) if is_timeout(&error) => {
                log::debug!(
                    "GC shared lock at '{}' not acquired within {timeout:?}; proceeding without it \
                     (a concurrent clean is holding the exclusive lock)",
                    path.display()
                );
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    /// Returns the path of the lock file.
    #[allow(dead_code)] // public accessor; held guards are bound as `_gc_lock`.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

/// Returns `true` when `error` is a wrapped lock-acquisition timeout.
///
/// `LockedFile::open_*_with_timeout` maps a `FileLock` timeout to
/// `Error::InternalFile(path, io)` with `io.kind() == TimedOut`.
fn is_timeout(error: &crate::Error) -> bool {
    matches!(
        error,
        crate::Error::InternalFile(_, io) if io.kind() == std::io::ErrorKind::TimedOut
    )
}

/// Resolves the GC lock file path for the given state directory.
///
/// `$OCX_STATE_DIR/gc.lock`
pub fn gc_lock_path(state_dir: &std::path::Path) -> PathBuf {
    state_dir.join("gc.lock")
}

/// Reads `OCX_GC_LOCK_TIMEOUT` from the environment.
///
/// Returns `(exclusive_timeout, shared_timeout)` — the exclusive timeout is
/// configurable; the shared timeout is always `DEFAULT_SHARED_TIMEOUT` (not
/// externally configurable to ensure installs are never blocked indefinitely).
///
/// Falls back to (`DEFAULT_EXCLUSIVE_TIMEOUT`, `DEFAULT_SHARED_TIMEOUT`) when
/// the variable is absent, empty, or unparseable.
pub fn lock_timeouts_from_env() -> (Duration, Duration) {
    let exclusive = env::var(env::keys::OCX_GC_LOCK_TIMEOUT)
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_EXCLUSIVE_TIMEOUT);
    (exclusive, DEFAULT_SHARED_TIMEOUT)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tempfile::tempdir;

    use super::*;

    // ── gc_lock_path ────────────────────────────────────────────────────────

    #[test]
    fn gc_lock_path_is_gc_lock_under_state_dir() {
        // Requirement: system_design_shared_store.md §5 M4 item 1 —
        // the lock file lives at `$OCX_STATE_DIR/gc.lock`.
        // Traced to: plan_shared_store P3.2s GC-lock test 1.
        let dir = std::path::PathBuf::from("/some/state");
        let path = gc_lock_path(&dir);
        assert_eq!(path, dir.join("gc.lock"));
    }

    // ── acquire_exclusive ──────────────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread")]
    async fn exclusive_acquire_succeeds_when_no_contender() {
        // Requirement: system_design_shared_store.md §5 M4 item 1 —
        // `clean` takes exclusive lock at `$OCX_STATE_DIR/gc.lock`.
        // Traced to: plan_shared_store P3.2s GC-lock test 1 (exclusive acquire).
        let dir = tempdir().unwrap();
        let lock = GcLock::acquire_exclusive(dir.path()).await;
        assert!(lock.is_ok(), "exclusive acquire on idle lock must succeed");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn exclusive_acquire_creates_lock_file() {
        // The lock file must be created when it doesn't exist yet.
        // Traced to: plan_shared_store P3.2s GC-lock test 5
        // (open_shared_create_with_timeout creates-then-locks when file absent —
        // the same creation obligation applies to exclusive acquire).
        let dir = tempdir().unwrap();
        let lock_path = gc_lock_path(dir.path());
        assert!(!lock_path.exists(), "lock file must not exist before acquire");
        let _lock = GcLock::acquire_exclusive(dir.path()).await.unwrap();
        assert!(lock_path.exists(), "acquire_exclusive must create the lock file");
    }

    // ── acquire_shared ─────────────────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread")]
    async fn shared_acquire_returns_some_on_success() {
        // Requirement: system_design_shared_store.md §5 M4 item 1 —
        // mutators take a shared lock; on timeout they proceed without the lock.
        // Traced to: plan_shared_store P3.2s GC-lock test 2 (shared acquire).
        let dir = tempdir().unwrap();
        let guard = GcLock::acquire_shared(dir.path()).await;
        assert!(guard.is_ok(), "shared acquire must not return an I/O error");
        // Returns Some when lock was acquired (first acquirer, no contention).
        assert!(guard.unwrap().is_some(), "shared acquire on idle lock must return Some");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn multiple_shared_acquires_coexist() {
        // Shared locks must coexist (not block each other).
        // Traced to: plan_shared_store P3.2s GC-lock test 2.
        let dir = tempdir().unwrap();
        // First shared acquire — creates the file.
        let _guard_a = GcLock::acquire_shared(dir.path()).await.unwrap();
        // Second shared acquire must also succeed (files created by first).
        let guard_b = GcLock::acquire_shared(dir.path()).await;
        assert!(guard_b.is_ok(), "second shared acquire must not I/O-error");
        assert!(
            guard_b.unwrap().is_some(),
            "second shared acquire must coexist with first"
        );
    }

    // ── exclusive blocks shared (drain) ────────────────────────────────────

    #[tokio::test(flavor = "multi_thread")]
    async fn exclusive_acquire_with_zero_timeout_fails_when_shared_held() {
        // Requirement: system_design_shared_store.md §5 M4 item 1 —
        // exclusive blocks until all shared holders release.
        // Traced to: plan_shared_store P3.2s GC-lock test 3
        // (exclusive blocks shared / drain — timeout variation to stay deterministic).
        let dir = tempdir().unwrap();

        // Hold a shared lock.
        let _shared = GcLock::acquire_shared(dir.path()).await.unwrap().unwrap();

        // Exclusive acquire with zero timeout must fail (cannot drain the shared holder).
        let result = GcLock::acquire_exclusive_with_timeout(dir.path(), Duration::from_millis(0)).await;
        // Either a timeout error or a TempFail-classified error is acceptable.
        assert!(
            result.is_err(),
            "exclusive acquire with zero timeout must fail while shared lock is held"
        );
    }

    // ── timeout → TempFail (75) ────────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread")]
    async fn exclusive_timeout_is_classified_as_temp_fail() {
        // Requirement: system_design_shared_store.md §5 M4 item 1 —
        // timeout on exclusive acquire → `TempFail` (75).
        // Traced to: plan_shared_store P3.2s GC-lock test 4.
        use crate::cli::{ClassifyExitCode, ExitCode};

        let dir = tempdir().unwrap();

        // Hold exclusive lock to force timeout.
        let _held = GcLock::acquire_exclusive(dir.path()).await.unwrap();

        let result = GcLock::acquire_exclusive_with_timeout(dir.path(), Duration::from_millis(1)).await;
        assert!(result.is_err(), "must fail on timeout");
        let err = result.unwrap_err();
        let code = err.classify();
        assert_eq!(
            code,
            Some(ExitCode::TempFail),
            "exclusive-acquire timeout must classify as TempFail (75), got {code:?}"
        );
    }

    // ── open_shared_create_with_timeout creates-then-locks ─────────────────

    #[tokio::test(flavor = "multi_thread")]
    async fn open_shared_create_creates_file_when_absent() {
        // Requirement: system_design_shared_store.md §5 M4 item 1 —
        // `open_shared_create_with_timeout` creates the lock file (and parents)
        // if absent, then takes a shared advisory lock on it. The first mutator
        // to run before any `clean` must be able to create the file.
        // Traced to: plan_shared_store P3.2s GC-lock test 5.
        use crate::utility::fs::LockedFile;

        let dir = tempdir().unwrap();
        let lock_path = dir.path().join("subdir").join("gc.lock");
        assert!(!lock_path.exists(), "file must not pre-exist");

        let result = LockedFile::open_shared_create_with_timeout(&lock_path, Duration::from_secs(5)).await;
        assert!(result.is_ok(), "open_shared_create must succeed on absent file");
        assert!(lock_path.exists(), "must create the lock file");
    }

    // ── lock ordering (GC-before-L1) ───────────────────────────────────────

    #[test]
    fn gc_lock_path_is_in_state_dir_not_object_dir() {
        // Requirement: system_design_shared_store.md §5 M4 item 1 —
        // "Order GC-before-L1 (no deadlock). The acquire methods enforce this
        // by operating at the state-dir level, which is coarser than any
        // object-level lock."
        // Traced to: plan_shared_store P3.2s GC-lock test 6 (lock ordering).
        //
        // The ordering invariant is structural: gc.lock lives under state_dir,
        // not under packages/ or layers/ where L1 object locks live. This test
        // asserts that the path computation places the file at the state-dir
        // level so reviewers cannot accidentally relocate it into an object dir.
        let state_dir = std::path::Path::new("/var/ocx/state");
        let lock_path = gc_lock_path(state_dir);
        // Must be a direct child of state_dir — no deeper nesting.
        assert_eq!(
            lock_path.parent(),
            Some(state_dir),
            "gc.lock must be a direct child of state_dir, not nested under an object sub-dir"
        );
    }

    // ── lock_timeouts_from_env ─────────────────────────────────────────────

    #[test]
    fn lock_timeouts_from_env_returns_defaults_when_var_absent() {
        // Requirement: system_design_shared_store.md §5 M4 item 1 —
        // `OCX_GC_LOCK_TIMEOUT` absent → default 120 s exclusive, 10 s shared.
        // Traced to: plan_shared_store P3.2s GC-lock test 7 (timeout mapping).
        let env = crate::test::env::lock();
        env.remove(crate::env::keys::OCX_GC_LOCK_TIMEOUT);
        let (exclusive, shared) = lock_timeouts_from_env();
        assert_eq!(
            exclusive, DEFAULT_EXCLUSIVE_TIMEOUT,
            "exclusive timeout must default to 120 s"
        );
        assert_eq!(
            shared, DEFAULT_SHARED_TIMEOUT,
            "shared timeout must always be DEFAULT_SHARED_TIMEOUT (10 s)"
        );
    }

    #[test]
    fn lock_timeouts_from_env_parses_custom_exclusive_timeout() {
        // When OCX_GC_LOCK_TIMEOUT is set to a valid integer, the exclusive
        // timeout uses that value; shared timeout is always the default.
        // Traced to: plan_shared_store P3.2s GC-lock test 7.
        let env = crate::test::env::lock();
        env.set(crate::env::keys::OCX_GC_LOCK_TIMEOUT, "300");
        let (exclusive, shared) = lock_timeouts_from_env();
        assert_eq!(
            exclusive,
            Duration::from_secs(300),
            "exclusive timeout must use env value"
        );
        assert_eq!(
            shared, DEFAULT_SHARED_TIMEOUT,
            "shared timeout must remain at DEFAULT_SHARED_TIMEOUT regardless of env"
        );
    }

    #[test]
    fn lock_timeouts_from_env_falls_back_on_invalid_value() {
        // Unparseable value → fall back to defaults.
        // Traced to: plan_shared_store P3.2s GC-lock test 7.
        let env = crate::test::env::lock();
        env.set(crate::env::keys::OCX_GC_LOCK_TIMEOUT, "not_a_number");
        let (exclusive, _shared) = lock_timeouts_from_env();
        assert_eq!(
            exclusive, DEFAULT_EXCLUSIVE_TIMEOUT,
            "invalid env value must fall back to default exclusive timeout"
        );
    }
}
