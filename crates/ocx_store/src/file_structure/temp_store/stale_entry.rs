// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

use super::acquire_result::TempAcquireResult;

/// A discovered temp entry (directory and/or lock file).
pub struct TempEntry {
    /// The temp directory path (may or may not exist on disk).
    pub dir: PathBuf,
    /// Whether the sibling `.lock` file exists.
    pub has_lock_file: bool,
}

/// A stale temp entry ready for cleanup.
pub enum StaleEntry {
    /// Has a lock file that was successfully acquired.
    Locked(TempAcquireResult),
    /// Directory exists but no lock file — safe to remove directly.
    Orphan(PathBuf),
}
