// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Filesystem-based reachability graph for object store garbage collection.
//!
//! Built from `refs/` (install back-references) and `deps/` (dependency forward-references).
//! Objects with live refs or profile content-mode references are roots. BFS through
//! `deps/` edges determines reachable objects.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::stream::{self, StreamExt};

use crate::{
    file_structure::{CasTier, FileStructure},
    log,
    profile::ProfileSnapshot,
    utility,
};

/// Maximum concurrent I/O tasks for graph building.
const BUILD_CONCURRENCY: usize = 50;

/// Pre-computed dependency graph with BFS reachability queries.
///
/// Covers all three CAS tiers (packages, layers, blobs) in a single graph.
/// Only packages can be roots and have outgoing edges; layers and blobs are
/// reachable exclusively through package `refs/layers/` and `refs/blobs/` edges.
pub struct ReachabilityGraph {
    pub roots: HashSet<PathBuf>,
    pub edges: HashMap<PathBuf, Vec<PathBuf>>,
    pub all_entries: HashMap<PathBuf, CasTier>,
}

impl ReachabilityGraph {
    /// Scan all three CAS stores, identify roots, build edges.
    ///
    /// Packages are probed for `refs/symlinks/` (roots), `refs/deps/` (package edges),
    /// `refs/layers/` (layer edges), and `refs/blobs/` (blob edges). Layers and blobs
    /// are passive entries — they have no outgoing edges and are reachable only through
    /// package refs.
    pub async fn build(file_structure: &FileStructure, profile: &ProfileSnapshot) -> crate::Result<Self> {
        // Walk all three stores in parallel.
        let (package_dirs, layer_dirs, blob_dirs) = tokio::try_join!(
            file_structure.packages.list_all(),
            file_structure.layers.list_all(),
            file_structure.blobs.list_all(),
        )?;

        let canon_packages_root = canonicalize_or_keep(file_structure.packages.root());
        let canon_layers_root = canonicalize_or_keep(file_structure.layers.root());
        let canon_blobs_root = canonicalize_or_keep(file_structure.blobs.root());

        // Resolve profile content-mode entries to package store paths (forward lookup).
        let profile_roots: HashSet<PathBuf> = profile
            .content_digests()
            .into_iter()
            .filter_map(|id| {
                let pinned = crate::oci::PinnedIdentifier::try_from(id.clone()).ok()?;
                let pkg_path = file_structure.packages.path(&pinned);
                Some(canonicalize_or_keep(&pkg_path))
            })
            .collect();

        // Probe refs/ for each package with bounded concurrency. The same
        // `.buffered` pattern is used by the pull-side `extract_layers` /
        // `setup_dependencies` and the push-side `push_layers_to_repository`.
        let packages_root = Arc::new(canon_packages_root);
        let layers_root = Arc::new(canon_layers_root);
        let blobs_root = Arc::new(canon_blobs_root);

        let collected: Vec<(PathBuf, bool, Vec<PathBuf>)> = stream::iter(package_dirs.iter())
            .map(|pkg| {
                let pkg_dir = canonicalize_or_keep(&pkg.dir);
                let deps_dir = pkg.refs_deps_dir();
                let layers_dir = pkg.refs_layers_dir();
                let blobs_dir = pkg.refs_blobs_dir();
                let pkgs_root = Arc::clone(&packages_root);
                let lyrs_root = Arc::clone(&layers_root);
                let blbs_root = Arc::clone(&blobs_root);
                async move {
                    let is_root = has_live_refs(&pkg_dir).await;
                    let dep_refs = read_refs(&deps_dir, &pkgs_root).await;
                    let layer_refs = read_refs(&layers_dir, &lyrs_root).await;
                    let blob_refs = read_refs(&blobs_dir, &blbs_root).await;
                    let mut all_edges = dep_refs;
                    all_edges.extend(layer_refs);
                    all_edges.extend(blob_refs);
                    (pkg_dir, is_root, all_edges)
                }
            })
            .buffered(BUILD_CONCURRENCY)
            .collect()
            .await;

        let mut roots = HashSet::new();
        let mut edges = HashMap::new();
        let mut all_entries = HashMap::new();

        for (pkg_dir, is_root, pkg_edges) in collected {
            if is_root || profile_roots.contains(&pkg_dir) {
                roots.insert(pkg_dir.clone());
            }
            edges.insert(pkg_dir.clone(), pkg_edges);
            all_entries.insert(pkg_dir, CasTier::Package);
        }

        // Register layers and blobs as passive entries (no edges, no roots).
        for layer in &layer_dirs {
            let layer_dir = canonicalize_or_keep(&layer.dir);
            all_entries.insert(layer_dir, CasTier::Layer);
        }
        for blob in &blob_dirs {
            let blob_dir = canonicalize_or_keep(&blob.dir);
            all_entries.insert(blob_dir, CasTier::Blob);
        }

        Ok(Self {
            roots,
            edges,
            all_entries,
        })
    }

    /// BFS from the given starting set through all edge types (deps, layers, blobs).
    ///
    /// Starting paths are canonicalized to match the graph's internal representation.
    /// Internal edges are already canonical from [`build()`].
    pub fn bfs(&self, starts: impl IntoIterator<Item = PathBuf>) -> HashSet<PathBuf> {
        let mut reachable = HashSet::new();
        let mut queue: VecDeque<PathBuf> = starts.into_iter().map(|p| canonicalize_or_keep(&p)).collect();

        while let Some(dir) = queue.pop_front() {
            if !reachable.insert(dir.clone()) {
                continue;
            }
            if let Some(neighbors) = self.edges.get(&dir) {
                queue.extend(neighbors.iter().cloned());
            }
        }

        reachable
    }

    /// BFS from the real roots.
    pub fn reachable(&self) -> HashSet<PathBuf> {
        self.bfs(self.roots.iter().cloned())
    }
}

/// Reads forward-refs from a refs subdirectory (deps/, layers/, or blobs/).
///
/// Each symlink target is expected to be a content path inside `store_root`.
/// The parent of the target (the CAS entry directory) is returned.
/// Symlinks pointing outside `store_root` are skipped (defence-in-depth).
async fn read_refs(refs_dir: &Path, store_root: &Path) -> Vec<PathBuf> {
    let mut targets = Vec::new();
    let Ok(mut entries) = tokio::fs::read_dir(refs_dir).await else {
        return targets;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let entry_path = entry.path();
        if crate::symlink::is_link(&entry_path)
            && let Ok(ref_target) = tokio::fs::read_link(&entry_path).await
            && let Some(entry_dir) = ref_target.parent()
        {
            let canon = canonicalize_or_keep(entry_dir);
            if !canon.starts_with(store_root) {
                log::warn!(
                    "Skipping refs/ symlink pointing outside store: {}",
                    ref_target.display()
                );
                continue;
            }
            targets.push(canon);
        }
    }
    targets
}

/// Returns true if the package directory has any live install refs.
///
/// A ref is live if its symlink target still exists. Broken refs (target deleted
/// by user or crashed uninstall) do not protect the package from collection.
async fn has_live_refs(pkg_dir: &Path) -> bool {
    let refs_dir = pkg_dir.join("refs").join("symlinks");
    let Ok(mut entries) = tokio::fs::read_dir(&refs_dir).await else {
        return false;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if crate::symlink::is_link(&path)
            && let Ok(target) = tokio::fs::read_link(&path).await
            && utility::fs::path_exists_lossy(&target).await
        {
            return true;
        }
    }
    false
}

/// Canonicalize a path, falling back to the original on error.
fn canonicalize_or_keep(path: &Path) -> PathBuf {
    dunce::canonicalize(path).unwrap_or_else(|e| {
        log::debug!("Cannot canonicalize {}: {e}", path.display());
        path.to_path_buf()
    })
}

#[cfg(test)]
pub mod tests {
    use std::collections::{HashMap, HashSet};
    use std::path::PathBuf;

    use super::ReachabilityGraph;
    use crate::file_structure::CasTier;

    fn path(name: &str) -> PathBuf {
        PathBuf::from(format!("/objects/{name}"))
    }

    /// Builds a test graph where all entries in `edges` and `extra_objects` are packages.
    pub fn graph(roots: &[&str], edges: &[(&str, &[&str])], extra_objects: &[&str]) -> ReachabilityGraph {
        graph_with_tiers(roots, edges, extra_objects, &[])
    }

    /// Builds a test graph with explicit tier overrides.
    ///
    /// `tier_overrides` maps entry names to their tier; entries not listed default to `Package`.
    pub fn graph_with_tiers(
        roots: &[&str],
        edges: &[(&str, &[&str])],
        extra_objects: &[&str],
        tier_overrides: &[(&str, CasTier)],
    ) -> ReachabilityGraph {
        let roots: HashSet<PathBuf> = roots.iter().map(|n| path(n)).collect();
        let edges_map: HashMap<PathBuf, Vec<PathBuf>> = edges
            .iter()
            .map(|(from, tos)| (path(from), tos.iter().map(|t| path(t)).collect()))
            .collect();

        let tier_map: HashMap<&str, CasTier> = tier_overrides.iter().copied().collect();

        let mut all_entries: HashMap<PathBuf, CasTier> = HashMap::new();
        for key in edges_map.keys() {
            let name = key.file_name().and_then(|n| n.to_str()).unwrap_or("");
            all_entries.insert(key.clone(), tier_map.get(name).copied().unwrap_or(CasTier::Package));
        }
        for targets in edges_map.values() {
            for t in targets {
                let name = t.file_name().and_then(|n| n.to_str()).unwrap_or("");
                all_entries
                    .entry(t.clone())
                    .or_insert_with(|| tier_map.get(name).copied().unwrap_or(CasTier::Package));
            }
        }
        for name in extra_objects {
            all_entries
                .entry(path(name))
                .or_insert_with(|| tier_map.get(*name).copied().unwrap_or(CasTier::Package));
        }

        ReachabilityGraph {
            roots,
            edges: edges_map,
            all_entries,
        }
    }

    pub fn set(names: &[&str]) -> HashSet<PathBuf> {
        names.iter().map(|n| path(n)).collect()
    }

    // ── reachable ───────────────────────────────────────────────────────

    #[test]
    fn reachable_single_root_with_chain() {
        let g = graph(&["A"], &[("A", &["B"]), ("B", &["C"])], &[]);
        assert_eq!(g.reachable(), set(&["A", "B", "C"]));
    }

    #[test]
    fn bfs_handles_cycle() {
        let g = graph(&["A"], &[("A", &["B"]), ("B", &["A"])], &[]);
        assert_eq!(g.reachable(), set(&["A", "B"]));
    }

    #[test]
    fn empty_graph_reachable_is_empty() {
        let g = graph(&[], &[], &[]);
        assert!(g.reachable().is_empty());
    }

    // ── cross-tier reachability ─────────────────────────────────────────

    #[test]
    fn bfs_follows_layer_edges() {
        let g = graph_with_tiers(&["A"], &[("A", &["L1"])], &[], &[("L1", CasTier::Layer)]);
        assert_eq!(g.reachable(), set(&["A", "L1"]));
    }

    #[test]
    fn bfs_follows_blob_edges() {
        let g = graph_with_tiers(&["A"], &[("A", &["B1"])], &[], &[("B1", CasTier::Blob)]);
        assert_eq!(g.reachable(), set(&["A", "B1"]));
    }

    #[test]
    fn bfs_follows_mixed_edges() {
        // A → dep D, layer L, blob B
        let g = graph_with_tiers(
            &["A"],
            &[("A", &["D", "L", "B"])],
            &[],
            &[("L", CasTier::Layer), ("B", CasTier::Blob)],
        );
        assert_eq!(g.reachable(), set(&["A", "D", "L", "B"]));
    }

    #[test]
    fn unreferenced_layer_not_reachable() {
        let g = graph_with_tiers(&["A"], &[], &["orphan_layer"], &[("orphan_layer", CasTier::Layer)]);
        assert_eq!(g.reachable(), set(&["A"]));
    }

    #[test]
    fn unreferenced_blob_not_reachable() {
        let g = graph_with_tiers(&["A"], &[], &["orphan_blob"], &[("orphan_blob", CasTier::Blob)]);
        assert_eq!(g.reachable(), set(&["A"]));
    }
}
