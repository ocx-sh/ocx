// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Project-root digests for the reachability graph (`adr_project_gc_symlink_ledger.md`).

use std::path::PathBuf;

use ocx_oci::PinnedPackageRef;

/// GC roots pinned by one registered project's `ocx.lock`.
#[derive(Clone, Debug)]
pub struct ProjectRootDigests {
    /// Canonical `ocx.lock` path, for `ocx clean --dry-run`'s `Held By` column only.
    pub ocx_lock_path: PathBuf,
    /// Pinned identifiers from the lock's tool entries.
    pub digests: Vec<PinnedPackageRef>,
}
