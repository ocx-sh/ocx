// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Project-root digests for the reachability graph.
//!
//! [`ProjectRootDigests`] is the value type produced by
//! `collect_project_roots` (in `tasks/clean.rs`) and consumed by
//! [`super::super::reachability_graph::ReachabilityGraph::build`]. Each
//! instance pairs an `ocx.lock` path (for diagnostic output in dry-run
//! previews) with the set of [`crate::oci::PinnedIdentifier`]s that lock
//! pins.
//!
//! See [`adr_clean_project_backlinks.md`] for the full design rationale and
//! the "Reachability Graph Contract Change" section for how these digests
//! are inserted as roots alongside live install symlinks.

use std::path::PathBuf;

use crate::oci::PinnedIdentifier;

/// Resolved GC roots derived from a single registered project's `ocx.lock`.
///
/// Produced by `collect_project_roots` in `tasks/clean.rs` after reading each
/// entry from [`crate::project::registry::ProjectRegistry::load_and_prune`]
/// and parsing its lock file. Consumed by
/// [`super::reachability_graph::ReachabilityGraph::build`] to add project-held
/// packages as reachability roots alongside live install symlinks.
///
/// The `ocx_lock_path` is carried through to the GC result so that
/// `ocx clean --dry-run` can surface which project is holding each retained
/// package in the `Held By` column.
#[derive(Clone, Debug)]
pub struct ProjectRootDigests {
    /// Absolute, canonicalised path to the `ocx.lock` file that contributed
    /// these digests. Used for diagnostic output only; not load-bearing for
    /// GC decisions.
    pub ocx_lock_path: PathBuf,
    /// Pinned identifiers resolved from the lock's tool entries. Each digest
    /// maps to a package-store path that will be treated as a GC root.
    pub digests: Vec<PinnedIdentifier>,
}
