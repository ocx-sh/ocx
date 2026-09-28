// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_util::fs::LockedFile;

use super::TempStore;
use super::temp_dir::TempDir;

/// Exclusive lock on a temp directory; dropping it releases the lock and deletes the `.lock` file.
pub struct TempAcquireResult {
    pub lock: LockedFile,
    pub dir: TempDir,
    /// `true` if the directory contained leftover artifacts that were cleaned.
    pub was_cleaned: bool,
}

// Runs before the `lock` field drops, so the `.lock` file is never on disk unlocked.
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
