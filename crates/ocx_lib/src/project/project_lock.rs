// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Acquires the project mutation lock directly on `ocx.toml`.
//!
//! WHY IN-PLACE: the lock target is the data file itself. Writers must
//! rewrite `ocx.toml` through the lock-owning handle
//! ([`LockedFile::replace_bytes`]) — never through a separate tempfile
//! that is then renamed over the data file. A rename would rotate
//! `ocx.toml`'s inode and strand the lock fd on the orphan, breaking
//! mutual exclusion on Windows (where `LockFileEx` is per-handle and
//! mandatory).
//!
//! Trade-off accepted: in-place truncate+write loses the kill-9
//! atomicity that tempfile+rename provides. A SIGKILL between
//! `set_len(0)` and `sync_data` leaves `ocx.toml` truncated or partial.
//! Recovery is manual (restore from VCS / re-run the mutator). The
//! design rule for the project: in-place lock for the canonical
//! project config file; no sidecar, no rename.
//!
//! Readers (`ProjectLock::load`, `ProjectLock::from_path`,
//! `ProjectConfig::from_path`) never take a lock — concurrent reads are
//! always allowed.
//!
//! `init_project` does NOT call this function — `ocx.toml` does not exist
//! yet when `init_project` runs, so there is nothing to lock.

use std::path::Path;

use crate::utility::fs::LockedFile;

use super::Error;
use super::error::{ProjectError, ProjectErrorKind};

/// How long a contended acquire keeps retrying before reporting
/// [`ProjectErrorKind::Locked`], and the interval between attempts.
///
/// A contended `flock` does not always mean a live writer. `flock` is held by
/// the *open file description*, and `fork` duplicates every descriptor into
/// the child; `O_CLOEXEC` only drops it at `execve`. So any process that
/// spawns a subprocess while this lock is held keeps the lock alive in the
/// child until that child execs, even after the guard is dropped here. The
/// same holds for a concurrent writer that is milliseconds from releasing.
/// Reporting `Locked` on the first refusal turns both into a hard
/// `ExitCode::TempFail` for the user.
///
/// The budget is sized against the measured fork→exec window on this
/// codebase's own suite (32-way parallelism: p50 2.8 ms, max 27.9 ms), with
/// well over an order of magnitude of headroom. A writer that genuinely holds
/// the lock for longer still surfaces `Locked` — the budget smooths transient
/// contention, it does not wait out a real one.
const CONTENTION_BUDGET: std::time::Duration = std::time::Duration::from_millis(500);
const CONTENTION_TICK: std::time::Duration = std::time::Duration::from_millis(25);

/// Acquire an exclusive advisory lock on `<project_root>/ocx.toml`.
///
/// Convenience wrapper around [`acquire_project_lock_for_file`] for the
/// canonical `ocx.toml` case. Use [`acquire_project_lock_for_file`]
/// directly when the project config has a custom filename
/// (e.g. `--project=custom.toml`).
///
/// # Errors
///
/// See [`acquire_project_lock_for_file`].
pub async fn acquire_project_lock(project_root: &Path) -> Result<LockedFile, Error> {
    acquire_project_lock_for_file(&project_root.join("ocx.toml")).await
}

/// Acquire an exclusive advisory lock on the project config file at
/// `config_path`.
///
/// The file is created if it does not yet exist. Every open attempt is
/// preceded by a symlink refusal ([`refuse_symlink_at`]) — `ocx.toml` is the
/// canonical project declaration, never a symlink target.
///
/// Polls [`LockedFile::try_exclusive`] and maps the three outcomes:
///
/// - `Ok(Some(guard))` → lock acquired; returns the guard.
/// - `Ok(None)` → contended; retried every [`CONTENTION_TICK`] until
///   [`CONTENTION_BUDGET`] is exhausted, then [`ProjectErrorKind::Locked`].
/// - `Err(e)` → I/O error (e.g., permission denied); returns
///   [`ProjectErrorKind::Io`].
///
/// The returned guard holds the exclusive lock until it is dropped.
/// All blocking work runs on a `spawn_blocking` thread so the async
/// runtime is not stalled.
///
/// # Errors
///
/// - [`ProjectErrorKind::Locked`] — a writer still held the exclusive lock
///   after [`CONTENTION_BUDGET`] of retrying.
/// - [`ProjectErrorKind::Io`] — the file could not be opened or created
///   (e.g., permission denied, or the path is a symlink).
pub async fn acquire_project_lock_for_file(config_path: &Path) -> Result<LockedFile, Error> {
    // Acquire an exclusive lock on ocx.toml itself. LockedFile::try_exclusive
    // creates the file if absent and returns Ok(None) on contention; retry a
    // contended acquire for CONTENTION_BUDGET before giving up (see the
    // constant for why a single refusal is not evidence of a live writer).
    //
    // Deliberately a try-loop rather than LockedFile::open_exclusive_with_timeout:
    // that helper runs a blocking flock inside spawn_blocking and abandons the
    // task on timeout, so the orphan later acquires the lock and drops it —
    // which would manufacture exactly the spurious contention this loop exists
    // to absorb. The try-loop also preserves the three-way outcome
    // (acquired / contended / I/O error) that the Locked-vs-Io mapping needs.
    let deadline = std::time::Instant::now() + CONTENTION_BUDGET;
    loop {
        // Inside the loop, not ahead of it: the refusal guards an open, and
        // this loop performs up to CONTENTION_BUDGET / CONTENTION_TICK of them.
        refuse_symlink_at(config_path).await?;

        let maybe_guard = LockedFile::try_exclusive(config_path).await.map_err(|e| {
            // crate::Error::InternalFile → unwrap into ProjectError::Io so the
            // caller sees a consistent ProjectErrorKind.
            Error::Project(ProjectError::new(
                config_path.to_path_buf(),
                ProjectErrorKind::Io(std::io::Error::other(e)),
            ))
        })?;

        match maybe_guard {
            Some(guard) => return Ok(guard),
            None if std::time::Instant::now() < deadline => {
                tokio::time::sleep(CONTENTION_TICK).await;
            }
            None => {
                return Err(Error::Project(ProjectError::new(
                    config_path.to_path_buf(),
                    ProjectErrorKind::Locked,
                )));
            }
        }
    }
}

/// Refuse a symlink at `config_path` before the caller opens it.
///
/// `O_NOFOLLOW` discipline for the data file, applied by hand: [`LockedFile`]
/// takes no `OpenOptions`, so the refusal is a `symlink_metadata` check on
/// every platform rather than an open flag. `ocx.toml` is the canonical project
/// declaration the invoking user owns; a symlink there is a misconfiguration at
/// best and, at worst, a redirect that `MutationGuard::commit` would truncate
/// and rewrite through the lock-owning handle (CWE-59).
///
/// Must run before **every** open, which is why
/// [`acquire_project_lock_for_file`] calls it inside its retry loop rather than
/// once ahead of it. A contended acquire reopens the path up to
/// [`CONTENTION_BUDGET`] / [`CONTENTION_TICK`] times, and anyone who can write
/// the project directory can hold an `flock` on `ocx.toml` to *force* exactly
/// that, then swap a symlink in during a tick (CWE-367).
///
/// A window survives — check and open are two syscalls, and only `O_NOFOLLOW`
/// on [`LockedFile`]'s own open would close it — but it is one scheduling gap
/// wide, not the whole contention budget.
///
/// # Errors
///
/// - [`ProjectErrorKind::Io`] — the path is a symlink, or its metadata could
///   not be read. `NotFound` is not an error: `LockedFile::try_exclusive`
///   creates the file.
async fn refuse_symlink_at(config_path: &Path) -> Result<(), Error> {
    let refusal = |io_error| Error::Project(ProjectError::new(config_path, ProjectErrorKind::Io(io_error)));
    match tokio::fs::symlink_metadata(config_path).await {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(refusal(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "ocx.toml path is a symlink",
        ))),
        Ok(_) => Ok(()),
        // NotFound is fine — LockedFile::try_exclusive will create the file.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        // Any other metadata error is an I/O failure on the config path.
        Err(error) => Err(refusal(error)),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tempfile::tempdir;

    use super::*;

    /// Helper: create a minimal ocx.toml at `dir/ocx.toml`.
    fn write_ocx_toml(dir: &std::path::Path) -> PathBuf {
        let path = dir.join("ocx.toml");
        std::fs::write(&path, "[tools]\n").expect("write ocx.toml");
        path
    }

    /// Confirm that acquiring the lock does NOT create a sidecar `.lock` file
    /// next to `ocx.toml`, and that `ocx.toml` itself is left byte-identical.
    #[tokio::test(flavor = "multi_thread")]
    async fn acquire_project_lock_leaves_no_sidecar() {
        let dir = tempdir().unwrap();
        let config_path = write_ocx_toml(dir.path());
        let sidecar_path = config_path.with_added_extension("lock");

        let guard = acquire_project_lock_for_file(&config_path)
            .await
            .expect("first lock acquisition must succeed");

        // No sidecar file must appear.
        assert!(
            !sidecar_path.exists(),
            "in-place lock must not create a sidecar .lock file"
        );

        // Release the guard BEFORE raw verification reads. On Windows `LockFileEx`
        // is per-handle; a second raw read against the locked range hits
        // `ERROR_LOCK_VIOLATION (33)`. Tests must drop the guard before any
        // out-of-band `std::fs::read*` of ocx.toml.
        drop(guard);

        // ocx.toml content is unmodified by the lock acquisition.
        let toml_content = std::fs::read_to_string(&config_path).unwrap();
        assert_eq!(
            toml_content, "[tools]\n",
            "ocx.toml must be unmodified by lock acquisition"
        );
    }

    /// Two `acquire_project_lock_for_file` attempts; second returns
    /// `ProjectErrorKind::Locked`. The contended file is `ocx.toml` itself.
    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_mutation_contention_blocks_second_writer() {
        let dir = tempdir().unwrap();
        let config_path = write_ocx_toml(dir.path());

        // First acquisition must succeed.
        let guard = acquire_project_lock_for_file(&config_path)
            .await
            .expect("first exclusive lock must succeed");

        // Second attempt must fail with Locked.
        let err = acquire_project_lock_for_file(&config_path)
            .await
            .expect_err("second lock attempt must fail while first holds");

        assert!(
            matches!(&err, Error::Project(pe) if matches!(pe.kind, ProjectErrorKind::Locked)),
            "expected ProjectErrorKind::Locked on contention; got: {err}"
        );

        // Release the guard BEFORE raw verification reads (see
        // `acquire_project_lock_leaves_no_sidecar` for the Windows F1 rationale).
        drop(guard);

        // ocx.toml is untouched by the lock machinery.
        let toml_content = std::fs::read_to_string(&config_path).unwrap();
        assert_eq!(toml_content, "[tools]\n", "ocx.toml must be unmodified");
    }

    /// Regression: a lock released while the acquirer is waiting must be
    /// picked up, not reported as `Locked`.
    ///
    /// The one-shot `try_exclusive` this replaced failed on the first refusal,
    /// which made every transient hold a hard error. Transient holds are not
    /// hypothetical: `flock` lives on the open file description, `fork`
    /// duplicates it into the child, and `O_CLOEXEC` only drops it at `execve`
    /// — so a subprocess spawned anywhere in the process keeps this lock alive
    /// for the child's fork→exec window even after the guard here is dropped.
    /// A 60 ms hold stands in for that window; it exceeds `CONTENTION_TICK`,
    /// so the acquire must genuinely retry rather than win on the first try.
    #[tokio::test(flavor = "multi_thread")]
    async fn acquire_waits_out_a_transient_holder() {
        let dir = tempdir().unwrap();
        let config_path = write_ocx_toml(dir.path());

        let guard = acquire_project_lock_for_file(&config_path)
            .await
            .expect("first lock acquisition must succeed");
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(60)).await;
            drop(guard);
        });

        acquire_project_lock_for_file(&config_path)
            .await
            .expect("a lock released mid-wait must be acquired, not reported as Locked");
    }

    /// Unix-only: an in-place rewrite via the lock-owning handle MUST keep
    /// `ocx.toml`'s inode stable (no rename, no orphan inode, no lock-fd
    /// stranding). This is the core property that makes the in-place design
    /// F2-safe on Windows.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn replace_bytes_keeps_ocx_toml_inode_stable() {
        use std::os::unix::fs::MetadataExt;

        let dir = tempdir().unwrap();
        let config_path = write_ocx_toml(dir.path());

        let mut guard = acquire_project_lock_for_file(&config_path)
            .await
            .expect("lock acquisition must succeed");

        let toml_inode_before = std::fs::metadata(&config_path).expect("ocx.toml must exist").ino();

        // Rewrite ocx.toml in place through the lock-owning handle. This is the
        // primitive `MutationGuard::commit` calls — it must NOT rotate the inode.
        guard
            .replace_bytes(b"[tools]\n# updated\n")
            .await
            .expect("replace_bytes through lock-owning handle must succeed");

        let toml_inode_after = std::fs::metadata(&config_path)
            .expect("ocx.toml must still exist after replace_bytes")
            .ino();
        assert_eq!(
            toml_inode_before, toml_inode_after,
            "in-place replace_bytes must NOT rotate the ocx.toml inode (lock fd remains valid)"
        );

        // Content reflects the new bytes.
        let toml_content = std::fs::read_to_string(&config_path).unwrap();
        assert_eq!(toml_content, "[tools]\n# updated\n");

        drop(guard);
    }

    /// Windows cfg-gated: hold the in-place lock and rewrite `ocx.toml`
    /// through the lock-owning handle. The lock fd never strands (no rename
    /// happens), so the rewrite must not hit os error 33
    /// (`ERROR_LOCK_VIOLATION`).
    #[cfg(target_os = "windows")]
    #[tokio::test(flavor = "multi_thread")]
    async fn replace_bytes_via_locked_handle_no_lock_violation() {
        let dir = tempdir().unwrap();
        let config_path = write_ocx_toml(dir.path());

        let mut guard = acquire_project_lock_for_file(&config_path)
            .await
            .expect("lock acquisition must succeed");

        // Rewrite through the lock-owning handle — F2-safe by construction.
        for i in 0u32..10 {
            guard
                .replace_bytes(format!("[tools]\n# iteration {i}\n").as_bytes())
                .await
                .expect("replace_bytes through lock-owning handle must not hit os error 33");
        }

        drop(guard);
    }

    /// Regression: the symlink refusal must re-run before **every** open, not
    /// once before the retry loop.
    ///
    /// The loop reopens `ocx.toml` up to 21 times across `CONTENTION_BUDGET`,
    /// so a refusal hoisted out of it guards the first open and none of the
    /// other twenty. Anyone who can write the project directory can take an
    /// `flock` on `ocx.toml` to *force* the retry, plant a symlink during a
    /// tick, and be handed a lock-owning handle on the target — which
    /// `MutationGuard::commit` then truncates and rewrites through
    /// `LockedFile::replace_bytes` (CWE-59 / CWE-367).
    ///
    /// The holder is never released, so the acquire can only ever return `Ok`
    /// by following the planted symlink: a success here is the vulnerability,
    /// not a race the test lost.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn a_symlink_planted_during_the_retry_loop_is_refused() {
        let dir = tempdir().unwrap();
        let config_path = write_ocx_toml(dir.path());
        let victim_path = dir.path().join("victim.toml");
        std::fs::write(&victim_path, "[victim]\n").expect("write the symlink target");

        // Never dropped: this is the attacker's `flock`, and it is what forces
        // the acquire below into the retry loop.
        let _holder = acquire_project_lock_for_file(&config_path)
            .await
            .expect("first lock acquisition must succeed");

        let planted_at = config_path.clone();
        let planted_to = victim_path.clone();
        tokio::spawn(async move {
            // One tick in, leaving ~460 ms of the 500 ms budget to be caught in.
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
            std::fs::remove_file(&planted_at).expect("unlink ocx.toml");
            std::os::unix::fs::symlink(&planted_to, &planted_at).expect("plant the symlink");
        });

        let err = acquire_project_lock_for_file(&config_path)
            .await
            .expect_err("a symlink planted mid-retry must be refused, not followed");

        assert!(
            matches!(&err, Error::Project(project_error)
                if matches!(&project_error.kind, ProjectErrorKind::Io(io_error)
                    if io_error.to_string().contains("symlink"))),
            "expected the symlink refusal; got: {err}"
        );
    }
}
