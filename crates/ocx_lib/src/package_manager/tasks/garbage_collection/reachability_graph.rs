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

use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::{file_structure::FileStructure, log, profile::ProfileSnapshot};

/// Maximum concurrent I/O tasks for graph building.
const BUILD_CONCURRENCY: usize = 50;

/// Pre-computed dependency graph with BFS reachability queries.
pub struct ReachabilityGraph {
    pub roots: HashSet<PathBuf>,
    pub dep_edges: HashMap<PathBuf, Vec<PathBuf>>,
    pub all_objects: HashSet<PathBuf>,
}

impl ReachabilityGraph {
    /// Scan the object store, identify roots, build dependency edges.
    ///
    /// Reads `refs/` and `deps/` for all objects in parallel using a
    /// semaphore-bounded [`JoinSet`] for concurrent I/O across directories.
    pub async fn build(file_structure: &FileStructure, profile: &ProfileSnapshot) -> crate::Result<Self> {
        let object_dirs = file_structure.objects.list_all().await?;

        let canon_store_root = Arc::new(dunce::canonicalize(file_structure.objects.root()).unwrap_or_else(|e| {
            log::debug!(
                "Cannot canonicalize store root {}: {e}",
                file_structure.objects.root().display()
            );
            file_structure.objects.root().clone()
        }));

        // Resolve profile content-mode entries to object store paths (forward lookup).
        let profile_roots: HashSet<PathBuf> = profile
            .content_digests()
            .into_iter()
            .filter_map(|id| {
                let pinned = crate::oci::PinnedIdentifier::try_from(id.clone()).ok()?;
                let obj_path = file_structure.objects.path(&pinned);
                Some(dunce::canonicalize(&obj_path).unwrap_or_else(|e| {
                    log::debug!("Cannot canonicalize profile root {}: {e}", obj_path.display());
                    obj_path
                }))
            })
            .collect();

        // Spawn parallel I/O tasks to probe refs/ and deps/ for each object.
        let sem = Arc::new(Semaphore::new(BUILD_CONCURRENCY));
        let mut tasks = JoinSet::new();

        for obj in &object_dirs {
            let obj_dir = dunce::canonicalize(&obj.dir).unwrap_or_else(|e| {
                log::debug!("Cannot canonicalize object dir {}: {e}", obj.dir.display());
                obj.dir.clone()
            });
            let deps_dir = obj.deps_dir();
            let store_root = Arc::clone(&canon_store_root);
            let sem = Arc::clone(&sem);

            tasks.spawn(async move {
                let _permit = sem.acquire_owned().await.expect("semaphore closed");
                let is_root = has_live_refs(&obj_dir).await;
                let deps = read_deps(&deps_dir, &store_root).await;
                (obj_dir, is_root, deps)
            });
        }

        // Collect results and assemble the graph.
        let mut roots = HashSet::new();
        let mut dep_edges = HashMap::new();
        let mut all_objects = HashSet::new();

        while let Some(result) = tasks.join_next().await {
            let (obj_dir, is_root, deps) = result.expect("task panicked");

            if is_root || profile_roots.contains(&obj_dir) {
                roots.insert(obj_dir.clone());
            }

            dep_edges.insert(obj_dir.clone(), deps);
            all_objects.insert(obj_dir);
        }

        Ok(Self {
            roots,
            dep_edges,
            all_objects,
        })
    }

    /// BFS from the given starting set through `dep_edges`.
    ///
    /// Starting paths are canonicalized to match the graph's internal representation.
    /// Internal edges are already canonical from [`build()`].
    pub fn bfs(&self, starts: impl IntoIterator<Item = PathBuf>) -> HashSet<PathBuf> {
        let mut reachable = HashSet::new();
        let mut queue: VecDeque<PathBuf> = starts
            .into_iter()
            .map(|p| {
                dunce::canonicalize(&p).unwrap_or_else(|e| {
                    log::debug!("Cannot canonicalize BFS start {}: {e}", p.display());
                    p
                })
            })
            .collect();

        while let Some(dir) = queue.pop_front() {
            if !reachable.insert(dir.clone()) {
                continue;
            }
            if let Some(deps) = self.dep_edges.get(&dir) {
                queue.extend(deps.iter().cloned());
            }
        }

        reachable
    }

    /// BFS from the real roots.
    pub fn reachable(&self) -> HashSet<PathBuf> {
        self.bfs(self.roots.iter().cloned())
    }
}

/// Reads dependency forward-refs from `deps/`, returning canonicalized object directories.
/// Symlinks pointing outside `store_root` are skipped (defence-in-depth).
async fn read_deps(deps_dir: &Path, store_root: &Path) -> Vec<PathBuf> {
    let mut deps = Vec::new();
    let Ok(mut entries) = tokio::fs::read_dir(deps_dir).await else {
        return deps;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let entry_path = entry.path();
        if crate::symlink::is_link(&entry_path)
            && let Ok(dep_content) = tokio::fs::read_link(&entry_path).await
            && let Some(dep_obj_dir) = dep_content.parent()
        {
            let canon_dep = dunce::canonicalize(dep_obj_dir).unwrap_or_else(|e| {
                log::debug!("Cannot canonicalize dep path {}: {e}", dep_obj_dir.display());
                dep_obj_dir.to_path_buf()
            });
            if !canon_dep.starts_with(store_root) {
                log::warn!(
                    "Skipping deps/ symlink pointing outside object store: {}",
                    dep_content.display()
                );
                continue;
            }
            deps.push(canon_dep);
        }
    }
    deps
}

/// Returns true if the object directory has any live install refs.
///
/// A ref is live if its symlink target still exists. Broken refs (target deleted
/// by user or crashed uninstall) do not protect the object from collection.
async fn has_live_refs(obj_dir: &Path) -> bool {
    let refs_dir = obj_dir.join("refs");
    let Ok(mut entries) = tokio::fs::read_dir(&refs_dir).await else {
        return false;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if crate::symlink::is_link(&path)
            && let Ok(target) = tokio::fs::read_link(&path).await
            && tokio::fs::try_exists(&target).await.unwrap_or(false)
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
pub mod tests {
    use std::collections::{HashMap, HashSet};
    use std::path::PathBuf;

    use super::ReachabilityGraph;

    fn path(name: &str) -> PathBuf {
        PathBuf::from(format!("/objects/{name}"))
    }

    pub fn graph(roots: &[&str], edges: &[(&str, &[&str])], extra_objects: &[&str]) -> ReachabilityGraph {
        let roots: HashSet<PathBuf> = roots.iter().map(|n| path(n)).collect();
        let dep_edges: HashMap<PathBuf, Vec<PathBuf>> = edges
            .iter()
            .map(|(from, tos)| (path(from), tos.iter().map(|t| path(t)).collect()))
            .collect();

        let mut all_objects: HashSet<PathBuf> = dep_edges.keys().cloned().collect();
        for deps in dep_edges.values() {
            all_objects.extend(deps.iter().cloned());
        }
        for name in extra_objects {
            all_objects.insert(path(name));
        }

        ReachabilityGraph {
            roots,
            dep_edges,
            all_objects,
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
}
