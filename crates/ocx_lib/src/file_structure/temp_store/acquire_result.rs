// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::{file_lock, log};

use super::TempStore;
use super::temp_dir::TempDir;

/// Result of acquiring a temp directory via [`TempStore::try_acquire`] or
/// [`TempStore::acquire_with_timeout`].
///
/// Holds the exclusive lock for the directory's lifetime. When this value
/// is dropped, the OS lock is released and the sibling `.lock` file is
/// deleted (best-effort).
///
/// Field order matters: `lock` is declared first so it is dropped first,
/// releasing the OS lock before the `Drop` impl deletes the file.
pub struct TempAcquireResult {
    pub lock: file_lock::FileLock,
    pub dir: TempDir,
    /// `true` if the directory contained leftover artifacts that were cleaned.
    pub was_cleaned: bool,
}

impl Drop for TempAcquireResult {
    fn drop(&mut self) {
        let lock_path = TempStore::lock_path_for(&self.dir.dir);
        if let Err(e) = std::fs::remove_file(&lock_path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            log::debug!("Failed to remove temp lock file {}: {}", lock_path.display(), e);
        }
    }
}
