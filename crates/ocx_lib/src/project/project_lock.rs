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
/// The file is created if it does not yet exist. A `symlink_metadata`
/// pre-check rejects symlinks at the config path on all platforms before
/// opening (defence-in-depth — `ocx.toml` is always the canonical project
/// declaration, never a symlink target).
///
/// Calls [`LockedFile::try_exclusive`] (non-blocking) and maps the
/// three outcomes:
///
/// - `Ok(Some(guard))` → lock acquired; returns the guard.
/// - `Ok(None)` → another process holds the lock; returns
///   [`ProjectErrorKind::Locked`].
/// - `Err(e)` → I/O error (e.g., permission denied); returns
///   [`ProjectErrorKind::Io`].
///
/// The returned guard holds the exclusive lock until it is dropped.
/// All blocking work runs on a `spawn_blocking` thread so the async
/// runtime is not stalled.
///
/// # Errors
///
/// - [`ProjectErrorKind::Locked`] — another writer holds the exclusive lock;
///   caller should retry with backoff.
/// - [`ProjectErrorKind::Io`] — the file could not be opened or created
///   (e.g., permission denied, or the path is a symlink).
pub async fn acquire_project_lock_for_file(config_path: &Path) -> Result<LockedFile, Error> {
    // O_NOFOLLOW discipline for the data file: reject a planted symlink at
    // ocx.toml before opening. LockedFile does not accept OpenOptions, so we
    // apply a symlink_metadata pre-check uniformly on all platforms. There is
    // a narrow TOCTOU window between the check and the open — acceptable:
    // ocx.toml is the canonical project declaration that the user owns, and a
    // symlink at this path is a misconfiguration we refuse defensively.
    //
    // NOTE: the check is performed inside spawn_blocking alongside the lock
    // acquisition to avoid a context switch between the check and the open.
    let check_path = config_path.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<(), Error> {
        match std::fs::symlink_metadata(&check_path) {
            Ok(meta) if meta.file_type().is_symlink() => Err(Error::Project(ProjectError::new(
                check_path.clone(),
                ProjectErrorKind::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "ocx.toml path is a symlink",
                )),
            ))),
            // NotFound is fine — LockedFile::try_exclusive will create the file.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            // Any other metadata error is treated as an I/O failure on the config path.
            Err(e) => Err(Error::Project(ProjectError::new(check_path, ProjectErrorKind::Io(e)))),
            Ok(_) => Ok(()),
        }
    })
    .await
    .map_err(|join_err| {
        Error::Project(ProjectError::new(
            config_path.to_path_buf(),
            ProjectErrorKind::Io(std::io::Error::other(join_err)),
        ))
    })??;

    // Acquire an exclusive lock on ocx.toml itself. LockedFile::try_exclusive
    // creates the file if absent and returns Ok(None) on contention.
    let maybe_guard = LockedFile::try_exclusive(config_path).await.map_err(|e| {
        // crate::Error::InternalFile → unwrap into ProjectError::Io so the
        // caller sees a consistent ProjectErrorKind.
        Error::Project(ProjectError::new(
            config_path.to_path_buf(),
            ProjectErrorKind::Io(std::io::Error::other(e)),
        ))
    })?;

    match maybe_guard {
        Some(guard) => Ok(guard),
        None => Err(Error::Project(ProjectError::new(
            config_path.to_path_buf(),
            ProjectErrorKind::Locked,
        ))),
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
}
