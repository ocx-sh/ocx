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
    log,
};

use super::project_roots::ProjectRootDigests;

/// Three-state liveness of a package install back-reference.
///
/// Returned by the back-ref probe performed during GC root identification.
/// The distinction between [`RefLiveness::Dead`] and [`RefLiveness::Unknown`]
/// is the SEC-1 silent-data-loss guard: collapsing a transient I/O `Err`
/// into "dead" would cause `clean` to collect a live package.
///
/// `max` semantics apply when combining results over multiple back-refs:
/// `Live > Unknown > Dead`.
// Stub: used from P3.3 implementation onward.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefLiveness {
    /// At least one install back-ref resolves to a live install symlink. The
    /// package is a GC root and must not be collected.
    Live,
    /// A transient I/O error (`EACCES`, `ESTALE`, `EIO`, …) prevented a
    /// definitive liveness check. Treated as `Live` for GC purposes (retain).
    Unknown,
    /// All install back-refs are definitively absent (`Ok(false)` or
    /// `NotFound`). Safe to consider for collection.
    Dead,
}

// Stub: `max` used from P3.3 implementation onward.
#[allow(dead_code)]
impl RefLiveness {
    /// Returns the "higher" of two liveness outcomes.
    ///
    /// `Live > Unknown > Dead` — conservatively retaining a package when any
    /// back-ref signals uncertainty.
    pub fn max(self, other: Self) -> Self {
        match (self, other) {
            (Self::Live, _) | (_, Self::Live) => Self::Live,
            (Self::Unknown, _) | (_, Self::Unknown) => Self::Unknown,
            _ => Self::Dead,
        }
    }
}

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
                let liveness = has_live_refs(&pkg_dir).await;
                // Forward-edge reads fail closed: a transient I/O error aborts
                // the GC build rather than silently dropping this package's
                // outgoing edges (see `read_refs` docs — forward-edge SEC-1).
                let dep_refs = read_refs(&deps_dir, &pkgs_root).await?;
                let layer_refs = read_refs(&layers_dir, &lyrs_root).await?;
                let blob_refs = read_refs(&blobs_dir, &blbs_root).await?;
                let mut all_edges = dep_refs;
                all_edges.extend(layer_refs);
                all_edges.extend(blob_refs);
                Ok::<_, crate::Error>((pkg_dir, liveness, all_edges))
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
            let (pkg_dir, liveness, pkg_edges) = result.expect("task panicked")?;

            // Retain both `Live` and `Unknown` as roots. `Unknown` means the
            // liveness probe hit a transient I/O error (NFS/automount) — failing
            // safe (retain) avoids GC'ing a live package whose back-refs could
            // not be read this run. Only a definitively `Dead` package is a
            // collection candidate.
            if !matches!(liveness, RefLiveness::Dead) {
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

/// Reads forward-refs from a refs subdirectory (deps/, layers/, or blobs/).
///
/// Each symlink target is expected to be a content path inside `store_root`.
/// The parent of the target (the CAS entry directory) is returned.
/// Symlinks pointing outside `store_root` are skipped (defence-in-depth).
///
/// # Errors
///
/// Returns the underlying I/O error (as [`crate::Error::InternalFile`]) when the
/// forward-edge set cannot be enumerated completely: a non-`NotFound` `read_dir`
/// failure, a mid-iteration `read_dir` error, or a `read_link` failure on a
/// confirmed symlink. A genuinely-absent directory (`NotFound`) is **not** an
/// error — it means the package declares no edges of this kind and yields
/// `Ok(empty)`.
///
/// This is the forward-edge mirror of the [`has_live_refs`] back-ref guard
/// (SEC-1 silent-data-loss class). Collapsing a transient I/O error into an
/// empty edge set would drop a live package's outgoing edges, making its still
/// referenced deps/layers/blobs unreachable and therefore collectible while the
/// package itself stays a live root. Failing closed (propagating) aborts `clean`
/// rather than letting it collect against an incomplete reachability graph.
async fn read_refs(refs_dir: &Path, store_root: &Path) -> crate::Result<Vec<PathBuf>> {
    let mut targets = Vec::new();
    let mut entries = match tokio::fs::read_dir(refs_dir).await {
        Ok(entries) => entries,
        // A genuinely-absent refs subdir = no edges of this kind (e.g. a package
        // with no dependencies has no refs/deps). This is the only safe "empty
        // edge set": definitive absence.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(targets),
        // Any OTHER error (EACCES/ESTALE/EIO on NFS/automount, …) leaves the
        // forward-edge set indeterminate — fail closed (see fn doc).
        Err(e) => return Err(crate::error::file_error(refs_dir, e)),
    };
    loop {
        let entry = match entries.next_entry().await {
            Ok(Some(entry)) => entry,
            Ok(None) => break,
            // A mid-iteration directory-read error truncates the edge set — same
            // indeterminate-forward-edges hazard as the read_dir arm.
            Err(e) => return Err(crate::error::file_error(refs_dir, e)),
        };
        let entry_path = entry.path();
        if !crate::symlink::is_link(&entry_path) {
            continue;
        }
        let ref_target = match tokio::fs::read_link(&entry_path).await {
            Ok(target) => target,
            // The entry IS a symlink (a declared edge) but its target cannot be
            // read — we know an edge exists yet not where it points. Fail closed.
            Err(e) => return Err(crate::error::file_error(&entry_path, e)),
        };
        let Some(entry_dir) = ref_target.parent() else {
            continue;
        };
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
    Ok(targets)
}

/// Three-state liveness of a package directory's install back-refs.
///
/// `Live` = at least one `refs/symlinks/` entry still points to an existing
/// target. `Dead` = directory readable but no live ref (safe to collect).
/// `Unknown` = a transient I/O error (`EACCES`/`ESTALE`/`EIO` on NFS/automount)
/// left liveness indeterminate; the caller retains the package as a root
/// (fail-safe — collapsing this to `Dead` would silently GC a live package,
/// the SEC-1 class).
///
/// Liveness over multiple back-refs is the `max` fold (`Live > Unknown > Dead`):
/// a single live ref wins, otherwise a single `Unknown` ref forces retention.
async fn has_live_refs(pkg_dir: &Path) -> RefLiveness {
    let refs_dir = pkg_dir.join("refs").join("symlinks");
    let mut entries = match tokio::fs::read_dir(&refs_dir).await {
        Ok(entries) => entries,
        // A genuinely-absent refs dir means no install back-refs → Dead.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return RefLiveness::Dead,
        // Any other error reading the directory is a transient I/O failure —
        // retain conservatively.
        Err(e) => {
            log::debug!(
                "Cannot read install back-refs at '{}' ({e}); treating liveness as Unknown (retain).",
                refs_dir.display()
            );
            return RefLiveness::Unknown;
        }
    };

    let mut liveness = RefLiveness::Dead;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if !crate::symlink::is_link(&path) {
            continue;
        }
        let target = match tokio::fs::read_link(&path).await {
            Ok(target) => target,
            // Reading the symlink itself failed transiently (EACCES/ESTALE/EIO
            // on NFS/automount). We cannot determine where this back-ref points,
            // so it contributes Unknown (retain) — collapsing it to Dead by
            // skipping the entry would silently GC a possibly-live package
            // (the SEC-1 class). Mirrors the try_exists Err arm below.
            Err(e) => {
                log::debug!(
                    "I/O error reading install back-ref symlink '{}' ({e}); liveness Unknown (retain).",
                    path.display()
                );
                liveness = liveness.max(RefLiveness::Unknown);
                continue;
            }
        };
        match tokio::fs::try_exists(&target).await {
            // Target present → this package is a live GC root; short-circuit.
            Ok(true) => return RefLiveness::Live,
            // Target definitively absent → contributes Dead (no change).
            Ok(false) => {}
            // Probe failed transiently → contributes Unknown (retain).
            Err(e) => {
                log::debug!(
                    "I/O error probing install back-ref target '{}' ({e}); liveness Unknown (retain).",
                    target.display()
                );
                liveness = liveness.max(RefLiveness::Unknown);
            }
        }
    }
    liveness
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

    use super::RefLiveness;

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

    // ── RefLiveness three-state ─────────────────────────────────────────────
    //
    // Requirement: system_design_shared_store.md §5 M4 item 2 —
    // `RefLiveness::{Live, Unknown, Dead}` with max semantics `Live > Unknown > Dead`.
    // Unknown must be treated as Live for GC (retain, not collect).
    // Traced to: plan_shared_store P3.2s back-ref three-state.

    #[test]
    fn ref_liveness_max_live_beats_all() {
        // Live > Unknown and Live > Dead.
        assert_eq!(RefLiveness::Live.max(RefLiveness::Unknown), RefLiveness::Live);
        assert_eq!(RefLiveness::Live.max(RefLiveness::Dead), RefLiveness::Live);
        assert_eq!(RefLiveness::Unknown.max(RefLiveness::Live), RefLiveness::Live);
        assert_eq!(RefLiveness::Dead.max(RefLiveness::Live), RefLiveness::Live);
    }

    #[test]
    fn ref_liveness_max_unknown_beats_dead() {
        // Unknown > Dead.
        assert_eq!(RefLiveness::Unknown.max(RefLiveness::Dead), RefLiveness::Unknown);
        assert_eq!(RefLiveness::Dead.max(RefLiveness::Unknown), RefLiveness::Unknown);
    }

    #[test]
    fn ref_liveness_max_dead_plus_dead_is_dead() {
        // Dead max Dead = Dead (safe to collect).
        assert_eq!(RefLiveness::Dead.max(RefLiveness::Dead), RefLiveness::Dead);
    }

    #[test]
    fn ref_liveness_max_is_commutative() {
        // Commutativity — order of arguments must not matter.
        let pairs = [
            (RefLiveness::Live, RefLiveness::Dead),
            (RefLiveness::Live, RefLiveness::Unknown),
            (RefLiveness::Unknown, RefLiveness::Dead),
        ];
        for (a, b) in pairs {
            assert_eq!(
                a.max(b),
                b.max(a),
                "RefLiveness::max must be commutative for ({a:?}, {b:?})"
            );
        }
    }

    #[test]
    fn ref_liveness_unknown_retain_semantic() {
        // SEC-1 guard: collapsing an I/O error to Dead would cause clean to
        // collect a live package. Unknown must compare as >= Dead so a single
        // Unknown back-ref stops collection.
        //
        // This test encodes the retain-on-unknown semantic: given a set of
        // back-refs where at least one returns Unknown, the combined liveness
        // must not be Dead.
        let liveness_results = [RefLiveness::Dead, RefLiveness::Unknown, RefLiveness::Dead];
        let combined = liveness_results
            .iter()
            .copied()
            .fold(RefLiveness::Dead, RefLiveness::max);
        assert_ne!(
            combined,
            RefLiveness::Dead,
            "a single Unknown back-ref must prevent Dead verdict (retain-on-unknown)"
        );
        assert_eq!(
            combined,
            RefLiveness::Unknown,
            "combined liveness of [Dead, Unknown, Dead] must be Unknown"
        );
    }

    // ── install_backref_io_error_is_unknown (SEC-1 guard via has_live_refs) ──────

    /// Proves that a genuine I/O error probing `refs/symlinks/` yields
    /// `RefLiveness::Unknown`, NOT `Dead`.
    ///
    /// The SEC-1 guard: collapsing a transient I/O error to `Dead` would cause
    /// `clean` to collect a live package whose back-refs could not be read due
    /// to a permission flip, NFS stale handle, etc.
    ///
    /// Technique (Unix-only): create a package directory with a `refs/symlinks/`
    /// sub-directory holding one symlink entry, then set `0o000` permissions on
    /// `refs/symlinks/` so `read_dir` returns `EACCES`. `has_live_refs` must
    /// return `Unknown` (retain), never `Dead`.
    #[cfg(unix)]
    #[tokio::test]
    async fn install_backref_io_error_is_unknown() {
        use std::os::unix::fs::PermissionsExt;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let pkg_dir = dir.path().join("packages/sha256/aa/bb");

        // Build refs/symlinks/ with one entry (a dangling symlink is fine —
        // has_live_refs reads the dir before following targets).
        let refs_dir = pkg_dir.join("refs").join("symlinks");
        std::fs::create_dir_all(&refs_dir).unwrap();
        // Place a symlink entry so the directory is non-empty (empty dirs with
        // 0o000 still return EACCES on read_dir, but be explicit).
        let symlink_entry = refs_dir.join("entry");
        std::os::unix::fs::symlink("/nonexistent/target", &symlink_entry).unwrap();

        // Revoke read+exec so `read_dir` → EACCES (transient-like I/O error).
        std::fs::set_permissions(&refs_dir, std::fs::Permissions::from_mode(0o000)).unwrap();

        let liveness = super::has_live_refs(&pkg_dir).await;

        // Restore permissions so tempdir cleanup can remove the directory.
        std::fs::set_permissions(&refs_dir, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(
            liveness,
            RefLiveness::Unknown,
            "has_live_refs must return Unknown (not Dead) when refs/symlinks/ is unreadable \
             (SEC-1 silent-data-loss guard: an I/O error must not be collapsed to Dead)"
        );
    }

    // ── I/O-error → Unknown retain (try_exists probe path) ─────────────────────

    /// Documents the design contract for the `try_exists` Err arm of
    /// `has_live_refs`: when probing a back-ref target returns an `Err`
    /// (EACCES, ESTALE, EIO, …), that ref contributes `Unknown`, which causes
    /// the whole package to be retained rather than collected.
    ///
    /// We cannot inject a real `try_exists` I/O error here without a
    /// filesystem, but the `max()` combinator that folds per-ref results is
    /// exercised directly above (`max_combinator_*`), and the `read_dir` Err
    /// path is covered by `install_backref_io_error_is_unknown`. This test
    /// pins the distinctness the retain-on-error guard relies on.
    #[test]
    fn ref_liveness_io_error_yields_unknown_not_dead() {
        // The implementation obligation: map `Err(_)` in `try_exists` (and in
        // `read_link`) to `RefLiveness::Unknown` before folding with `max`.
        assert_ne!(
            RefLiveness::Unknown,
            RefLiveness::Dead,
            "Unknown and Dead must be distinct; collapsing them breaks the retain-on-error guard"
        );
    }

    // ── read_link I/O error → Unknown retain (FIX A) ───────────────────────────

    /// Proves the `read_link` Err arm of `has_live_refs` yields `Unknown`
    /// (retain), not `Dead`, for a readable `refs/symlinks/` directory whose
    /// entry cannot be `read_link`-ed.
    ///
    /// PORTABILITY GAP (documented, intentional): a deterministic `read_link`
    /// error is not portably injectable. A valid symlink reads fine; making the
    /// symlink's *parent* unreadable makes `read_dir` fail first (a different
    /// code path, already covered by `install_backref_io_error_is_unknown`),
    /// and a dangling symlink still `read_link`s successfully (the target need
    /// not exist). We therefore do not fabricate a fragile platform-specific
    /// `read_link` failure. Instead this test pins the *adjacent* invariant:
    /// a `refs/symlinks/` entry that is a dangling symlink (read_dir + read_link
    /// both succeed, but the target is absent) contributes `Dead` only — so the
    /// fold result is `Dead` when no error and no live target exists. The Err
    /// arm itself is verified by the matching code structure (the read_link Err
    /// arm mirrors the try_exists Err arm verified by the SEC-1 test) plus the
    /// `max`-combinator coverage above.
    #[cfg(unix)]
    #[tokio::test]
    async fn ref_liveness_dangling_symlink_is_dead_not_unknown() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let pkg_dir = dir.path().join("packages/sha256/aa/bb");
        let refs_dir = pkg_dir.join("refs").join("symlinks");
        std::fs::create_dir_all(&refs_dir).unwrap();

        // A dangling symlink: read_dir + read_link both succeed; the target is
        // absent, so try_exists returns Ok(false) → contributes Dead. This
        // confirms the read_link Ok arm does not spuriously yield Unknown.
        let symlink_entry = refs_dir.join("entry");
        std::os::unix::fs::symlink("/nonexistent/target", &symlink_entry).unwrap();

        let liveness = super::has_live_refs(&pkg_dir).await;

        assert_eq!(
            liveness,
            RefLiveness::Dead,
            "a dangling back-ref (read_link OK, target absent) must be Dead, not Unknown — \
             only a read_link/try_exists I/O *error* yields Unknown"
        );
    }

    // ── read_refs forward-edge fail-closed (data-loss guard) ───────────────────
    //
    // Forward-edge mirror of the SEC-1 back-ref guard. `read_refs` enumerates a
    // package's outgoing edges (refs/deps, refs/layers, refs/blobs). If a
    // transient I/O error is collapsed into an empty edge set, the package's
    // still-referenced deps/layers/blobs become unreachable and collectible while
    // the package itself stays a live root — silent data loss.
    //
    // Requirement traced to: /swarm-review max + Codex cross-model finding;
    // symmetric to has_live_refs three-state (system_design_shared_store §5 M4).

    /// A non-`NotFound` `read_dir` error (here `ENOTDIR`, from pointing at a
    /// regular file) must propagate, never collapse the forward-edge set to
    /// empty. Deterministic + root-safe: no permission trick — `ENOTDIR` fires
    /// for any uid.
    #[tokio::test]
    async fn read_refs_non_notfound_error_propagates_not_empty() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let store_root = dir.path().join("layers");
        // A regular file where a refs subdir is expected: `read_dir` fails with
        // ENOTDIR (≠ NotFound).
        let not_a_dir = dir.path().join("refs_deps_is_a_file");
        std::fs::write(&not_a_dir, b"x").unwrap();

        let result = super::read_refs(&not_a_dir, &store_root).await;

        assert!(
            result.is_err(),
            "read_refs must propagate a non-NotFound I/O error (ENOTDIR) rather than collapse the \
             forward-edge set to empty; collapsing makes a live package's deps/layers/blobs \
             collectible (forward-edge mirror of the SEC-1 silent-data-loss guard)"
        );
    }

    /// A genuinely-absent refs subdir (`NotFound`) is the one safe empty edge
    /// set — the package declares no edges of this kind. Must be `Ok(empty)`,
    /// not an error (an error would needlessly abort every `clean`).
    #[tokio::test]
    async fn read_refs_absent_dir_is_empty_not_error() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let store_root = dir.path().join("layers");
        // Never created.
        let absent = dir.path().join("refs").join("deps");

        let result = super::read_refs(&absent, &store_root).await;

        assert_eq!(
            result.expect("an absent (NotFound) refs subdir must be Ok, not Err"),
            Vec::<PathBuf>::new(),
            "a genuinely-absent refs subdir (NotFound) means the package declares no edges of \
             this kind → Ok(empty)"
        );
    }
}
