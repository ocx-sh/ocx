// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

use super::acquire_result::TempAcquireResult;

/// A discovered temp entry (directory and/or lock file).
pub struct TempEntry {
    /// May not exist on disk.
    pub dir: PathBuf,
    pub has_lock_file: bool,
}

/// A stale temp entry ready for cleanup.
pub enum StaleEntry {
    Locked(TempAcquireResult),
    /// Directory exists but no lock file — safe to remove directly.
    Orphan(PathBuf),
}
