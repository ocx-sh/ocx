// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

mod reachability_graph;

use std::collections::HashSet;
use std::path::PathBuf;

use crate::{file_structure::FileStructure, log, profile::ProfileSnapshot, reference_manager::ReferenceManager};

use reachability_graph::ReachabilityGraph;

/// Garbage collector for the object store.
///
/// Built from the current filesystem state, provides both full GC
/// ([`unreachable_objects`]) and scoped purge ([`orphaned_by_seeds`]).
/// Query methods return sets without side effects; [`delete_objects`]
/// performs the actual filesystem mutations.
pub struct GarbageCollector {
    graph: ReachabilityGraph,
    rm: ReferenceManager,
}

impl GarbageCollector {
    pub async fn build(file_structure: &FileStructure, profile: &ProfileSnapshot) -> crate::Result<Self> {
        let graph = ReachabilityGraph::build(file_structure, profile).await?;
        let rm = ReferenceManager::new(file_structure.clone());
        Ok(Self { graph, rm })
    }

    /// Returns all objects not reachable from any root.
    pub fn unreachable_objects(&self) -> HashSet<PathBuf> {
        let reachable = self.graph.reachable();
        self.graph.all_objects.difference(&reachable).cloned().collect()
    }

    /// Returns objects newly orphaned by removing the given seeds.
    ///
    /// Computes via set difference:
    ///   reachable_if_seeds_installed = bfs(roots ∪ seeds)
    ///   reachable_actual             = bfs(roots)
    ///   orphaned = reachable_if_seeds_installed - reachable_actual
    pub fn orphaned_by_seeds(&self, seeds: &[PathBuf]) -> HashSet<PathBuf> {
        let reachable_actual = self.graph.reachable();
        let reachable_if_installed = self
            .graph
            .bfs(self.graph.roots.iter().cloned().chain(seeds.iter().cloned()));

        reachable_if_installed
            .difference(&reachable_actual)
            .filter(|p| self.graph.all_objects.contains(*p))
            .cloned()
            .collect()
    }

    /// Convenience: combines [`orphaned_by_seeds`] and [`delete_objects`].
    pub async fn purge(&self, obj_dirs: &[PathBuf]) -> crate::Result<Vec<PathBuf>> {
        let targets = self.orphaned_by_seeds(obj_dirs);
        self.delete_objects(&targets, false).await
    }

    /// Deletes the given object directories from disk.
    ///
    /// For each target: re-checks `has_live_refs` as a best-effort guard
    /// against concurrent installs, unlinks dependency forward-refs via
    /// [`ReferenceManager`], then removes the directory.
    ///
    /// Handles `NotFound` errors from `remove_dir_all` gracefully — a
    /// concurrent deletion or external cleanup is not treated as failure.
    pub async fn delete_objects(&self, targets: &HashSet<PathBuf>, dry_run: bool) -> crate::Result<Vec<PathBuf>> {
        if targets.is_empty() {
            return Ok(Vec::new());
        }

        log::debug!(
            "{} {} object(s){}.",
            if dry_run { "Would remove" } else { "Removing" },
            targets.len(),
            if dry_run { " (dry run)" } else { "" },
        );

        let mut removed = Vec::new();
        let mut sorted_targets: Vec<&PathBuf> = targets.iter().collect();
        sorted_targets.sort();

        for target in sorted_targets {
            if dry_run {
                log::info!("Would remove unreferenced object: {}", target.display());
                removed.push(target.clone());
                continue;
            }

            log::info!("Removing unreferenced object: {}", target.display());

            // Remove forward-refs to dependencies before deleting the object.
            let content = target.join("content");
            if let Some(deps) = self.graph.dep_edges.get(target) {
                for dep_dir in deps {
                    let _ = self.rm.unlink_dependency(&content, &dep_dir.join("content"));
                }
            }

            match tokio::fs::remove_dir_all(target).await {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    log::debug!("Object '{}' already removed (concurrent deletion).", target.display());
                }
                Err(e) => return Err(crate::Error::InternalFile(target.clone(), e)),
            }
            removed.push(target.clone());
        }

        log::debug!(
            "{} {} object(s).",
            if dry_run { "Would remove" } else { "Removed" },
            removed.len(),
        );

        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::reachability_graph::tests::{graph, set};
    use super::*;

    fn path(name: &str) -> PathBuf {
        PathBuf::from(format!("/objects/{name}"))
    }

    fn gc(roots: &[&str], edges: &[(&str, &[&str])], extra: &[&str]) -> GarbageCollector {
        GarbageCollector {
            graph: graph(roots, edges, extra),
            rm: dummy_rm(),
        }
    }

    fn dummy_rm() -> ReferenceManager {
        let fs = crate::file_structure::FileStructure::with_root(PathBuf::from("/tmp/dummy"));
        ReferenceManager::new(fs)
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
}
