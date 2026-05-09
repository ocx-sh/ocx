// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Per-project exclusive lock on `ocx.toml`.
//!
//! Writers acquire an exclusive advisory flock on `ocx.toml` itself via
//! [`acquire_project_lock`] before mutating the project state. The lock is
//! released automatically when the returned [`FileLock`] guard is dropped.
//!
//! Readers (`ProjectLock::load`, `ProjectLock::from_path`,
//! `ProjectConfig::from_path`) never take a lock — concurrent reads are
//! always allowed.
//!
//! `init_project` does NOT call this function — `ocx.toml` does not exist
//! yet when `init_project` runs, so there is nothing to lock.

use std::path::Path;

use crate::file_lock::FileLock;

use super::Error;
use super::error::{ProjectError, ProjectErrorKind};

/// Acquire an exclusive advisory lock on `<project_root>/ocx.toml`.
///
/// Convenience wrapper around [`acquire_project_lock_for_file`] for the
/// canonical `ocx.toml` case. Use [`acquire_project_lock_for_file`]
/// directly when the project config has a custom filename (e.g.
/// `--project=custom.toml`) — the flock target must be the actual
/// resolved file path so two writers cannot race through aliased
/// names.
///
/// # Errors
///
/// See [`acquire_project_lock_for_file`].
pub async fn acquire_project_lock(project_root: &Path) -> Result<FileLock, Error> {
    acquire_project_lock_for_file(&project_root.join("ocx.toml")).await
}

/// Acquire an exclusive advisory lock on the project config file at
/// `config_path`.
///
/// Opens the config file (which must already exist) with `O_NOFOLLOW`
/// on Unix to prevent a TOCTOU attack where a symlink is planted at the
/// path. Calls [`FileLock::try_exclusive`] (non-blocking) and maps the
/// three outcomes:
///
/// - `Ok(Some(guard))` → lock acquired; returns the guard.
/// - `Ok(None)` → another process holds the lock; returns
///   [`ProjectErrorKind::Locked`].
/// - `Err(e)` → I/O error (e.g., file not found); returns
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
/// - [`ProjectErrorKind::Io`] — the config file could not be opened (e.g.,
///   missing, permission denied, or the path is a symlink on Unix).
pub async fn acquire_project_lock_for_file(config_path: &Path) -> Result<FileLock, Error> {
    let config_path = config_path.to_path_buf();

    let guard = tokio::task::spawn_blocking(move || -> Result<FileLock, Error> {
        let file = open_toml_no_follow(&config_path)?;

        match FileLock::try_exclusive(file) {
            Ok(Some(guard)) => Ok(guard),
            Ok(None) => Err(Error::Project(ProjectError::new(config_path, ProjectErrorKind::Locked))),
            Err(e) => Err(Error::Project(ProjectError::new(config_path, ProjectErrorKind::Io(e)))),
        }
    })
    .await
    .expect("spawn_blocking panicked in acquire_project_lock_for_file")?;

    Ok(guard)
}

/// Open `ocx.toml` without following symlinks.
///
/// On Unix uses `O_NOFOLLOW` so that a symlink planted at the path causes
/// an I/O error rather than redirecting the advisory lock to an
/// attacker-chosen file.
///
/// On non-Unix platforms performs a `symlink_metadata` pre-check as a
/// best-effort guard (narrow TOCTOU window, acceptable on platforms without
/// `O_NOFOLLOW`).
fn open_toml_no_follow(toml_path: &std::path::Path) -> Result<std::fs::File, Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            // SAFETY: O_NOFOLLOW is a standard POSIX flag; the cast to i32 is lossless.
            .custom_flags(libc::O_NOFOLLOW)
            .open(toml_path)
            .map_err(|e| Error::Project(ProjectError::new(toml_path.to_path_buf(), ProjectErrorKind::Io(e))))
    }
    #[cfg(not(unix))]
    {
        // Non-Unix best-effort: reject symlinks before opening.
        match std::fs::symlink_metadata(toml_path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(Error::Project(ProjectError::new(
                    toml_path.to_path_buf(),
                    ProjectErrorKind::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "ocx.toml path is a symlink",
                    )),
                )));
            }
            _ => {}
        }
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(toml_path)
            .map_err(|e| Error::Project(ProjectError::new(toml_path.to_path_buf(), ProjectErrorKind::Io(e))))
    }
}
