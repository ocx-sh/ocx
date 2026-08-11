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
    file_structure::{CasTier, FileStructure, ShimDir},
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
/// Covers all four store tiers (packages, layers, blobs, shims) in a single
/// graph. Packages and shims carry outgoing edges; layers and blobs are passive,
/// reachable exclusively through a package's or shim's `refs/layers/` and
/// `refs/blobs/` edges. A shim is rooted only by a lock pin — it has no incoming
/// edge, because a shim exists precisely when the package it stands in for does
/// not (plan contract C-014).
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
    /// Scan all four stores, identify roots, build edges.
    ///
    /// Packages are probed for `refs/symlinks/` (roots), `refs/deps/` (package edges),
    /// `refs/layers/` (layer edges), and `refs/blobs/` (blob edges). Shims are probed
    /// for `refs/blobs/` only — see [`shim_entries`]. Layers and blobs are passive
    /// entries: no outgoing edges, reachable only through a package's or shim's refs.
    ///
    /// `project_roots` supplies additional roots from registered projects' `ocx.lock`
    /// files (Unit 6), rooted through [`extend_lock_pinned_roots`] once every tier has
    /// been walked. Pass `&[]` when project-registry roots are suppressed (e.g.
    /// `ocx clean --force`) — which, since a shim is held by nothing but a lock pin,
    /// collects every shim in the store (plan contract C-014, F-3). See
    /// [`adr_clean_project_backlinks.md`].
    pub async fn build(file_structure: &FileStructure, project_roots: &[ProjectRootDigests]) -> crate::Result<Self> {
        // Walk all four stores in parallel.
        let (package_dirs, layer_dirs, blob_dirs, shim_dirs) = tokio::try_join!(
            file_structure.packages.list_all(),
            file_structure.layers.list_all(),
            file_structure.blobs.list_all(),
            file_structure.shims.list_all(),
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

        // Register shims with the PACKAGE shape — entry *and* edges — never the
        // passive shape above. A shim directory's `refs/blobs/` links are the
        // only thing holding a deferred tool's closure config blobs, and they
        // are where a deferred consumer reads its env carriers from (C-014's
        // F-1 correction; C-020).
        for (shim_dir, blob_refs) in shim_entries(&shim_dirs, blobs_root.as_path()).await {
            edges.insert(shim_dir.clone(), blob_refs);
            all_entries.insert(shim_dir, CasTier::Shim);
        }

        // Root every lock-pinned identity in whichever tiers hold it. This runs
        // *below* the drain and the passive loops because both insertions are
        // guarded on `all_entries` membership, which is only complete here.
        extend_lock_pinned_roots(
            file_structure,
            project_roots,
            &all_entries,
            &mut roots,
            &mut roots_attribution,
        );

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
///
/// The per-blob read+parse is fanned out across the same bounded-parallel
/// pattern the package walk in [`ReachabilityGraph::build`] uses (a [`JoinSet`]
/// gated by a shared [`Semaphore`] with [`BUILD_CONCURRENCY`] permits). Each
/// task carries its `blob_dirs` index; results are reassembled in input order
/// before edges are appended, so the resulting `edges` map is identical to the
/// previous serial pass (and identical run-to-run despite completion-order
/// `join_next`).
async fn add_index_retention_edges(
    blob_dirs: &[crate::file_structure::BlobDir],
    edges: &mut HashMap<PathBuf, Vec<PathBuf>>,
) {
    let sem = Arc::new(Semaphore::new(BUILD_CONCURRENCY));
    let mut tasks = JoinSet::new();

    for (order, blob) in blob_dirs.iter().enumerate() {
        // The registry slug is three levels up from the digest-suffix dir:
        // .../{registry_slug}/{algo}/{2hex}/{30hex}.
        let Some(registry_root) = blob.dir.parent().and_then(|p| p.parent()).and_then(|p| p.parent()) else {
            continue;
        };
        let registry_root = registry_root.to_path_buf();
        let data_path = blob.data();
        let blob_dir = blob.dir.clone();
        let sem = Arc::clone(&sem);

        tasks.spawn(async move {
            // `sem` is constructed in this function and outlives every spawned
            // task (each holds an `Arc` clone); it is never closed before all
            // permits release, so `acquire_owned` cannot fail.
            let _permit = sem.acquire_owned().await.expect("semaphore closed");
            let pairs = index_retention_pairs(&data_path, &blob_dir, &registry_root).await;
            (order, pairs)
        });
    }

    // Collect completion-order results, then restore `blob_dirs` order so the
    // appended edges match the serial pass byte-for-byte (the BFS only reads
    // set membership, but deterministic ordering keeps the graph identical
    // run-to-run, as required by quality-rust.md for `JoinSet` consumers).
    let mut collected: Vec<(usize, Vec<(PathBuf, PathBuf)>)> = Vec::with_capacity(blob_dirs.len());
    while let Some(result) = tasks.join_next().await {
        collected.push(result.expect("task panicked"));
    }
    collected.sort_by_key(|(order, _)| *order);

    for (_, pairs) in collected {
        for (child_dir, index_dir) in pairs {
            // Reverse edge: a reachable child leaf blob retains its parent index.
            edges.entry(child_dir).or_default().push(index_dir);
        }
    }
}

/// Read one candidate blob and, if it is an OCI image-index manifest, resolve
/// every advertised child to its `(child_blob_dir, index_blob_dir)` retention
/// pair.
///
/// Returns an empty vec when the blob is unreadable, too large to be a manifest,
/// or not an image index — the same best-effort skip the serial pass performed.
/// Pairs are emitted in the index's `manifests` order so the caller, appending
/// them in `blob_dirs` order, reproduces the serial edge layout exactly.
async fn index_retention_pairs(data_path: &Path, blob_dir: &Path, registry_root: &Path) -> Vec<(PathBuf, PathBuf)> {
    use crate::oci;

    // Read the blob in full when it is small enough to be a manifest, or skip
    // it without reading when it exceeds the manifest ceiling (a layer tarball
    // or config). A candidate index is never truncated, so a large-but-valid
    // index cannot lose its retention edge.
    let Some(bytes) = read_manifest_candidate_blob(data_path).await else {
        return Vec::new();
    };
    let Ok(oci::Manifest::ImageIndex(index)) = serde_json::from_slice::<oci::Manifest>(&bytes) else {
        return Vec::new();
    };

    let index_dir = canonicalize_or_keep(blob_dir);
    let mut pairs = Vec::with_capacity(index.manifests.len());
    for entry in &index.manifests {
        let Ok(child_digest) = oci::Digest::try_from(entry.digest.as_str()) else {
            continue;
        };
        let child_dir = canonicalize_or_keep(&registry_root.join(crate::file_structure::cas_shard_path(&child_digest)));
        pairs.push((child_dir, index_dir.clone()));
    }
    pairs
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

/// Collects the shim tier's contribution to the graph: one entry per published
/// shim directory, paired with the `refs/blobs/` forward-refs it carries
/// (plan contract C-014, [#302](https://github.com/ocx-sh/ocx/issues/302)).
///
/// **The shim tier is edge-bearing, not passive.** Layers and blobs are
/// registered by the passive-entry loop in [`ReachabilityGraph::build`] because
/// they have no outgoing references. A shim directory does have them: its
/// [`refs/blobs/`](ShimDir::refs_blobs_dir) links are the only thing keeping a
/// deferred tool's closure config blobs off the unreachable set, and the only
/// place a consumer can read that tool's env carriers from, since no package
/// directory exists for it (plan contract C-020). Registering a shim the way a
/// layer is registered would root the shim and collect every blob it needs.
///
/// Returned paths are canonical, matching the graph's keying; each edge target
/// is the blob **entry directory** ([`read_refs`] takes the symlink target's
/// parent) and is dropped if it escapes `blobs_root`.
///
/// Sequential, deliberately: this is one `read_dir` per *deferred tool* — a set
/// bounded by the registered locks — where the package walk's bounded fan-out
/// exists because a package store routinely holds thousands of entries.
async fn shim_entries(shim_dirs: &[ShimDir], blobs_root: &Path) -> Vec<(PathBuf, Vec<PathBuf>)> {
    let mut entries = Vec::with_capacity(shim_dirs.len());
    for shim in shim_dirs {
        let blob_refs = read_refs(&shim.refs_blobs_dir(), blobs_root).await;
        entries.push((canonicalize_or_keep(shim.root()), blob_refs));
    }
    entries
}

/// Roots every lock-pinned `(repository, digest)` in whichever tiers actually
/// hold it, and records which `ocx.lock` contributed each root
/// (plan contract C-014).
///
/// Replaces the inline project-root loop in [`ReachabilityGraph::build`] and
/// extends it with the shim tier.
///
/// **Two tiers, one pin.** A pinned leaf names a package directory
/// ([`PackageStore::path`](crate::file_structure::PackageStore::path), keyed on
/// registry and digest with the repository deliberately dropped for cross-repo
/// dedup) *and* a shim directory
/// ([`ShimStore::path`](crate::file_structure::ShimStore::path), which does
/// carry the repository). Normally exactly one of the two exists — a
/// shim directory exists precisely when the package is absent — but both may:
/// materializing a deferred tool never removes its shim, and plan contract C-013
/// keeps the composer emitting the shim slot regardless of content-cache state,
/// so a leftover shim is live, not litter.
///
/// **Shim liveness has no package edge to inherit.** Layers and blobs stay alive
/// through an edge out of a materialized package directory. A deferred tool has
/// no package directory at all, so modelling shim liveness on that pattern would
/// collect every live shim on the first `ocx clean`. The lock pin *is* the root.
///
/// Both insertions are guarded on `all_entries` membership — the same guard
/// [`GarbageCollector::build`](super::GarbageCollector::build) applies to its
/// patch roots. A pin whose tier is absent on this machine (a foreign-platform
/// leaf; a tool that was never deferred) must not become a root: an unwalked
/// path can never be *collected*, but it would be *reported*, because
/// `PackageManager::clean` turns every attribution key into a dry-run row.
///
/// Call this **after** `all_entries` is complete — below the passive-entry
/// loops, not at the top of `build` where today's unguarded loop sits.
fn extend_lock_pinned_roots(
    file_structure: &FileStructure,
    project_roots: &[ProjectRootDigests],
    all_entries: &HashMap<PathBuf, CasTier>,
    roots: &mut HashSet<PathBuf>,
    roots_attribution: &mut HashMap<PathBuf, Vec<PathBuf>>,
) {
    for project_root in project_roots {
        for pinned in &project_root.digests {
            let tiers = [file_structure.packages.path(pinned), file_structure.shims.path(pinned)];
            for tier_path in tiers {
                // Canonicalize before the guard, never after: `all_entries` is
                // canonical-keyed, so a raw probe misses whenever `$OCX_HOME`
                // sits behind a symlink. An absent tier fails to canonicalize,
                // falls back to the raw path, misses the guard, and is skipped
                // — which is exactly the gate this contract wants.
                let canonical = canonicalize_or_keep(&tier_path);
                if !all_entries.contains_key(&canonical) {
                    continue;
                }
                roots.insert(canonical.clone());
                roots_attribution
                    .entry(canonical)
                    .or_default()
                    .push(project_root.ocx_lock_path.clone());
            }
        }
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
        // Skip `replace_atomic`/`register` staging temps: a crash-orphaned
        // `.tmp-*` symlink must never root a package (issue #179), mirroring
        // `ProjectRegistry::live_projects`.
        if entry.file_name().to_string_lossy().starts_with(".tmp-") {
            continue;
        }
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
    use std::path::{Path, PathBuf};

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
            artifact_type: None,
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
                artifact_type: None,
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

    // ── C-014: the shim tier ───────────────────────────────────────────────
    //
    // Fixtures here are real directories under a *canonicalized* tempdir root.
    // The defect class C-014 guards against is path keying — a raw path probed
    // against a canonical-keyed map — so a hand-built path map would exercise
    // the wrong thing (`quality-rust.md` "Cross-Platform Path Handling").

    use crate::file_structure::FileStructure;
    use crate::oci;

    use super::super::ProjectRootDigests;

    /// A valid SHA-256 hex built from one repeated nibble. Two fixtures with
    /// different nibbles land in different CAS shards, which keys on the first
    /// 32 hex characters.
    fn digest_of(nibble: char) -> oci::Digest {
        oci::Digest::Sha256(nibble.to_string().repeat(64))
    }

    fn pin(registry: &str, repository: &str, nibble: char) -> oci::PinnedIdentifier {
        oci::PinnedIdentifier::try_from(
            oci::Identifier::new_registry(repository, registry).clone_with_digest(digest_of(nibble)),
        )
        .expect("an identifier carrying a digest is a valid PinnedIdentifier")
    }

    /// A `FileStructure` rooted at the tempdir's **canonical** path, so an
    /// expected value derived from a store accessor already matches the keys
    /// `build` writes (macOS `/tmp` -> `/private/tmp`).
    fn home(tmp: &tempfile::TempDir) -> FileStructure {
        FileStructure::with_root(dunce::canonicalize(tmp.path()).expect("tempdir canonicalizes"))
    }

    /// Writes a blob entry (`<blob-dir>/data`) and returns its canonical entry
    /// directory — the shape `BlobStore::list_all` reports and `read_refs`
    /// recovers from a forward-ref target's parent.
    async fn seed_blob(file_structure: &FileStructure, registry: &str, digest: &oci::Digest) -> PathBuf {
        let dir = file_structure.blobs.path(registry, digest);
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join("data"), b"config-blob").await.unwrap();
        super::canonicalize_or_keep(&dir)
    }

    /// Publishes a shim directory the way `prepare_lazy` does — `bin/` (the
    /// completeness marker), the `digest` file, and one `refs/blobs/`
    /// forward-ref per closure config blob — and returns its canonical path.
    async fn seed_shim(
        file_structure: &FileStructure,
        pinned: &oci::PinnedIdentifier,
        config_blobs: &[oci::Digest],
    ) -> PathBuf {
        let shim = file_structure.shims.shim_dir(pinned);
        tokio::fs::create_dir_all(shim.bin()).await.unwrap();
        tokio::fs::write(shim.digest_file(), pinned.digest().to_string())
            .await
            .unwrap();
        tokio::fs::create_dir_all(shim.refs_blobs_dir()).await.unwrap();
        for digest in config_blobs {
            seed_blob(file_structure, pinned.registry(), digest).await;
            crate::symlink::update(
                file_structure.blobs.data(pinned.registry(), digest),
                shim.refs_blobs_dir().join(crate::file_structure::cas_ref_name(digest)),
            )
            .unwrap();
        }
        super::canonicalize_or_keep(shim.root())
    }

    /// Creates the package directory for `pinned` — `content/` is what the
    /// package walk classifies on — and returns its canonical path.
    async fn seed_package(file_structure: &FileStructure, pinned: &oci::PinnedIdentifier) -> PathBuf {
        let dir = file_structure.packages.path(pinned);
        tokio::fs::create_dir_all(dir.join("content")).await.unwrap();
        super::canonicalize_or_keep(&dir)
    }

    /// One `ocx.lock` pinning `digests`, in the shape `collect_project_roots`
    /// hands to the graph builder.
    fn lock_pinning(lock_path: &Path, digests: &[oci::PinnedIdentifier]) -> Vec<ProjectRootDigests> {
        vec![ProjectRootDigests {
            ocx_lock_path: lock_path.to_path_buf(),
            digests: digests.to_vec(),
        }]
    }

    // ── shim_entries: the shim tier is edge-bearing, not passive ───────────

    /// C-014: "its `refs/blobs/` links keep the closure's config blobs
    /// reachable". The shim tier is therefore registered with the **package**
    /// shape — entry *and* edges — so `shim_entries` pairs each walked shim
    /// with the blob entry directories its forward-refs name.
    #[tokio::test]
    async fn shim_entries_pairs_each_shim_with_its_ref_linked_config_blobs() {
        let tmp = tempfile::tempdir().unwrap();
        let file_structure = home(&tmp);
        let pinned = pin("example.com", "cmake", 'a');
        let config = digest_of('b');
        let shim_dir = seed_shim(&file_structure, &pinned, std::slice::from_ref(&config)).await;
        let blob_dir = super::canonicalize_or_keep(&file_structure.blobs.path(pinned.registry(), &config));

        let entries = super::shim_entries(
            &file_structure.shims.list_all().await.unwrap(),
            &super::canonicalize_or_keep(file_structure.blobs.root()),
        )
        .await;

        assert_eq!(entries.len(), 1, "one published shim must produce exactly one entry");
        assert_eq!(
            entries[0].0, shim_dir,
            "C-014: the entry is keyed by the shim's canonical directory"
        );
        assert_eq!(
            entries[0].1,
            vec![blob_dir],
            "C-014: the shim's `refs/blobs/` forward-ref becomes an edge to the blob ENTRY dir \
             (the target's parent), which is what keeps the closure's config blob reachable"
        );
    }

    /// A shim that links no config blob is still an **entry**. Dropping it
    /// would keep it out of `all_entries`, and the `all_entries`-guarded
    /// rooting would then refuse to root a live shim — collecting it. C-008
    /// (A1) makes the empty case legal, not exceptional.
    #[tokio::test]
    async fn shim_entries_reports_a_shim_with_no_config_blobs_as_an_edgeless_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let file_structure = home(&tmp);
        let shim_dir = seed_shim(&file_structure, &pin("example.com", "cmake", 'a'), &[]).await;

        let entries = super::shim_entries(
            &file_structure.shims.list_all().await.unwrap(),
            &super::canonicalize_or_keep(file_structure.blobs.root()),
        )
        .await;

        assert_eq!(entries.len(), 1, "a shim with an empty `refs/blobs/` is still an entry");
        assert_eq!(entries[0].0, shim_dir);
        assert!(entries[0].1.is_empty(), "it simply carries no edges");
    }

    /// Defence in depth, matching `read_refs`: a forward-ref resolving outside
    /// the blob store is dropped rather than becoming an edge. Paired with a
    /// legitimate ref in the same shim so the assertion cannot pass by the
    /// function returning nothing at all.
    #[tokio::test]
    async fn shim_entries_drops_a_ref_pointing_outside_the_blob_store() {
        let tmp = tempfile::tempdir().unwrap();
        let file_structure = home(&tmp);
        let pinned = pin("example.com", "cmake", 'a');
        let config = digest_of('b');
        seed_shim(&file_structure, &pinned, std::slice::from_ref(&config)).await;
        let blob_dir = super::canonicalize_or_keep(&file_structure.blobs.path(pinned.registry(), &config));

        // A second forward-ref aimed at a file outside `blobs/` entirely.
        let outside = file_structure.root().join("outside");
        tokio::fs::create_dir_all(&outside).await.unwrap();
        tokio::fs::write(outside.join("data"), b"not-a-blob").await.unwrap();
        crate::symlink::update(
            outside.join("data"),
            file_structure
                .shims
                .shim_dir(&pinned)
                .refs_blobs_dir()
                .join("sha256_escapee"),
        )
        .unwrap();

        let entries = super::shim_entries(
            &file_structure.shims.list_all().await.unwrap(),
            &super::canonicalize_or_keep(file_structure.blobs.root()),
        )
        .await;

        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].1,
            vec![blob_dir],
            "the in-store ref survives and the escaping one is dropped"
        );
    }

    // ── extend_lock_pinned_roots: one gate, two tiers ─────────────────────

    /// C-014, the core clause: a shim directory is live iff its
    /// `(repository, digest)` is in the lock-pinned root set. The pin is the
    /// root — a deferred tool has no package directory to inherit an edge from.
    #[tokio::test]
    async fn extend_lock_pinned_roots_roots_a_deferred_tools_shim_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let file_structure = home(&tmp);
        let pinned = pin("example.com", "cmake", 'a');
        let shim_dir = seed_shim(&file_structure, &pinned, &[]).await;
        let lock_path = file_structure.root().join("ocx.lock");

        let all_entries = HashMap::from([(shim_dir.clone(), CasTier::Shim)]);
        let mut roots = HashSet::new();
        let mut attribution: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
        super::extend_lock_pinned_roots(
            &file_structure,
            &lock_pinning(&lock_path, std::slice::from_ref(&pinned)),
            &all_entries,
            &mut roots,
            &mut attribution,
        );

        assert!(
            roots.contains(&shim_dir),
            "C-014: the lock pin roots the shim directory; got roots={roots:?}"
        );
        assert_eq!(
            attribution.get(&shim_dir),
            Some(&vec![lock_path]),
            "the shim root is attributed to the lock that pins it — `ocx clean --dry-run` \
             renders that as the `Held By` column"
        );
        assert!(
            !roots.contains(&super::canonicalize_or_keep(&file_structure.packages.path(&pinned))),
            "the package tier is absent for a deferred tool, so its path must NOT be rooted: \
             an unwalked path can never be collected, but it WOULD be reported as a phantom \
             dry-run row"
        );
    }

    /// The same one gate still roots the package tier — a materialized tool's
    /// pin must keep working exactly as before the shim tier existed.
    #[tokio::test]
    async fn extend_lock_pinned_roots_roots_a_materialized_tools_package_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let file_structure = home(&tmp);
        let pinned = pin("example.com", "cmake", 'a');
        let package_dir = seed_package(&file_structure, &pinned).await;
        let lock_path = file_structure.root().join("ocx.lock");

        let all_entries = HashMap::from([(package_dir.clone(), CasTier::Package)]);
        let mut roots = HashSet::new();
        let mut attribution: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
        super::extend_lock_pinned_roots(
            &file_structure,
            &lock_pinning(&lock_path, std::slice::from_ref(&pinned)),
            &all_entries,
            &mut roots,
            &mut attribution,
        );

        assert!(
            roots.contains(&package_dir),
            "the package tier's rooting is unchanged; got roots={roots:?}"
        );
        assert!(
            !roots.contains(&super::canonicalize_or_keep(&file_structure.shims.path(&pinned))),
            "no shim exists for a materialized tool, so no shim path may be rooted"
        );
    }

    /// C-013: the composer emits the shim slot **regardless of content-cache
    /// state**, and materializing never removes the shim directory — so a tool
    /// that was composed lazily and then materialized has both tiers on disk
    /// and both are live. Collecting the leftover shim would make the emitted
    /// environment a function of content-cache state, which C-013 forbids.
    #[tokio::test]
    async fn extend_lock_pinned_roots_roots_both_tiers_when_both_exist() {
        let tmp = tempfile::tempdir().unwrap();
        let file_structure = home(&tmp);
        let pinned = pin("example.com", "cmake", 'a');
        let shim_dir = seed_shim(&file_structure, &pinned, &[]).await;
        let package_dir = seed_package(&file_structure, &pinned).await;
        let lock_path = file_structure.root().join("ocx.lock");

        let all_entries = HashMap::from([
            (shim_dir.clone(), CasTier::Shim),
            (package_dir.clone(), CasTier::Package),
        ]);
        let mut roots = HashSet::new();
        let mut attribution: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
        super::extend_lock_pinned_roots(
            &file_structure,
            &lock_pinning(&lock_path, &[pinned]),
            &all_entries,
            &mut roots,
            &mut attribution,
        );

        assert!(roots.contains(&package_dir), "the materialized package stays rooted");
        assert!(
            roots.contains(&shim_dir),
            "C-013: the shim survives materialization — the next compose still emits its slot"
        );
    }

    /// The gate stays a gate. A pin whose tier is absent on this machine (a
    /// foreign-platform leaf, a tool never pulled) roots nothing and produces
    /// no attribution row — otherwise `PackageManager::clean` renders a
    /// nonexistent path as a held object in `--dry-run`.
    #[tokio::test]
    async fn extend_lock_pinned_roots_roots_nothing_for_a_pin_absent_from_every_tier() {
        let tmp = tempfile::tempdir().unwrap();
        let file_structure = home(&tmp);
        let present = pin("example.com", "cmake", 'a');
        let absent = pin("example.com", "shfmt", 'c');
        let shim_dir = seed_shim(&file_structure, &present, &[]).await;
        let lock_path = file_structure.root().join("ocx.lock");

        let all_entries = HashMap::from([(shim_dir.clone(), CasTier::Shim)]);
        let mut roots = HashSet::new();
        let mut attribution: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
        super::extend_lock_pinned_roots(
            &file_structure,
            &lock_pinning(&lock_path, &[present, absent.clone()]),
            &all_entries,
            &mut roots,
            &mut attribution,
        );

        assert_eq!(
            roots,
            HashSet::from([shim_dir.clone()]),
            "only the pin that exists in a walked tier becomes a root"
        );
        assert_eq!(
            attribution.keys().collect::<Vec<_>>(),
            vec![&shim_dir],
            "and only that pin gets an attribution row — no phantom `Held By` line for \
             `{}` or `{}`",
            file_structure.packages.path(&absent).display(),
            file_structure.shims.path(&absent).display()
        );
    }

    /// The shim root is keyed on the **logical** pinned identity — the one
    /// `ocx.lock` stores and `prepare_lazy` publishes under (`resolved.pinned`,
    /// Decision C2) — never on the physical transport identity an index-routed
    /// package resolves through.
    ///
    /// `ocx.sh/ocx/cli` is exactly that case: a logical name the published
    /// index routes to a different registry *and* repository, both carrying the
    /// same digest. `ShimStore::path` puts registry and repository in the path,
    /// so the two identities name two different directories — and a
    /// same-registry fixture cannot tell them apart at all, which is what makes
    /// a transport-keyed regression invisible.
    #[tokio::test]
    async fn extend_lock_pinned_roots_keys_the_shim_root_on_the_logical_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let file_structure = home(&tmp);
        let logical = pin("ocx.sh", "ocx/cli", 'a');
        let physical = pin("ghcr.io", "ocx-sh/ocx", 'a');
        let logical_shim = seed_shim(&file_structure, &logical, &[]).await;
        let physical_shim = seed_shim(&file_structure, &physical, &[]).await;
        assert_ne!(
            logical_shim, physical_shim,
            "precondition: the two identities must name different directories"
        );
        let lock_path = file_structure.root().join("ocx.lock");

        let all_entries = HashMap::from([
            (logical_shim.clone(), CasTier::Shim),
            (physical_shim.clone(), CasTier::Shim),
        ]);
        let mut roots = HashSet::new();
        let mut attribution: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
        super::extend_lock_pinned_roots(
            &file_structure,
            &lock_pinning(&lock_path, &[logical]),
            &all_entries,
            &mut roots,
            &mut attribution,
        );

        assert_eq!(
            roots,
            HashSet::from([logical_shim]),
            "the lock's logical pin roots the logical shim and only it — rooting the physical \
             one instead would collect every index-routed package's shim on the first clean"
        );
    }

    // ── the graph, end to end ─────────────────────────────────────────────

    /// C-014's mandated red-and-green, first half: the graph must **see** a
    /// deferred tool's shim directory as a walked `CasTier::Shim` entry, root
    /// it from the lock pin, and reach its config blob through the shim's own
    /// `refs/blobs/` edge.
    ///
    /// The `reachable()` assertion is the one that reds if the shim tier is
    /// ever registered with the passive shape (`all_entries.insert` and nothing
    /// else, the way layers and blobs are): a passively-registered shim is
    /// rooted while its closure's config blobs are collected on the same run.
    ///
    /// The fixture's repository is deliberately deep. `ShimStore::list_all` is
    /// unbounded by C-004's amendment, and a `max_depth` bound copied from
    /// `package_store.rs` reports an EMPTY list — which under C-014 means
    /// "collect every shim". This test reds if that bound ever reappears.
    #[tokio::test(flavor = "multi_thread")]
    async fn reachability_graph_walks_and_roots_a_deferred_tools_shim_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let file_structure = home(&tmp);
        let pinned = pin("example.com", "org/project/sub/tool", 'a');
        let config = digest_of('b');
        let shim_dir = seed_shim(&file_structure, &pinned, std::slice::from_ref(&config)).await;
        let blob_dir = super::canonicalize_or_keep(&file_structure.blobs.path(pinned.registry(), &config));
        let lock_path = file_structure.root().join("ocx.lock");

        let graph = ReachabilityGraph::build(&file_structure, &lock_pinning(&lock_path, &[pinned]))
            .await
            .unwrap();

        assert_eq!(
            graph.all_entries.get(&shim_dir),
            Some(&CasTier::Shim),
            "C-014: the shim store is a walked tier; got all_entries={:?}",
            graph.all_entries
        );
        assert!(
            graph.roots.contains(&shim_dir),
            "C-014: the lock pin roots it; got roots={:?}",
            graph.roots
        );
        assert!(
            graph.reachable().contains(&blob_dir),
            "C-014/C-020: the closure's config blob is reachable THROUGH the shim's \
             `refs/blobs/` edge — a passively-registered shim would leave it orphaned"
        );
    }

    /// C-014's mandated red-and-green, second half, at the layer that decides
    /// deletion: install nothing, compose lazily, clean — the shim directory
    /// and its config blob survive.
    ///
    /// The seeded orphan blob is the positive control. Without it the two
    /// `!contains` assertions would pass trivially on a build that never walks
    /// the shim store at all, which is precisely today's state
    /// (`quality-rust.md` "Negative path assertions are the trap").
    #[tokio::test(flavor = "multi_thread")]
    async fn clean_keeps_a_deferred_tools_shim_dir_and_its_config_blob() {
        let tmp = tempfile::tempdir().unwrap();
        let file_structure = home(&tmp);
        let pinned = pin("example.com", "org/project/sub/tool", 'a');
        let config = digest_of('b');
        let shim_dir = seed_shim(&file_structure, &pinned, std::slice::from_ref(&config)).await;
        let blob_dir = super::canonicalize_or_keep(&file_structure.blobs.path(pinned.registry(), &config));
        let orphan_blob = seed_blob(&file_structure, "example.com", &digest_of('f')).await;
        let lock_path = file_structure.root().join("ocx.lock");

        let collector = super::super::GarbageCollector::build(
            &file_structure,
            &lock_pinning(&lock_path, &[pinned]),
            &super::super::super::resolve::SitePatchRoots::default(),
        )
        .await
        .unwrap();
        let unreachable = collector.unreachable_objects();

        assert!(
            unreachable.contains(&orphan_blob),
            "positive control: an unreferenced blob IS collected, so the assertions below \
             are observing a live harness; got unreachable={unreachable:?}"
        );
        assert!(
            !unreachable.contains(&shim_dir),
            "C-014/S-008: a shim the lock still pins survives `ocx clean`"
        );
        assert!(
            !unreachable.contains(&blob_dir),
            "C-020: and so does the config blob it ref-links — a deferred consumer reads its \
             env carriers from there, having no package directory to read them from"
        );
    }

    /// A shim whose lock pin disappeared is collected, and its config blob goes
    /// with it. Nothing keeps a shim alive by accident: `has_live_refs` roots
    /// packages off `refs/symlinks/`, which a shim has none of, and every
    /// package edge is confined to the package/layer/blob roots — so no package
    /// edge can point into `shims/`.
    #[tokio::test(flavor = "multi_thread")]
    async fn clean_collects_a_shim_whose_lock_pin_disappeared() {
        let tmp = tempfile::tempdir().unwrap();
        let file_structure = home(&tmp);
        let pinned = pin("example.com", "cmake", 'a');
        let config = digest_of('b');
        let shim_dir = seed_shim(&file_structure, &pinned, std::slice::from_ref(&config)).await;
        let blob_dir = super::canonicalize_or_keep(&file_structure.blobs.path(pinned.registry(), &config));

        let collector = super::super::GarbageCollector::build(
            &file_structure,
            &[],
            &super::super::super::resolve::SitePatchRoots::default(),
        )
        .await
        .unwrap();
        let unreachable = collector.unreachable_objects();

        assert!(
            unreachable.contains(&shim_dir),
            "an unpinned shim is garbage; got unreachable={unreachable:?}"
        );
        assert!(
            unreachable.contains(&blob_dir),
            "and its config blob is reachable only through it, so it is collected too"
        );
    }

    /// D4 / C-014: `$OCX_HOME/.bin/` is never collected. It holds no CAS tier —
    /// nothing enumerates it — so the guarantee is structural, and its red
    /// state is reachable only by a future change that enumerates `.bin` as a
    /// tier. That is exactly the regression this guard exists for; it defends
    /// nothing wider, and the paired orphan blob (which IS deleted here) is
    /// what keeps the `!any` assertion from being an unchecked green.
    #[tokio::test(flavor = "multi_thread")]
    async fn dot_bin_ocx_shim_is_never_collected() {
        let tmp = tempfile::tempdir().unwrap();
        let file_structure = home(&tmp);
        let published_shim_blob = file_structure.shim_bin.ensure().await.unwrap();
        let orphan_blob = seed_blob(&file_structure, "example.com", &digest_of('f')).await;

        let collector = super::super::GarbageCollector::build(
            &file_structure,
            &[],
            &super::super::super::resolve::SitePatchRoots::default(),
        )
        .await
        .unwrap();
        let unreachable = collector.unreachable_objects();
        let dot_bin = file_structure.root().join(".bin");

        assert!(
            unreachable.contains(&orphan_blob),
            "positive control: the collector does select entries for deletion here"
        );
        assert!(
            !unreachable.iter().any(|entry| entry.starts_with(&dot_bin)),
            "D4: nothing under `$OCX_HOME/.bin/` may be selected for collection; \
             got unreachable={unreachable:?}"
        );

        collector.delete_objects(&unreachable, false).await.unwrap();
        assert!(
            published_shim_blob.is_file(),
            "the embedded shim blob survives a real collection pass"
        );
        assert!(
            !orphan_blob.exists(),
            "while the orphan blob is genuinely removed — both outcomes observed"
        );
    }

    /// The GC collects precisely what `ShimStore::list_all` fails to report
    /// (C-014), and a repository's segment count is variable and unbounded
    /// (C-004's amendment) — so a `max_depth` bound on that walk is not a
    /// partial bug but total data loss. The shallow shim is asserted alongside
    /// the deep one because the bound copied from `package_store.rs` is short
    /// by the whole repository component and loses both.
    #[tokio::test]
    async fn gc_shim_enumeration_reaches_a_deep_repository_shim() {
        let tmp = tempfile::tempdir().unwrap();
        let file_structure = home(&tmp);
        let shallow = seed_shim(&file_structure, &pin("example.com", "cmake", 'a'), &[]).await;
        let deep = seed_shim(&file_structure, &pin("example.com", "org/project/sub/tool", 'b'), &[]).await;

        let found: Vec<PathBuf> = file_structure
            .shims
            .list_all()
            .await
            .unwrap()
            .iter()
            .map(|shim| super::canonicalize_or_keep(shim.root()))
            .collect();

        assert!(
            found.contains(&deep),
            "a shim under a 4-segment repository must be reported; got {found:?}"
        );
        assert!(
            found.contains(&shallow),
            "and so must a shim under a 1-segment one; got {found:?}"
        );
    }

    // ── benchmark scaffolding (run explicitly: --ignored --nocapture) ───────

    /// Serial reference implementation of the index-retention scan — a copy of
    /// the pre-parallelization loop, kept only inside this `#[ignore]` bench so
    /// the before/after numbers are measured against the real prior shape.
    #[cfg(test)]
    async fn add_index_retention_edges_serial(
        blob_dirs: &[crate::file_structure::BlobDir],
        edges: &mut HashMap<PathBuf, Vec<PathBuf>>,
    ) {
        use crate::oci;
        for blob in blob_dirs {
            let Some(registry_root) = blob.dir.parent().and_then(|p| p.parent()).and_then(|p| p.parent()) else {
                continue;
            };
            let Some(bytes) = super::read_manifest_candidate_blob(&blob.data()).await else {
                continue;
            };
            let Ok(oci::Manifest::ImageIndex(index)) = serde_json::from_slice::<oci::Manifest>(&bytes) else {
                continue;
            };
            let index_dir = super::canonicalize_or_keep(&blob.dir);
            for entry in &index.manifests {
                let Ok(child_digest) = oci::Digest::try_from(entry.digest.as_str()) else {
                    continue;
                };
                let child_dir = super::canonicalize_or_keep(
                    &registry_root.join(crate::file_structure::cas_shard_path(&child_digest)),
                );
                edges.entry(child_dir).or_default().push(index_dir.clone());
            }
        }
    }

    /// Build a synthetic blob store: `index_count` small image-index blobs (each
    /// advertising `children_per_index` leaves) plus `noise_count` non-manifest
    /// "layer" blobs that the scan must stat-and-skip. Returns the `BlobDir`
    /// list in store-walk order.
    #[cfg(test)]
    async fn build_synthetic_store(
        root: &std::path::Path,
        index_count: usize,
        children_per_index: usize,
        noise_count: usize,
    ) -> Vec<crate::file_structure::BlobDir> {
        use crate::file_structure::{BlobDir, cas_shard_path};
        use crate::oci;

        // `cas_shard_path` keys on the FIRST 32 hex chars of the digest, so a
        // unique on-disk shard requires the counter to vary the leading hex —
        // zero-padding on the right keeps every blob in its own directory.
        let unique = |prefix: u8, counter: usize| -> oci::Digest {
            oci::Digest::Sha256(format!("{prefix:02x}{counter:030x}{:032x}", 0u64))
        };

        let registry_root = root.join("registry_slug");
        let mut blob_dirs = Vec::new();

        for index_number in 0..index_count {
            let manifests = (0..children_per_index)
                .map(|child_number| oci::ImageIndexEntry {
                    media_type: "application/vnd.oci.image.manifest.v1+json".to_string(),
                    // `Digest` Display already renders as `sha256:<hex>`.
                    digest: unique(0x0c, index_number * children_per_index + child_number).to_string(),
                    size: 100,
                    platform: None,
                    artifact_type: None,
                    annotations: None,
                })
                .collect();
            let index = oci::Manifest::ImageIndex(oci::ImageIndex {
                schema_version: 2,
                media_type: Some("application/vnd.oci.image.index.v1+json".to_string()),
                artifact_type: None,
                manifests,
                annotations: None,
            });
            let index_json = serde_json::to_vec(&index).expect("serialize index");
            let dir = registry_root.join(cas_shard_path(&unique(0x1a, index_number)));
            tokio::fs::create_dir_all(&dir).await.expect("mkdir");
            tokio::fs::write(dir.join("data"), &index_json).await.expect("write");
            blob_dirs.push(BlobDir { dir });
        }

        // Non-manifest blobs: 8 KiB of bytes that fail to parse as a manifest
        // but are still read in full (under the 4 MiB ceiling), exercising the
        // read+parse cost on the common non-index blob.
        let noise = vec![0u8; 8 * 1024];
        for noise_number in 0..noise_count {
            let dir = registry_root.join(cas_shard_path(&unique(0x2b, noise_number)));
            tokio::fs::create_dir_all(&dir).await.expect("mkdir");
            tokio::fs::write(dir.join("data"), &noise).await.expect("write");
            blob_dirs.push(BlobDir { dir });
        }

        blob_dirs
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    #[ignore = "benchmark: run with --ignored --nocapture"]
    async fn bench_index_retention_scan_serial_vs_parallel() {
        const INDEX_COUNT: usize = 200;
        const CHILDREN_PER_INDEX: usize = 8;
        const NOISE_COUNT: usize = 2000;
        const ITERATIONS: u32 = 5;

        let tmp = tempfile::tempdir().expect("tempdir");
        let blob_dirs = build_synthetic_store(tmp.path(), INDEX_COUNT, CHILDREN_PER_INDEX, NOISE_COUNT).await;
        let total_blobs = blob_dirs.len();

        // Warm the page cache so both variants read from the same warm state.
        let mut warm: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
        super::add_index_retention_edges(&blob_dirs, &mut warm).await;
        let edge_count: usize = warm.values().map(|v| v.len()).sum();
        assert_eq!(
            edge_count,
            INDEX_COUNT * CHILDREN_PER_INDEX,
            "synthetic store must produce one retention edge per advertised child"
        );

        let mut serial_total = std::time::Duration::ZERO;
        let mut parallel_total = std::time::Duration::ZERO;
        let mut serial_edges: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
        let mut parallel_edges: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();

        for _ in 0..ITERATIONS {
            let mut edges = HashMap::new();
            let start = std::time::Instant::now();
            add_index_retention_edges_serial(&blob_dirs, &mut edges).await;
            serial_total += start.elapsed();
            serial_edges = edges;

            let mut edges = HashMap::new();
            let start = std::time::Instant::now();
            super::add_index_retention_edges(&blob_dirs, &mut edges).await;
            parallel_total += start.elapsed();
            parallel_edges = edges;
        }

        // Equivalence proof: parallel output must equal serial output exactly.
        assert_eq!(
            serial_edges, parallel_edges,
            "parallel scan must produce an identical edge map to the serial scan"
        );

        let serial_ms = serial_total.as_secs_f64() * 1000.0 / f64::from(ITERATIONS);
        let parallel_ms = parallel_total.as_secs_f64() * 1000.0 / f64::from(ITERATIONS);
        println!("\n=== index-retention scan benchmark ===");
        println!("blobs scanned per pass : {total_blobs} ({INDEX_COUNT} indexes + {NOISE_COUNT} noise)");
        println!("retention edges created: {edge_count}");
        println!("iterations             : {ITERATIONS}");
        println!("serial   (mean)        : {serial_ms:.2} ms");
        println!("parallel (mean)        : {parallel_ms:.2} ms");
        println!("speedup                : {:.2}x", serial_ms / parallel_ms);
    }
}
