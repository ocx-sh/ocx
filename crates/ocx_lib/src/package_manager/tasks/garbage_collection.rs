// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

mod reachability_graph;

use std::collections::HashSet;
use std::path::PathBuf;

use crate::{
    file_structure::{CasTier, FileStructure},
    log,
    profile::ProfileSnapshot,
};

use reachability_graph::ReachabilityGraph;

/// Garbage collector for the object store.
///
/// Built from the current filesystem state, provides both full GC
/// ([`unreachable_objects`]) and scoped purge ([`orphaned_by_seeds`]).
/// Query methods return sets without side effects; [`delete_objects`]
/// performs the actual filesystem mutations.
pub struct GarbageCollector {
    graph: ReachabilityGraph,
}

impl GarbageCollector {
    pub async fn build(file_structure: &FileStructure, profile: &ProfileSnapshot) -> crate::Result<Self> {
        let graph = ReachabilityGraph::build(file_structure, profile).await?;
        Ok(Self { graph })
    }

    /// Returns all entries not reachable from any root.
    ///
    /// Unreferenced blobs are excluded from `clean` because the local index
    /// stores cached manifests in `blobs/` — collecting them would break
    /// subsequent installs that resolve from the local tag→manifest cache.
    /// Once the index supports fall-through resolution (local → network),
    /// this skip can be removed. See #35 for policy-based blob eviction.
    ///
    /// Referenced blobs (via `refs/blobs/`) ARE collected when their parent
    /// package is purged via [`orphaned_by_seeds`].
    pub fn unreachable_objects(&self) -> HashSet<PathBuf> {
        let reachable = self.graph.reachable();
        self.graph
            .all_entries
            .iter()
            .filter(|(path, tier)| **tier != CasTier::Blob && !reachable.contains(*path))
            .map(|(path, _)| path.clone())
            .collect()
    }

    /// Returns entries newly orphaned by removing the given seeds.
    ///
    /// Computes what becomes unreachable when the seeds are no longer roots:
    ///   reachable_with_seeds    = bfs(roots ∪ seeds)
    ///   reachable_without_seeds = bfs(roots - seeds)
    ///   orphaned = reachable_with_seeds - reachable_without_seeds
    ///
    /// Correct regardless of whether seeds are currently roots in the graph.
    pub fn orphaned_by_seeds(&self, seeds: &[PathBuf]) -> HashSet<PathBuf> {
        let seed_set: HashSet<PathBuf> = seeds.iter().cloned().collect();

        let reachable_with_seeds = self
            .graph
            .bfs(self.graph.roots.iter().cloned().chain(seeds.iter().cloned()));
        let reachable_without_seeds = self
            .graph
            .bfs(self.graph.roots.iter().filter(|r| !seed_set.contains(*r)).cloned());

        reachable_with_seeds
            .difference(&reachable_without_seeds)
            .filter(|p| self.graph.all_entries.contains_key(*p))
            .cloned()
            .collect()
    }

    /// Convenience: combines [`orphaned_by_seeds`] and [`delete_objects`].
    pub async fn purge(&self, obj_dirs: &[PathBuf]) -> crate::Result<Vec<PathBuf>> {
        let targets = self.orphaned_by_seeds(obj_dirs);
        self.delete_objects(&targets, false).await
    }

    /// Deletes the given CAS entry directories from disk.
    ///
    /// For packages: unlinks dependency, layer, and blob forward-refs via
    /// [`ReferenceManager`], then removes the directory. For layers and blobs:
    /// removes the directory directly (they have no outgoing refs).
    ///
    /// Handles `NotFound` errors from `remove_dir_all` gracefully — a
    /// concurrent deletion or external cleanup is not treated as failure.
    ///
    /// **Note:** No guard against concurrent installs. Do not run `clean`
    /// while other OCX operations are in progress.
    pub async fn delete_objects(&self, targets: &HashSet<PathBuf>, dry_run: bool) -> crate::Result<Vec<PathBuf>> {
        if targets.is_empty() {
            return Ok(Vec::new());
        }

        log::debug!(
            "{} {} entry/entries{}.",
            if dry_run { "Would remove" } else { "Removing" },
            targets.len(),
            if dry_run { " (dry run)" } else { "" },
        );

        let mut removed = Vec::new();
        let mut sorted_targets: Vec<&PathBuf> = targets.iter().collect();
        sorted_targets.sort();

        for target in sorted_targets {
            if dry_run {
                log::info!("Would remove unreferenced entry: {}", target.display());
                removed.push(target.clone());
                continue;
            }

            log::info!("Removing unreferenced entry: {}", target.display());

            match tokio::fs::remove_dir_all(target).await {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    log::debug!("Entry '{}' already removed (concurrent deletion).", target.display());
                }
                Err(e) => return Err(crate::Error::InternalFile(target.clone(), e)),
            }
            removed.push(target.clone());
        }

        log::debug!(
            "{} {} entry/entries.",
            if dry_run { "Would remove" } else { "Removed" },
            removed.len(),
        );

        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::reachability_graph::tests::{graph, graph_with_tiers, set};
    use super::*;

    fn path(name: &str) -> PathBuf {
        PathBuf::from(format!("/objects/{name}"))
    }

    fn gc(roots: &[&str], edges: &[(&str, &[&str])], extra: &[&str]) -> GarbageCollector {
        GarbageCollector {
            graph: graph(roots, edges, extra),
        }
    }

    fn gc_with_tiers(
        roots: &[&str],
        edges: &[(&str, &[&str])],
        extra: &[&str],
        tiers: &[(&str, CasTier)],
    ) -> GarbageCollector {
        GarbageCollector {
            graph: graph_with_tiers(roots, edges, extra, tiers),
        }
    }

    // ── unreachable ─────────────────────────────────────────────────────

    #[test]
    fn unreachable_no_roots() {
        assert_eq!(gc(&[], &[("A", &["B"])], &[]).unreachable_objects(), set(&["A", "B"]));
    }

    #[test]
    fn unreachable_skips_reachable() {
        assert_eq!(gc(&["A"], &[("A", &["B"])], &["X"]).unreachable_objects(), set(&["X"]));
    }

    #[test]
    fn empty_graph_nothing_unreachable() {
        assert!(gc(&[], &[], &[]).unreachable_objects().is_empty());
    }

    // ── unreachable (cross-tier) ────────────────────────────────────────

    #[test]
    fn unreachable_layers_are_collected() {
        let collector = gc_with_tiers(
            &["A"],
            &[("A", &["L1"])],
            &["L2"],
            &[("L1", CasTier::Layer), ("L2", CasTier::Layer)],
        );
        // L1 is reachable via A; L2 is unreachable and should be collected.
        assert_eq!(collector.unreachable_objects(), set(&["L2"]));
    }

    #[test]
    fn unreachable_blobs_skipped_by_clean() {
        // Unreferenced blobs are cache entries — excluded from clean (#35).
        let collector = gc_with_tiers(&["A"], &[], &["orphan_blob"], &[("orphan_blob", CasTier::Blob)]);
        assert!(collector.unreachable_objects().is_empty());
    }

    #[test]
    fn referenced_blob_survives() {
        // A (root) → B1 (blob). B1 is reachable via BFS, not collected.
        let collector = gc_with_tiers(&["A"], &[("A", &["B1"])], &[], &[("B1", CasTier::Blob)]);
        assert!(collector.unreachable_objects().is_empty());
    }

    #[test]
    fn unreachable_mixed_tiers() {
        let collector = gc_with_tiers(
            &["A"],
            &[("A", &["D", "L1"])],
            &["pkg_orphan", "layer_orphan", "blob_orphan"],
            &[
                ("L1", CasTier::Layer),
                ("layer_orphan", CasTier::Layer),
                ("blob_orphan", CasTier::Blob),
            ],
        );
        // Packages and layers collected; blobs skipped (cache retention #35).
        assert_eq!(collector.unreachable_objects(), set(&["pkg_orphan", "layer_orphan"]));
    }

    // ── orphaned_by_seeds ───────────────────────────────────────────────

    #[test]
    fn purge_cascades_chain() {
        assert_eq!(
            gc(&[], &[("A", &["B"]), ("B", &["C"])], &[]).orphaned_by_seeds(&[path("A")]),
            set(&["A", "B", "C"])
        );
    }

    #[test]
    fn purge_shared_dep_survives() {
        assert_eq!(
            gc(&["B"], &[("A", &["C"]), ("B", &["C"])], &[]).orphaned_by_seeds(&[path("A")]),
            set(&["A"])
        );
    }

    #[test]
    fn purge_shared_dep_both_removed() {
        assert_eq!(
            gc(&[], &[("A", &["C"]), ("B", &["C"])], &[]).orphaned_by_seeds(&[path("A"), path("B")]),
            set(&["A", "B", "C"])
        );
    }

    #[test]
    fn purge_unrelated_orphan_untouched() {
        assert_eq!(
            gc(&[], &[("A", &["B"])], &["X"]).orphaned_by_seeds(&[path("A")]),
            set(&["A", "B"])
        );
    }

    #[test]
    fn purge_diamond() {
        assert_eq!(
            gc(&[], &[("A", &["B", "C"]), ("B", &["D"]), ("C", &["D"])], &[]).orphaned_by_seeds(&[path("A")]),
            set(&["A", "B", "C", "D"])
        );
    }

    #[test]
    fn purge_seed_protected_as_dep() {
        assert_eq!(
            gc(&["R"], &[("R", &["A"]), ("A", &["B"])], &[]).orphaned_by_seeds(&[path("A")]),
            set(&[])
        );
    }

    #[test]
    fn purge_cycle_terminates() {
        assert_eq!(
            gc(&[], &[("A", &["B"]), ("B", &["A"])], &[]).orphaned_by_seeds(&[path("A")]),
            set(&["A", "B"])
        );
    }

    #[test]
    fn empty_graph_empty_seeds() {
        assert!(gc(&[], &[], &[]).orphaned_by_seeds(&[path("X")]).is_empty());
    }

    // ── orphaned_by_seeds (cross-tier) ──────────────────────────────────

    #[test]
    fn purge_cascades_through_layers() {
        // A → L1 (layer); removing A orphans both A and L1.
        let collector = gc_with_tiers(&[], &[("A", &["L1"])], &[], &[("L1", CasTier::Layer)]);
        assert_eq!(collector.orphaned_by_seeds(&[path("A")]), set(&["A", "L1"]));
    }

    #[test]
    fn purge_cascades_through_blobs() {
        // A → B1 (blob); removing A orphans both A and B1.
        let collector = gc_with_tiers(&[], &[("A", &["B1"])], &[], &[("B1", CasTier::Blob)]);
        assert_eq!(collector.orphaned_by_seeds(&[path("A")]), set(&["A", "B1"]));
    }

    #[test]
    fn purge_seed_that_is_root() {
        // A is a root (has live install symlink) AND is a seed being purged.
        // The algorithm must still identify A and its deps as orphaned.
        let collector = gc(&["A"], &[("A", &["B"])], &[]);
        assert_eq!(collector.orphaned_by_seeds(&[path("A")]), set(&["A", "B"]));
    }

    #[test]
    fn purge_seed_that_is_root_with_shared_dep() {
        // A and B are both roots, sharing dep C. Removing A should orphan A only.
        // C survives via B. Old algorithm: bfs({A,B}) - bfs({A,B}) = {} (wrong).
        let collector = gc(&["A", "B"], &[("A", &["C"]), ("B", &["C"])], &[]);
        assert_eq!(collector.orphaned_by_seeds(&[path("A")]), set(&["A"]));
    }

    #[test]
    fn purge_seed_that_is_root_private_dep_orphaned_shared_dep_survives() {
        // A (root, seed) → C (private), A → D (shared with B).
        // Removing A: C orphaned, D survives via B.
        let collector = gc(&["A", "B"], &[("A", &["C", "D"]), ("B", &["D"])], &[]);
        assert_eq!(collector.orphaned_by_seeds(&[path("A")]), set(&["A", "C"]));
    }

    #[test]
    fn purge_shared_layer_survives() {
        // A → L1, B → L1; B is a root. Removing A leaves L1 reachable via B.
        let collector = gc_with_tiers(
            &["B"],
            &[("A", &["L1"]), ("B", &["L1"])],
            &[],
            &[("L1", CasTier::Layer)],
        );
        assert_eq!(collector.orphaned_by_seeds(&[path("A")]), set(&["A"]));
    }
}
