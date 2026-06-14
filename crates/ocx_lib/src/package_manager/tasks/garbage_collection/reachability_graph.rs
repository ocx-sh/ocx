// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Filesystem-based reachability graph for object store garbage collection.
//!
//! Built from `refs/` (install back-references) and `deps/` (dependency forward-references).
//! Objects with live refs are roots. BFS through `deps/` edges determines reachable objects.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::{
    file_structure::{CasTier, FileStructure},
    log, utility,
};

use super::project_roots::ProjectRootDigests;

/// Maximum concurrent I/O tasks for graph building.
const BUILD_CONCURRENCY: usize = 50;

/// Size ceiling, in bytes, for a blob considered as a possible OCI image-index
/// manifest in [`add_index_retention_edges`]. A blob **larger** than this is
/// skipped without being read (it cannot be a manifest — it is a layer tarball
/// or config); a blob **at or under** this size is read in full and parsed.
///
/// This is a *whole-blob* ceiling, not a read prefix: a candidate index is read
/// completely so a large-but-valid index (many child descriptors / annotations)
/// is never truncated mid-JSON. Truncation would silently drop the
/// `child_leaf_blob → index_blob` retention edge and let GC collect a live
/// parent index blob. 4 MiB is far above any real OCI manifest/index (a few
/// hundred descriptors with annotations is still well under 1 MiB) and far
/// below a layer tarball, so the ceiling separates the two classes cleanly
/// without slurping multi-hundred-MB archives.
///
/// The OCI distribution spec recommends registries cap manifest size at
/// 4 MiB (`distribution` `maxManifestBytes`); matching that ceiling means any
/// manifest a spec-compliant registry would accept is read whole here.
const INDEX_MANIFEST_MAX_BYTES: u64 = 4 * 1024 * 1024;

/// Pre-computed dependency graph with BFS reachability queries.
///
/// Covers all three CAS tiers (packages, layers, blobs) in a single graph.
/// Only packages can be roots and have outgoing edges; layers and blobs are
/// reachable exclusively through package `refs/layers/` and `refs/blobs/` edges.
pub struct ReachabilityGraph {
    pub roots: HashSet<PathBuf>,
    pub edges: HashMap<PathBuf, Vec<PathBuf>>,
    pub all_entries: HashMap<PathBuf, CasTier>,
    /// Maps each package-store path that is a project-registry root to the
    /// `ocx.lock` paths that contributed it. Used by `ocx clean --dry-run`
    /// to populate the `Held By` column. Empty when `project_roots` is `&[]`.
    pub roots_attribution: HashMap<PathBuf, Vec<PathBuf>>,
}

impl ReachabilityGraph {
    /// Scan all three CAS stores, identify roots, build edges.
    ///
    /// Packages are probed for `refs/symlinks/` (roots), `refs/deps/` (package edges),
    /// `refs/layers/` (layer edges), and `refs/blobs/` (blob edges). Layers and blobs
    /// are passive entries — they have no outgoing edges and are reachable only through
    /// package refs.
    ///
    /// `project_roots` supplies additional roots from registered projects' `ocx.lock`
    /// files (Unit 6). Pass `&[]` when project-registry roots are suppressed (e.g.
    /// `ocx clean --force`). See [`adr_clean_project_backlinks.md`].
    pub async fn build(file_structure: &FileStructure, project_roots: &[ProjectRootDigests]) -> crate::Result<Self> {
        // Walk all three stores in parallel.
        let (package_dirs, layer_dirs, blob_dirs) = tokio::try_join!(
            file_structure.packages.list_all(),
            file_structure.layers.list_all(),
            file_structure.blobs.list_all(),
        )?;

        let canon_packages_root = canonicalize_or_keep(file_structure.packages.root());
        let canon_layers_root = canonicalize_or_keep(file_structure.layers.root());
        let canon_blobs_root = canonicalize_or_keep(file_structure.blobs.root());

        // Spawn parallel I/O tasks to probe refs/ for each package.
        let sem = Arc::new(Semaphore::new(BUILD_CONCURRENCY));
        let mut tasks = JoinSet::new();

        let packages_root = Arc::new(canon_packages_root);
        let layers_root = Arc::new(canon_layers_root);
        let blobs_root = Arc::new(canon_blobs_root);

        for pkg in &package_dirs {
            let pkg_dir = canonicalize_or_keep(&pkg.dir);
            let deps_dir = pkg.refs_deps_dir();
            let layers_dir = pkg.refs_layers_dir();
            let blobs_dir = pkg.refs_blobs_dir();
            let pkgs_root = Arc::clone(&packages_root);
            let lyrs_root = Arc::clone(&layers_root);
            let blbs_root = Arc::clone(&blobs_root);
            let sem = Arc::clone(&sem);

            tasks.spawn(async move {
                // `sem` is constructed in this function and outlives every
                // spawned task (each holds an `Arc` clone); it is never closed
                // before all permits release, so `acquire_owned` cannot fail.
                let _permit = sem.acquire_owned().await.expect("semaphore closed");
                let is_root = has_live_refs(&pkg_dir).await;
                let dep_refs = read_refs(&deps_dir, &pkgs_root).await;
                let layer_refs = read_refs(&layers_dir, &lyrs_root).await;
                let blob_refs = read_refs(&blobs_dir, &blbs_root).await;
                let mut all_edges = dep_refs;
                all_edges.extend(layer_refs);
                all_edges.extend(blob_refs);
                (pkg_dir, is_root, all_edges)
            });
        }

        let mut roots = HashSet::new();
        let mut edges = HashMap::new();
        let mut all_entries = HashMap::new();
        let mut roots_attribution: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();

        // Resolve project_roots digests to package-store paths and insert as
        // additional reachability roots alongside live install symlinks. Also
        // build roots_attribution for dry-run attribution reporting ("Held By"
        // column in `ocx clean --dry-run`).
        for project_root in project_roots {
            for pinned in &project_root.digests {
                let pkg_path = file_structure.packages.path(pinned);
                let canon = canonicalize_or_keep(&pkg_path);
                roots.insert(canon.clone());
                roots_attribution
                    .entry(canon)
                    .or_default()
                    .push(project_root.ocx_lock_path.clone());
            }
        }

        while let Some(result) = tasks.join_next().await {
            let (pkg_dir, is_root, pkg_edges) = result.expect("task panicked");

            if is_root {
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

        // Index-manifest retention edges. An OCI image-index manifest blob
        // (the outer multi-platform index) is not referenced by any package's
        // `refs/blobs/` — packages reference only the per-platform leaf
        // manifest they were assembled from. Under V2 per-platform pinning the
        // index digest is never stored in `ocx.lock`, so the index blob has no
        // GC root and would be collected on every `ocx clean`, even though the
        // leaves it ties together are held.
        //
        // Add a `child_leaf_blob → index_blob` edge for every child the index
        // advertises: when any child leaf blob is reachable (held by a rooted
        // package), the BFS reaches the index blob and retains it. This is GC
        // hygiene only — it never roots the index, and a fully-unreferenced
        // index (no reachable child) is still collected.
        add_index_retention_edges(&blob_dirs, &mut edges).await;

        // Propagate project-root attribution transitively through the edge
        // graph so that layers and blobs reachable from a project-root package
        // carry the same `held_by` entries as the root itself.
        //
        // Without this step, `roots_attribution` would only map the top-level
        // package path → lock paths. Layer and blob paths reachable via
        // `refs/layers/` and `refs/blobs/` edges would return `None` from
        // the attribution lookup in `PackageManager::clean`, producing an
        // empty `held_by` in the dry-run report even though those entries are
        // retained by the registry.
        //
        // Single multi-source BFS: enumerate every (root, lock) pair, dedup
        // lock paths into a flat `lock_pool`, and propagate `LockId` indices
        // through the graph. Each node accumulates a `HashSet<LockId>`; we
        // materialise the final `Vec<PathBuf>` in `roots_attribution` once
        // when the BFS completes. This keeps allocations O(E) instead of
        // O(R·E) — the previous per-root BFS cloned the full lock-path list
        // at every visited node.
        if !roots_attribution.is_empty() {
            // Build a deduplicated pool of lock paths and a parallel id map.
            type LockId = u32;
            let mut lock_pool: Vec<PathBuf> = Vec::new();
            let mut lock_index: HashMap<PathBuf, LockId> = HashMap::new();
            let mut intern = |path: &PathBuf| -> LockId {
                if let Some(&id) = lock_index.get(path) {
                    return id;
                }
                let id = lock_pool.len() as LockId;
                lock_pool.push(path.clone());
                lock_index.insert(path.clone(), id);
                id
            };

            // Snapshot the seed (root, lock_id) pairs — `roots_attribution`
            // gets re-read during materialisation so we cannot borrow into
            // it during the BFS.
            let mut seeds: Vec<(PathBuf, LockId)> = Vec::new();
            for (root_path, lock_paths) in &roots_attribution {
                for lock_path in lock_paths {
                    seeds.push((root_path.clone(), intern(lock_path)));
                }
            }

            let mut propagated: HashMap<PathBuf, HashSet<LockId>> = HashMap::new();
            let mut queue: VecDeque<(PathBuf, LockId)> = seeds.iter().cloned().collect();

            while let Some((current, lock_id)) = queue.pop_front() {
                let entry = propagated.entry(current.clone()).or_default();
                if !entry.insert(lock_id) {
                    // This (node, lock) pair was already enqueued — skip.
                    continue;
                }
                if let Some(neighbors) = edges.get(&current) {
                    for n in neighbors {
                        queue.push_back((n.clone(), lock_id));
                    }
                }
            }

            // Materialise lock ids back into owned `PathBuf` values. Skip the
            // seed roots themselves (already attributed verbatim from the
            // seed map) so we do not double-append their lock paths.
            let seed_roots: HashSet<PathBuf> = seeds.into_iter().map(|(p, _)| p).collect();
            for (node, ids) in propagated {
                if seed_roots.contains(&node) {
                    continue;
                }
                let bucket = roots_attribution.entry(node).or_default();
                for id in ids {
                    bucket.push(lock_pool[id as usize].clone());
                }
            }
        }

        Ok(Self {
            roots,
            edges,
            all_entries,
            roots_attribution,
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

/// Add `child_leaf_blob_dir → index_blob_dir` retention edges for every OCI
/// image-index manifest blob in the store.
///
/// The index blob lives at `{blobs_root}/{registry_slug}/{algo}/{2hex}/{30hex}`;
/// each child manifest the index advertises lives under the **same** registry
/// slug at its own digest shard. Reading the index blob and resolving each
/// child to its on-disk blob dir lets the GC retain the index when any child
/// leaf is reachable (so a normal `ocx lock` + `ocx pull` leaves no orphan
/// index blob), without ever storing the index digest in `ocx.lock`.
///
/// Best-effort: unreadable or non-manifest blobs are skipped silently (a blob
/// store holds layer archives, configs, and leaf manifests too).
async fn add_index_retention_edges(
    blob_dirs: &[crate::file_structure::BlobDir],
    edges: &mut HashMap<PathBuf, Vec<PathBuf>>,
) {
    use crate::oci;

    for blob in blob_dirs {
        // The registry slug is three levels up from the digest-suffix dir:
        // .../{registry_slug}/{algo}/{2hex}/{30hex}.
        let Some(registry_root) = blob.dir.parent().and_then(|p| p.parent()).and_then(|p| p.parent()) else {
            continue;
        };

        // Read the blob in full when it is small enough to be a manifest, or
        // skip it without reading when it exceeds the manifest ceiling (a layer
        // tarball or config). A candidate index is never truncated, so a
        // large-but-valid index cannot lose its retention edge.
        let Some(bytes) = read_manifest_candidate_blob(&blob.data()).await else {
            continue;
        };
        let Ok(oci::Manifest::ImageIndex(index)) = serde_json::from_slice::<oci::Manifest>(&bytes) else {
            continue;
        };

        let index_dir = canonicalize_or_keep(&blob.dir);
        for entry in &index.manifests {
            let Ok(child_digest) = oci::Digest::try_from(entry.digest.as_str()) else {
                continue;
            };
            let child_dir =
                canonicalize_or_keep(&registry_root.join(crate::file_structure::cas_shard_path(&child_digest)));
            // Reverse edge: a reachable child leaf blob retains its parent index.
            edges.entry(child_dir).or_default().push(index_dir.clone());
        }
    }
}

/// Read a blob **in full** when it could be an OCI manifest, or skip it when it
/// is too large to be one, for the image-index probe in
/// [`add_index_retention_edges`].
///
/// A blob whose size exceeds [`INDEX_MANIFEST_MAX_BYTES`] is a layer tarball or
/// config, not a manifest — return `None` without reading it (so a
/// multi-hundred-MB archive is never slurped into memory). A blob at or under
/// the ceiling is read completely; a candidate index is therefore **never
/// truncated**, so a large-but-valid index (many descriptors / annotations)
/// keeps its `child_leaf_blob → index_blob` retention edge instead of being
/// mis-classified as a non-manifest and silently collected.
///
/// `metadata().len()` is the size authority. The bounded `take(MAX + 1)` read
/// is a defence-in-depth guard for synthetic files whose metadata reports 0 but
/// whose read is unbounded (procfs, pipes) — mirrors the lock loader's pattern;
/// a blob that grows past the ceiling between the stat and the read is dropped
/// rather than partially parsed.
///
/// Returns `None` on any I/O error (best-effort: an unreadable blob is simply
/// not treated as an index).
async fn read_manifest_candidate_blob(path: &Path) -> Option<Vec<u8>> {
    use tokio::io::AsyncReadExt;

    let file = tokio::fs::File::open(path).await.ok()?;
    // Stat first: skip a blob that is too large to be a manifest without
    // reading any of its bytes.
    if file.metadata().await.ok()?.len() > INDEX_MANIFEST_MAX_BYTES {
        return None;
    }

    // Read the whole blob, bounded by `MAX + 1` so a synthetic 0-length-metadata
    // file (procfs/pipe) cannot read unbounded. If the read reaches the bound,
    // the blob is larger than the ceiling after all — drop it (a manifest never
    // exceeds the ceiling).
    let mut buf = Vec::new();
    file.take(INDEX_MANIFEST_MAX_BYTES + 1)
        .read_to_end(&mut buf)
        .await
        .ok()?;
    if buf.len() as u64 > INDEX_MANIFEST_MAX_BYTES {
        return None;
    }
    Some(buf)
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
            roots_attribution: HashMap::new(),
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

    // ── index-manifest retention (child_leaf_blob → index_blob edge) ────────

    #[test]
    fn index_blob_held_when_child_leaf_reachable() {
        // A rooted package references its leaf manifest blob; the leaf carries a
        // retention edge to the parent index blob. The index must be reachable.
        let g = graph_with_tiers(
            &["pkg"],
            &[("pkg", &["leaf_blob"]), ("leaf_blob", &["index_blob"])],
            &[],
            &[("leaf_blob", CasTier::Blob), ("index_blob", CasTier::Blob)],
        );
        assert_eq!(g.reachable(), set(&["pkg", "leaf_blob", "index_blob"]));
    }

    #[test]
    fn index_blob_collected_when_no_child_reachable() {
        // No package references the leaf: neither the leaf nor the index blob
        // is reachable. A fully-unreferenced index is still collectable — the
        // retention edge only holds the index when a child leaf is held.
        let g = graph_with_tiers(
            &["pkg"],
            &[("leaf_blob", &["index_blob"])],
            &["leaf_blob", "index_blob"],
            &[("leaf_blob", CasTier::Blob), ("index_blob", CasTier::Blob)],
        );
        assert_eq!(g.reachable(), set(&["pkg"]));
    }

    // ── index-manifest probe (F5: large index must not be truncated) ────────

    /// F5 regression: an image-index manifest **larger than the old 16 KiB
    /// probe bound** must still produce its `child_leaf_blob → index_blob`
    /// retention edge. The previous code read only a 16 KiB prefix, so a large
    /// valid index parsed as truncated JSON → was skipped → its parent index
    /// blob lost the retention edge and GC could delete a live index.
    ///
    /// Drives [`add_index_retention_edges`] against a real on-disk blob holding
    /// a > 16 KiB valid OCI image index (padded with many annotated child
    /// descriptors). The retention edge for an advertised child must exist.
    #[tokio::test]
    async fn large_index_manifest_still_retains_via_child_edge() {
        use crate::file_structure::{BlobDir, cas_shard_path};
        use crate::oci;

        let tmp = tempfile::tempdir().expect("tempdir");
        // Blob layout: {root}/{registry_slug}/{algo}/{2hex}/{30hex}/data — the
        // index lives at its own digest shard; `add_index_retention_edges`
        // resolves children under the SAME registry slug.
        let registry_root = tmp.path().join("registry_slug");

        // A child leaf digest the index advertises — its retention edge is the
        // assertion target.
        let child_hex = "a".repeat(64);
        let child_digest = oci::Digest::Sha256(child_hex.clone());

        // Build a valid OCI image index whose serialized JSON exceeds the old
        // 16 KiB bound, by padding with many annotated child descriptors. The
        // first child is the one we assert the edge for.
        let mut manifests = vec![oci::ImageIndexEntry {
            media_type: "application/vnd.oci.image.manifest.v1+json".to_string(),
            digest: format!("sha256:{child_hex}"),
            size: 100,
            platform: None,
            annotations: Some(std::collections::BTreeMap::from([(
                "org.opencontainers.image.title".to_string(),
                "x".repeat(256),
            )])),
        }];
        for i in 0..200 {
            manifests.push(oci::ImageIndexEntry {
                media_type: "application/vnd.oci.image.manifest.v1+json".to_string(),
                digest: format!("sha256:{:064x}", i + 1),
                size: 100,
                platform: None,
                annotations: Some(std::collections::BTreeMap::from([(
                    "org.opencontainers.image.title".to_string(),
                    "y".repeat(256),
                )])),
            });
        }
        let index = oci::Manifest::ImageIndex(oci::ImageIndex {
            schema_version: 2,
            media_type: Some("application/vnd.oci.image.index.v1+json".to_string()),
            artifact_type: None,
            manifests,
            annotations: None,
        });
        let index_json = serde_json::to_vec(&index).expect("serialize index");
        assert!(
            index_json.len() > 16 * 1024,
            "the index fixture must exceed the old 16 KiB bound to exercise the regression; got {} bytes",
            index_json.len()
        );

        // Write the index blob at an arbitrary digest shard under the registry.
        let index_digest = oci::Digest::Sha256("c".repeat(64));
        let index_blob_dir = registry_root.join(cas_shard_path(&index_digest));
        tokio::fs::create_dir_all(&index_blob_dir)
            .await
            .expect("mkdir index dir");
        tokio::fs::write(index_blob_dir.join("data"), &index_json)
            .await
            .expect("write index blob");

        let blob_dirs = vec![BlobDir {
            dir: index_blob_dir.clone(),
        }];
        let mut edges: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
        super::add_index_retention_edges(&blob_dirs, &mut edges).await;

        // The advertised child leaf's blob dir must carry an edge to the index
        // blob dir — proving the large index was parsed (not truncated/skipped).
        let child_dir = super::canonicalize_or_keep(&registry_root.join(cas_shard_path(&child_digest)));
        let index_dir = super::canonicalize_or_keep(&index_blob_dir);
        let child_edges = edges
            .get(&child_dir)
            .expect("a large valid index must still create the child_leaf_blob → index_blob retention edge");
        assert!(
            child_edges.contains(&index_dir),
            "the child leaf must retain its parent index blob; child_edges={child_edges:?}, want {index_dir:?}"
        );
    }
}
