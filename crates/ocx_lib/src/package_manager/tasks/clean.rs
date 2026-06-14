// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::file_structure::{FileStructure, StaleEntry};
use crate::log;
use crate::oci;
use crate::project::shared_roots::{SharedRoots, SharedRootsUnion, is_shared_store_enabled};
use crate::project::{ProjectLock, ProjectRegistry};

use crate::utility::fs::filesystem_kind::{self, NetworkFsPosture};

use super::super::PackageManager;
use super::garbage_collection::audit_log::new_run_id;
use super::garbage_collection::audit_log::{AuditAction, ObjectKind};
use super::garbage_collection::{
    AuditLog, GarbageCollector, GcLock, ProjectRootDigests, grace_seconds_from_env, lock_timeouts_from_env,
};

/// Concurrency cap for `collect_project_roots` cross-tool / cross-platform
/// resolution. Mirrors the cap used by the reachability graph builder so a
/// pathological registry with many projects does not flood the I/O scheduler.
const COLLECT_ROOTS_CONCURRENCY: usize = 50;

/// A single object-store entry surfaced by `ocx clean`.
///
/// Carries the path of the object and the set of registered project lock files
/// that pin it. The `held_by` field is non-empty only in dry-run mode when the
/// object would have been collected in the absence of the project registry.
/// It is always empty for `temp` entries (see
/// [`adr_clean_project_backlinks.md`] "`ocx clean` UX").
#[derive(Debug, Clone)]
pub struct CleanedObject {
    /// Absolute path of the object-store entry.
    pub path: PathBuf,
    /// Absolute paths of every `ocx.lock` that pins this object.
    /// Empty when the object had no project-registry pin (truly unreferenced)
    /// or when `--force` was specified.
    pub held_by: Vec<PathBuf>,
}

/// Results returned by [`PackageManager::clean`].
///
/// `objects` lists every package-store entry that was removed (or would be
/// removed in dry-run mode), each with optional attribution to holding
/// projects. `temp` lists stale temporary directories cleaned up alongside.
///
/// See [`adr_clean_project_backlinks.md`] for the full data-flow contract.
pub struct CleanResult {
    pub objects: Vec<CleanedObject>,
    pub temp: Vec<PathBuf>,
}

/// Resolves a locked `pinned` identifier to the set of `PinnedIdentifier`s
/// that actually key package-store paths.
///
/// `ocx.lock` stores the **ImageIndex manifest digest** (the top-level OCI
/// manifest, which covers all platforms). The package store is keyed by the
/// **child platform-manifest digest** — the one selected when `ocx pull`
/// ran on the current platform. To find the correct package-store path, this
/// function:
///
/// 1. Reads the manifest blob from the local blob store at
///    `blobs/{registry}/{algo}/{shard}/data`.
/// 2. Parses it as an [`oci::Manifest`].
/// 3. If it is a flat `Image` manifest: the locked digest IS the package
///    digest; return it unchanged.
/// 4. If it is an `ImageIndex`: enumerate the child manifest digests and
///    return all that have a corresponding package directory on disk (i.e.
///    those that were actually pulled on this machine).
///
/// If the blob is absent (the package was never pulled to this store) or
/// unreadable, the original digest is returned as a best-effort fallback —
/// the resulting path will not exist in the store and will therefore be a
/// no-op root that does not affect GC decisions, but also does not raise an
/// error that would abort the operation. This benign fallback is correct for
/// **local** project roots (the package is not on this machine, so there is
/// nothing to protect). It is the WRONG choice for shared/peer roots — see
/// [`resolve_shared_root`], which fails closed instead.
async fn resolve_to_package_digests(
    pinned: &oci::PinnedIdentifier,
    file_structure: &FileStructure,
) -> Vec<oci::PinnedIdentifier> {
    match resolve_root_inner(pinned, file_structure).await {
        // Resolved cleanly: use the enumerated package digests as-is.
        RootResolution::Resolved(resolved) => resolved,
        // Unresolvable (blob absent/unreadable, manifest unparseable) OR an
        // ImageIndex whose children are all absent on disk: fall back to the
        // original digest so GC decisions are unaffected. The fallback path
        // does not exist in the store, so it is a harmless no-op root.
        RootResolution::Unresolvable | RootResolution::NoChildrenOnDisk => vec![pinned.clone()],
    }
}

/// Outcome of resolving a root digest to its package-store keys, retaining the
/// distinctions both callers need:
///
/// - `Resolved` — the manifest blob was read and parsed; the carried vector is
///   the set of package-store digests this root protects (a flat `Image`
///   resolves to the locked digest itself; an `ImageIndex` resolves to the
///   subset of platform children present on disk, which is always non-empty
///   here).
/// - `NoChildrenOnDisk` — the manifest WAS read and parsed as an `ImageIndex`,
///   but none of its platform children have a package directory on disk. This
///   is SAFE for shared roots (the blob was readable, no platform of this
///   package is on the volume, so there is nothing to protect) but the local
///   caller treats it as a no-op fallback.
/// - `Unresolvable` — the blob could not be read, or the manifest could not be
///   parsed. The peer's platform package digests cannot be enumerated. The
///   local caller treats this as a benign no-op; the shared caller MUST fail
///   closed (see [`resolve_shared_root`]).
enum RootResolution {
    Resolved(Vec<oci::PinnedIdentifier>),
    NoChildrenOnDisk,
    Unresolvable,
}

/// Shared manifest read + parse + child-enumeration logic behind both
/// [`resolve_to_package_digests`] (local, benign fallback) and
/// [`resolve_shared_root`] (shared, fail-closed). Keeping the ImageIndex walk
/// in one place avoids duplicating the (tested) platform-child translation.
async fn resolve_root_inner(pinned: &oci::PinnedIdentifier, file_structure: &FileStructure) -> RootResolution {
    let registry = pinned.registry();
    let digest = pinned.digest();
    let blob_path = file_structure.blobs.data(registry, &digest);

    let blob_bytes = match tokio::fs::read(&blob_path).await {
        Ok(bytes) => bytes,
        Err(_) => {
            // Blob not cached locally — we cannot enumerate the package digests.
            log::debug!("Project root blob not found locally for '{}'.", pinned);
            return RootResolution::Unresolvable;
        }
    };

    let manifest: oci::Manifest = match serde_json::from_slice(&blob_bytes) {
        Ok(m) => m,
        Err(e) => {
            log::warn!("Failed to parse manifest blob for '{}': {e}", pinned);
            return RootResolution::Unresolvable;
        }
    };

    match manifest {
        // Flat image manifest: the locked digest IS the package-store key.
        oci::Manifest::Image(_) => RootResolution::Resolved(vec![pinned.clone()]),

        // Image index: the package is stored by the child platform-manifest
        // digest. Enumerate all children and return those that are present
        // in the package store on disk (i.e. were pulled on this machine).
        oci::Manifest::ImageIndex(index) => {
            let mut resolved = Vec::new();
            for entry in &index.manifests {
                let child_digest = match oci::Digest::try_from(entry.digest.as_str()) {
                    Ok(d) => d,
                    Err(e) => {
                        log::warn!(
                            "ImageIndex child digest '{}' is malformed for '{}': {e}",
                            entry.digest,
                            pinned
                        );
                        continue;
                    }
                };
                // Construct a child PinnedIdentifier with the same registry
                // and repository as the parent, but with the child digest.
                let child_id =
                    oci::Identifier::new_registry(pinned.repository(), registry).clone_with_digest(child_digest);
                let child_pinned = match oci::PinnedIdentifier::try_from(child_id) {
                    Ok(p) => p,
                    Err(_) => continue,
                };
                // Only include children that actually have a package directory
                // on disk. This handles the common case where only one
                // platform was pulled to this machine.
                let pkg_path = file_structure.packages.path(&child_pinned);
                if crate::utility::fs::path_exists_lossy(&pkg_path).await {
                    resolved.push(child_pinned);
                }
            }
            if resolved.is_empty() {
                // No children present on disk — the manifest was readable, so
                // this is a determinate "nothing of this package is here".
                log::debug!("No ImageIndex children found on disk for '{}'.", pinned);
                RootResolution::NoChildrenOnDisk
            } else {
                RootResolution::Resolved(resolved)
            }
        }
    }
}

/// Resolves a **shared/peer** root digest to the package-store digests it
/// protects, failing closed on any unresolvable input.
///
/// Returns:
/// - `Some(vec)` when the manifest blob was read and parsed — the vector is the
///   subset of platform children present on disk (possibly empty `Some(vec![])`,
///   which is SAFE: the blob was readable, no platform of this package is on the
///   volume, so there is nothing to protect).
/// - `None` (UNRESOLVABLE) when the blob could not be read or the manifest could
///   not be parsed. The peer's platform package digests cannot be enumerated, so
///   the caller MUST retain everything this run rather than collect against an
///   incomplete root set.
///
/// Rationale: in the safe shared-CACHE deployment (UC1) a peer that pinned X
/// also pulled X into the shared cache, so the blob is present and resolution
/// succeeds. Blob-absence only occurs in the per-instance-cache deployment,
/// where this cleaner genuinely cannot determine the peer's package digest
/// while the peer's package directory still lives on the shared packages
/// volume — making fail-closed (RetainAll) the only safe choice. This is the
/// opposite of [`resolve_to_package_digests`]'s benign local fallback.
async fn resolve_shared_root(
    pinned: &oci::PinnedIdentifier,
    file_structure: &FileStructure,
) -> Option<Vec<oci::PinnedIdentifier>> {
    match resolve_root_inner(pinned, file_structure).await {
        RootResolution::Resolved(resolved) => Some(resolved),
        // The blob was readable but no platform child is on this volume —
        // nothing of this package to protect here, which is safe to treat as
        // an empty (but resolved) root set.
        RootResolution::NoChildrenOnDisk => Some(Vec::new()),
        // Blob unreadable or manifest unparseable: cannot enumerate the peer's
        // package digests → fail closed.
        RootResolution::Unresolvable => None,
    }
}

/// Enumerates live registered projects from the flat symlink ledger, reads each
/// project's `ocx.lock`, and returns the resolved package digests as GC roots.
///
/// This is a free function (not a method on [`PackageManager`]) per the
/// task-module architecture rule in `subsystem-package-manager.md`: helpers
/// that orchestrate multi-step work stay as free functions taking explicit
/// parameters, keeping the shared `impl PackageManager` namespace minimal.
///
/// Called by [`PackageManager::clean`] when `force` is `false`. When a single
/// project lock cannot be read, the entry is skipped with a WARN log and does
/// not abort the clean operation.
///
/// The `file_structure` parameter is used to resolve each locked identifier's
/// manifest from the local blob store. `ocx.lock` stores the **ImageIndex
/// manifest digest** (the top-level OCI index), but the package store is keyed
/// by the **child platform manifest digest**. This function reads the ImageIndex
/// blob and enumerates its children so that `ProjectRootDigests::digests`
/// contains the digests that actually map to package-store paths.
///
/// Uses [`crate::project::registry::ProjectRegistry::live_projects`] — the flat
/// symlink store at `$OCX_HOME/projects/` (ADR: `adr_project_gc_symlink_ledger.md`).
/// There is no JSON parse surface: stale/broken links are silently pruned.
/// A corrupt-registry exit-78 branch is deliberately absent — eliminated with
/// the JSON ledger (ADR §Risks "Corrupt-state failure mode removed, not relocated").
pub async fn collect_project_roots(ocx_home: &Path, file_structure: &FileStructure) -> crate::Result<CollectedRoots> {
    // The project GC ledger (`projects/`) lives in the per-instance STATE zone
    // (`OCX_STATE_DIR`, defaulting to `$OCX_HOME`), never under the shared
    // content store. With `OCX_STATE_DIR` set, rooting the registry at
    // `$OCX_HOME` would defeat UC1 isolation, so construct it from the resolved
    // state-zone root (system_design_shared_store.md §5 M2). `ocx_home` still
    // names the GLOBAL toolchain lock (`$OCX_HOME/ocx.lock`) added below — the
    // global tier's config genuinely lives in `$OCX_HOME`, not the state zone.
    let state_zone = file_structure.state_zone_root();
    let registry = ProjectRegistry::new(state_zone);

    // Opportunistic legacy cleanup: the superseded JSON ledger
    // (`projects.json` + its `.projects.lock` advisory sentinel) is obsolete
    // under the flat symlink store. No migration of contents — the symlink
    // ledger is rebuilt by ordinary `ocx.lock` saves. Remove once if present,
    // a single debug line (never WARN — benign legacy artifact). The legacy
    // files lived in the state zone too, so look there.
    let legacy_json = state_zone.join("projects.json");
    let legacy_lock = state_zone.join(".projects.lock");
    let legacy_present = crate::utility::fs::path_exists_lossy(&legacy_json).await
        || crate::utility::fs::path_exists_lossy(&legacy_lock).await;
    if legacy_present {
        log::debug!(
            "Removing obsolete legacy project ledger files ('{}', '{}').",
            legacy_json.display(),
            legacy_lock.display()
        );
        let _ = tokio::fs::remove_file(&legacy_json).await;
        let _ = tokio::fs::remove_file(&legacy_lock).await;
    }

    // Enumerate the live project directories from the flat symlink ledger,
    // self-pruning departed-project links along the way. There is no parse
    // surface (no JSON document), so the old corrupt-registry →
    // `crate::Error::InternalFile` (exit 78) branch is deliberately ELIMINATED
    // — a bad/dangling link is simply pruned (ADR §Risks "Corrupt-state
    // failure mode removed, not relocated").
    //
    // A `live_projects()` error fails CLOSED (plan A2): it propagates so
    // `ocx clean` aborts (classifies to `IoError`) rather than running
    // destructive GC against a live multi-project store with zero project
    // roots — degrading to `Vec::new()` here was the fail-open
    // silent-data-loss bug. `--force` already bypasses the registry entirely
    // upstream (explicit operator override), so the sanctioned escape hatch
    // is unaffected by this propagation.
    let project_dirs = registry.live_projects().await?;
    // The ledger targets the project *directory*; the lock is its canonical
    // sibling `<dir>/ocx.lock` (invariant
    // `lock_path_for(config) == <dir>/ocx.lock`, ARCH-4d). Derive the lock
    // path here so the downstream load/resolve pipeline (and the
    // `ProjectRootDigests.ocx_lock_path` diagnostic field) is unchanged.
    let mut entries: Vec<PathBuf> = project_dirs.into_iter().map(|dir| dir.join("ocx.lock")).collect();

    // The global toolchain lock (`$OCX_HOME/ocx.lock`) is an **implicit** GC
    // root. Its project directory is `$OCX_HOME` itself, which is barred from
    // the `$OCX_HOME/projects/` symlink ledger by design
    // (`adr_project_gc_symlink_ledger.md` — no self-link), so it never appears
    // via `live_projects()`. But the global tier is the project tier with a
    // different load site (`adr_global_toolchain_tier.md` D5, amended
    // 2026-05-19): its pinned packages must be reachable exactly like a
    // project's. Add it unconditionally — an absent global lock is mapped to
    // `Ok(None)` by `ProjectLock::from_path` below and skipped, so this is a
    // no-op when no global toolchain is configured.
    entries.push(ocx_home.join("ocx.lock"));

    // Two-pass parallel walk:
    //   1. Read every registered `ocx.lock` in parallel (one task per entry).
    //   2. Flat-fan-out the cross-product (entry, tool) and resolve every
    //      tool's `pinned` identifier concurrently under a shared semaphore.
    //
    // Sort each entry's resolved digests by `(group, name)` and the entries
    // themselves by `ocx_lock_path` so the output is deterministic — the
    // garbage-collector reachability graph keys on these paths.
    //
    // Step 1 — load locks in parallel.
    let mut load_set: JoinSet<LockLoad> = JoinSet::new();
    for lock_path in entries {
        load_set.spawn(async move {
            match ProjectLock::from_path(&lock_path).await {
                Ok(Some(lock)) => LockLoad::Loaded(LoadedLock {
                    lock_path,
                    tools: lock.tools,
                }),
                Ok(None) => {
                    // `from_path` maps a genuinely-absent lock (`NotFound`)
                    // to `Ok(None)`. This is the benign departed-project
                    // case (`test_lazy_prune_after_lockfile_deletion`):
                    // debug + drop the root.
                    log::debug!(
                        "Skipping project root '{}': lock file no longer present.",
                        lock_path.display()
                    );
                    LockLoad::Absent
                }
                Err(e) => {
                    // A registered live root whose lock cannot be read due
                    // to a transient (non-`NotFound`) I/O error —
                    // EACCES/ESTALE on a *live* holder (e.g. the
                    // `ProbeResult::Unknown` root `read_link`-recovered in
                    // `live_projects`, whose lock now sits behind a
                    // momentarily-unreachable path component). Its pinned
                    // digests are indeterminate. Fail CLOSED (plan A1/A2):
                    // signal the whole GC to retain everything this run
                    // rather than silently dropping the root (which would
                    // GC the live project's pinned packages — the
                    // silent-data-loss class A1 closes one layer up). The
                    // run still succeeds (non-fatal); `--force` remains the
                    // sanctioned override to GC anyway.
                    log::warn!(
                        "Project root '{}': lock unreadable (transient I/O); retaining all objects \
                         this run (fail-closed): {e}",
                        lock_path.display()
                    );
                    LockLoad::Indeterminate
                }
            }
        });
    }

    let mut loaded: Vec<LoadedLock> = Vec::new();
    while let Some(join) = load_set.join_next().await {
        match join.expect("collect_project_roots load task panicked") {
            LockLoad::Loaded(l) => loaded.push(l),
            LockLoad::Absent => {}
            LockLoad::Indeterminate => {
                // Drain the remaining joins so no spawned task is detached,
                // then return the fail-closed retain-all marker.
                load_set.abort_all();
                while load_set.join_next().await.is_some() {}
                return Ok(CollectedRoots::RetainAll);
            }
        }
    }

    // Step 2 — resolve every (lock, tool) pair under a bounded semaphore.
    let sem = Arc::new(Semaphore::new(COLLECT_ROOTS_CONCURRENCY));
    let mut resolve_set: JoinSet<(usize, String, String, Vec<oci::PinnedIdentifier>)> = JoinSet::new();
    for (index, loaded_lock) in loaded.iter().enumerate() {
        for tool in &loaded_lock.tools {
            let sem = Arc::clone(&sem);
            let pinned = tool.pinned.clone();
            let group = tool.group.clone();
            let name = tool.name.clone();
            // Dense post-filter position in `loaded` (Bug-R3): the resolve
            // buckets are sized `loaded.len()`, so the key MUST be the
            // survivor's dense index here, never the original `entries`
            // enumerate index (which spans `LockLoad::Absent` entries too and
            // would index `buckets` out of bounds).
            // `resolve_to_package_digests` borrows `&FileStructure`. Cloning is
            // cheap (the struct holds `Arc`-shared sub-stores).
            let fs = file_structure.clone();
            resolve_set.spawn(async move {
                let _permit = sem.acquire_owned().await.expect("semaphore closed");
                let resolved = resolve_to_package_digests(&pinned, &fs).await;
                (index, group, name, resolved)
            });
        }
    }

    // Materialise into a per-entry buffer keyed by the survivor's dense
    // position in `loaded` (Bug-R3: never the original enumerate index) so we
    // can sort tool-level results by (group, name) inside each entry without
    // depending on JoinSet completion order. `index` is in `0..loaded.len()`
    // by construction, so `buckets[index]` cannot panic.
    let mut buckets: Vec<Vec<(String, String, Vec<oci::PinnedIdentifier>)>> =
        (0..loaded.len()).map(|_| Vec::new()).collect();
    while let Some(join) = resolve_set.join_next().await {
        let (index, group, name, resolved) = join.expect("collect_project_roots resolve task panicked");
        buckets[index].push((group, name, resolved));
    }

    // Re-key the buckets onto their `LoadedLock` entries with deterministic
    // intra-entry ordering.
    let mut roots: Vec<ProjectRootDigests> = loaded
        .into_iter()
        .zip(buckets)
        .map(|(loaded_lock, mut bucket)| {
            bucket.sort_by(|a, b| (a.0.as_str(), a.1.as_str()).cmp(&(b.0.as_str(), b.1.as_str())));
            let mut digests = Vec::new();
            for (_, _, resolved) in bucket {
                digests.extend(resolved);
            }
            ProjectRootDigests {
                ocx_lock_path: loaded_lock.lock_path,
                digests,
            }
        })
        .collect();

    // Inter-entry order: sort by lock_path so callers see a stable list across
    // runs even when the registry's on-disk order changes.
    roots.sort_by(|a, b| a.ocx_lock_path.cmp(&b.ocx_lock_path));
    Ok(CollectedRoots::Roots(roots))
}

/// Unions every instance's shared-roots ledger and resolves it into a single
/// synthetic [`ProjectRootDigests`] grafted onto `project_roots` (P3.6).
///
/// Only called when `OCX_SHARED_STORE=true` ([`is_shared_store_enabled`]). The
/// shared-roots ledger lives at `$OCX_PACKAGES_DIR/roots/<instance_id>/<project_hash>`
/// and stores the **ImageIndex manifest digest** strings (`registry/repo@digest`)
/// pinned by every fleet member sharing this content volume. This function:
///
/// 1. Reads + unions all instances' ledgers via [`SharedRoots::union_all_digests`].
/// 2. Resolves each unioned digest string back through the same
///    [`resolve_to_package_digests`] path the project ledger uses (ImageIndex →
///    platform-child package digests present on disk). Resolve-on-read, never
///    resolve-on-write (Adjustment 2): the writer stays trivial and the tested
///    ImageIndex→platform translation is reused.
/// 3. Returns a single [`ProjectRootDigests`] whose `ocx_lock_path` is the
///    stable `roots/` directory marker so dry-run "Held By" attribution works
///    (Adjustment 3: reuses `reachability_graph.rs` root-insertion unchanged).
///
/// Fail-closed on two distinct unresolvable conditions:
/// 1. The union returns [`SharedRootsUnion::RetainAll`] (a peer ledger was
///    unparseable / unknown-version, or unreadable — the one-way-door safety
///    property): this returns [`CollectedRoots::RetainAll`].
/// 2. Any unioned peer digest is **blob-unresolvable** on this cleaner — the
///    manifest blob is absent or unparseable, so the peer's platform package
///    digests cannot be enumerated ([`resolve_shared_root`] returns `None`):
///    this also returns [`CollectedRoots::RetainAll`]. Unlike a local project
///    root (where a missing blob means the package is not on this machine and
///    is a harmless no-op), a peer's package directory may live on the shared
///    packages volume even when this cleaner lacks the blob in its own cache —
///    resolving to a no-op root would let `clean` delete the peer's package.
///
/// In both cases `clean` retains every object this run rather than collecting
/// against an incomplete root set.
async fn collect_shared_root_digests(
    file_structure: &FileStructure,
    state_zone_root: &Path,
) -> crate::Result<CollectedRoots> {
    // The ledger is rooted at the PACKAGES zone (`$OCX_PACKAGES_DIR`, shared
    // across the fleet), never the per-instance state zone. `packages.root()`
    // is `{packages_zone}/packages`; its parent is the packages zone root.
    let packages_zone_root = file_structure
        .packages
        .root()
        .parent()
        .unwrap_or_else(|| file_structure.packages.root());

    // `instance_id` comes from the per-instance state zone (`$OCX_STATE_DIR`),
    // resolved (created on first use) via the audit-log helper — single source
    // of truth for the instance-id file so the two consumers cannot drift.
    let instance_id = crate::utility::instance_id::load_or_create_instance_id(state_zone_root).await;
    let shared_roots = SharedRoots::new(packages_zone_root, instance_id);

    let union = match shared_roots.union_all_digests().await? {
        SharedRootsUnion::Digests(set) => set,
        SharedRootsUnion::RetainAll => return Ok(CollectedRoots::RetainAll),
    };

    // Resolve each unioned digest string (ImageIndex manifest digest) to its
    // package-store digests via the same translation the project ledger uses,
    // but FAIL CLOSED on any peer digest we cannot fully account for:
    //
    //   * a missing/unresolvable blob (`resolve_shared_root` → `None`), and
    //   * a digest STRING that does not parse as a `PinnedIdentifier`.
    //
    // The version discriminant already gated the ledger as readable + current,
    // so an unparseable entry inside it signals identifier-grammar skew or
    // corruption — not a benign no-op. Skipping it would silently drop a peer
    // root and let `clean` delete a live peer package (the cross-instance
    // data-loss class). Retain everything this run instead.
    let mut digests: Vec<oci::PinnedIdentifier> = Vec::new();
    for digest_str in union {
        let identifier = match oci::Identifier::parse(&digest_str) {
            Ok(id) => id,
            Err(e) => {
                log::warn!(
                    "Shared-roots digest '{digest_str}' is not a valid identifier ({e}); \
                     failing closed (RetainAll) to avoid deleting a peer's package."
                );
                return Ok(CollectedRoots::RetainAll);
            }
        };
        let pinned = match oci::PinnedIdentifier::try_from(identifier) {
            Ok(p) => p,
            Err(_) => {
                log::warn!(
                    "Shared-roots digest '{digest_str}' has no digest component; \
                     failing closed (RetainAll) to avoid deleting a peer's package."
                );
                return Ok(CollectedRoots::RetainAll);
            }
        };
        match resolve_shared_root(&pinned, file_structure).await {
            Some(resolved) => digests.extend(resolved),
            None => {
                log::warn!(
                    "Shared-roots digest '{pinned}' is blob-unresolvable on this cleaner; \
                     failing closed (RetainAll) to avoid deleting a peer's package."
                );
                return Ok(CollectedRoots::RetainAll);
            }
        }
    }

    // Stable synthetic marker so dry-run "Held By" attribution names the
    // shared-roots ledger directory rather than an absent lock file.
    let ocx_lock_path = packages_zone_root.join("roots");
    Ok(CollectedRoots::Roots(vec![ProjectRootDigests {
        ocx_lock_path,
        digests,
    }]))
}

/// A registered project's `ocx.lock` parsed into resolvable GC-root inputs.
///
/// Carries **no** load index: the resolve buckets are keyed by the survivor's
/// *dense* position in `loaded` (assigned via `loaded.iter().enumerate()`),
/// never the original `entries` enumerate index. Bug-R3 regression — the
/// original index spans every registered project including the ones that
/// became [`LockLoad::Absent`] (deleted `ocx.lock`, the common
/// departed-project case), so it can exceed `loaded.len()` and panic
/// `buckets[index]` out of bounds.
struct LoadedLock {
    lock_path: PathBuf,
    tools: Vec<crate::project::lock::LockedTool>,
}

/// Outcome of loading a single registered project's `ocx.lock`.
enum LockLoad {
    /// The lock parsed; its tools become resolvable GC roots.
    Loaded(LoadedLock),
    /// The lock is genuinely absent (`from_path` mapped `NotFound` →
    /// `Ok(None)`) — the benign departed-project case; the root is dropped.
    Absent,
    /// The lock could not be read due to a transient (non-`NotFound`) I/O
    /// error on a *registered live* root. The pinned digests are
    /// indeterminate; per plan A1/A2 the GC fails closed by retaining every
    /// object this run rather than dropping the root.
    Indeterminate,
}

/// Result of [`collect_project_roots`].
///
/// `Roots` carries the resolved per-project GC roots. `RetainAll` is the
/// fail-closed marker emitted when a registered *live* root's lock is
/// transiently unreadable (plan A1/A2): the lock's pinned digests cannot be
/// enumerated, so [`PackageManager::clean`] must retain every object this run
/// rather than collect against a partial root set (which would silently GC the
/// live project's pinned packages). The run still succeeds; `--force` remains
/// the sanctioned override.
pub enum CollectedRoots {
    /// Resolved project-registry GC roots, deterministically ordered.
    Roots(Vec<ProjectRootDigests>),
    /// Fail-closed: a live root's lock was transiently unreadable — retain
    /// all objects this run.
    RetainAll,
}

impl PackageManager {
    /// Runs garbage collection on the object store and stale temp directories.
    ///
    /// When `force` is `false` (default), packages held by any registered
    /// project's `ocx.lock` are added as reachability roots so they are not
    /// collected. When `force` is `true` the project registry is ignored
    /// entirely — only live install symlinks protect packages from collection.
    /// See `adr_project_gc_symlink_ledger.md` for the GC ledger design.
    pub async fn clean(&self, dry_run: bool, force: bool) -> crate::Result<CleanResult> {
        let ocx_home = self.file_structure().root().to_path_buf();

        // Network-FS posture (P3.7): probe the content/packages/state zones.
        // Under `refuse` this aborts with PolicyBlocked (81) before any lock or
        // deletion; under `warn` (default) it logs once and proceeds. Runs
        // before the GC lock so a refused store never even touches gc.lock.
        enforce_network_fs_posture(self.file_structure()).await?;

        // Store-wide exclusive GC lock (P3.3): serialises `clean` against
        // concurrent installs/pulls (which take the shared lock) within the same
        // `$OCX_HOME`. Held for the whole run via this RAII guard; a contended
        // lock that does not drain within `OCX_GC_LOCK_TIMEOUT` aborts with
        // TempFail (75) rather than running destructive GC alongside a mutator.
        // Acquired BEFORE building the reachability graph (no L1 object lock is
        // held yet → GC-before-L1 ordering, no deadlock).
        let (exclusive_timeout, _shared_timeout) = lock_timeouts_from_env();
        let _gc_lock =
            GcLock::acquire_exclusive_with_timeout(self.file_structure().state_zone_root(), exclusive_timeout).await?;

        // Audit log (P3.5) + mtime grace (P3.4): one run_id per invocation.
        // `open` delegates instance-id I/O to spawn_blocking so the async
        // worker is never blocked (replaces the old sync AuditLog::new call).
        let audit = AuditLog::open(self.file_structure().state_zone_root(), new_run_id()).await;
        let grace_seconds = grace_seconds_from_env();

        // Collect project-registry roots unless --force suppresses the
        // registry. A transiently-unreachable *live* root makes the root set
        // indeterminate (plan A1/A2): fail closed by retaining every object
        // this run — skip object collection entirely and only sweep stale
        // temps. The run stays non-fatal (exit 0); `--force` is the
        // sanctioned override to GC against live install symlinks alone.
        let mut project_roots: Vec<ProjectRootDigests> = if force {
            Vec::new()
        } else {
            match collect_project_roots(&ocx_home, self.file_structure()).await? {
                CollectedRoots::Roots(roots) => roots,
                CollectedRoots::RetainAll => {
                    let temp = clean_temp(self.file_structure(), dry_run, &audit).await?;
                    return Ok(CleanResult {
                        objects: Vec::new(),
                        temp,
                    });
                }
            }
        };

        // Shared-roots grafting (P3.6 Adjustment 3): under `OCX_SHARED_STORE`
        // (and never under `--force`, which bypasses the registry entirely),
        // union every fleet member's shared-roots ledger and append it as a
        // synthetic `ProjectRootDigests` so `reachability_graph.rs` roots a
        // peer's pinned packages exactly like a local project's. An unparseable
        // peer ledger fails closed (RetainAll → retain everything this run).
        if !force && is_shared_store_enabled() {
            match collect_shared_root_digests(self.file_structure(), self.file_structure().state_zone_root()).await? {
                CollectedRoots::Roots(shared) => project_roots.extend(shared),
                CollectedRoots::RetainAll => {
                    let temp = clean_temp(self.file_structure(), dry_run, &audit).await?;
                    return Ok(CleanResult {
                        objects: Vec::new(),
                        temp,
                    });
                }
            }
        }

        let garbage_collector = GarbageCollector::build(self.file_structure(), &project_roots).await?;

        let targets = garbage_collector.unreachable_objects();
        let attribution = garbage_collector.roots_attribution();

        log::debug!(
            "Scanning for unreferenced entries{}: {} candidate(s).",
            if dry_run { " (dry run)" } else { "" },
            targets.len(),
        );

        let raw_objects = garbage_collector
            .delete_objects_with_policy(&targets, dry_run, grace_seconds, &audit)
            .await?;
        // Objects returned by delete_objects are unreachable (in `targets`). By
        // definition, unreachable objects cannot appear in `attribution` (which
        // only contains objects reachable from project-registry roots). So
        // `held_by` is always empty here; the registry-held objects are added
        // separately below in dry-run mode.
        let mut objects: Vec<CleanedObject> = raw_objects
            .into_iter()
            .map(|path| CleanedObject {
                path,
                held_by: Vec::new(),
            })
            .collect();

        // In dry-run mode, also surface objects that are held by the project
        // registry. These objects are in `attribution` (reachable from a project
        // root) and by definition NOT in `targets` (reachable objects are never
        // unreachable). We add them explicitly so the dry-run report shows what
        // would be collected in `--force` mode and which lock is protecting each
        // entry.
        //
        // No second GarbageCollector::build is needed: the attribution map from
        // the single build already identifies every registry-held path via the
        // BFS propagation in ReachabilityGraph::build.
        if dry_run {
            for (held_path, held_by) in attribution {
                objects.push(CleanedObject {
                    path: held_path.clone(),
                    held_by: held_by.clone(),
                });
            }
        }

        let temp = clean_temp(self.file_structure(), dry_run, &audit).await?;
        Ok(CleanResult { objects, temp })
    }
}

/// Enforces the `OCX_NETWORK_FS` posture across the zones `clean` mutates.
///
/// Probes the content zone (packages/layers/blobs — rename atomicity) and the
/// state zone (`gc.lock` advisory locking). Under `OCX_NETWORK_FS=refuse` a
/// network filesystem on any zone aborts with `Error::NetworkFsRefused`
/// (PolicyBlocked, exit 81); under `warn` (default) it logs once per zone and
/// proceeds; under `allow` it is a no-op. A probe that fails (unsupported
/// platform, transient I/O) yields `FilesystemKind::Unknown`, which is treated
/// as local (proceed) — the posture never blocks on an indeterminate result.
///
/// Free function (not a `PackageManager` method) per the task-module
/// architecture rule: it orchestrates multi-step work over explicit params.
async fn enforce_network_fs_posture(fs: &FileStructure) -> crate::Result<()> {
    let posture = NetworkFsPosture::from_env();
    if posture == NetworkFsPosture::Allow {
        return Ok(());
    }
    // (zone label, representative path). Layers/blobs share the cache zone with
    // packages' content; probing one representative path per distinct zone root
    // is sufficient — the posture only distinguishes network vs local.
    let zones: [(&str, &Path); 3] = [
        ("cache", fs.blobs.root()),
        ("packages", fs.packages.root()),
        ("state", fs.state_zone_root()),
    ];
    for (label, path) in zones {
        let kind = filesystem_kind::filesystem_kind(path).await?;
        filesystem_kind::apply_posture(posture, label, kind)?;
    }
    Ok(())
}

/// Removes stale temp directories and orphan lock files from both temp zones.
///
/// Sweeps the package-staging temp (`fs.temp`, packages zone) and the
/// layer-staging temp (`fs.layer_temp`, cache zone). When the two zones are
/// unified (the default single-root layout) both stores point at the same
/// directory, so the second sweep finds nothing new — the operation is
/// idempotent. The stash directories created by `finalize_package_dir`'s
/// broken-install swap live under `fs.temp` as `__stale_*` orphan dirs and are
/// reclaimed here as well.
///
/// Uses [`TempStore::stale_entries`] which discovers entries from both
/// `.lock` files and directories, acquiring locks where possible to
/// prevent races with concurrent installs.
///
/// Each removed (or would-be-removed in dry-run) temp entry is recorded in the
/// audit log as `ObjectKind::Temp` with a `null` digest (best-effort — a log
/// write failure is never fatal).
async fn clean_temp(
    fs: &crate::file_structure::FileStructure,
    dry_run: bool,
    audit: &AuditLog,
) -> crate::Result<Vec<PathBuf>> {
    let mut removed = Vec::new();
    sweep_temp_store(&fs.temp, dry_run, audit, &mut removed).await?;
    // Skip the second sweep when both temp stores resolve to the same directory
    // (unified-zone layout) so an entry is never double-counted.
    if fs.layer_temp.root() != fs.temp.root() {
        sweep_temp_store(&fs.layer_temp, dry_run, audit, &mut removed).await?;
    }

    log::debug!(
        "{} {} stale temp entry/entries.",
        if dry_run { "Would remove" } else { "Removed" },
        removed.len(),
    );

    Ok(removed)
}

/// Sweeps a single [`TempStore`], appending removed directories to `removed`.
async fn sweep_temp_store(
    store: &crate::file_structure::TempStore,
    dry_run: bool,
    audit: &AuditLog,
    removed: &mut Vec<PathBuf>,
) -> crate::Result<()> {
    use AuditAction::{Deleted, WouldDelete};

    let stale = store.stale_entries()?;

    log::debug!(
        "Found {} stale temp entry/entries under {}{}.",
        stale.len(),
        store.root().display(),
        if dry_run { " (dry run)" } else { "" },
    );

    let action = if dry_run { WouldDelete } else { Deleted };

    for entry in stale {
        match entry {
            StaleEntry::Locked(acquired) => {
                let dir_path = acquired.dir.dir.clone();
                remove_stale_dir(&dir_path, dry_run, "stale").await?;
                // Drop releases the OS lock and auto-deletes the .lock file.
                drop(acquired);
                // Best-effort audit log — never fatal.
                audit.record_delete(action, ObjectKind::Temp, &dir_path, None).await;
                removed.push(dir_path);
            }
            StaleEntry::Orphan(dir_path) => {
                remove_stale_dir(&dir_path, dry_run, "orphan").await?;
                // Best-effort audit log — never fatal.
                audit.record_delete(action, ObjectKind::Temp, &dir_path, None).await;
                removed.push(dir_path);
            }
        }
    }

    Ok(())
}

async fn remove_stale_dir(dir_path: &std::path::Path, dry_run: bool, label: &str) -> crate::Result<()> {
    log::info!(
        "{} {} temp dir: {}",
        if dry_run { "Would remove" } else { "Removing" },
        label,
        dir_path.display(),
    );
    if !dry_run && dir_path.exists() {
        tokio::fs::remove_dir_all(dir_path)
            .await
            .map_err(|e| crate::Error::InternalFile(dir_path.to_path_buf(), e))?;
    }
    Ok(())
}

// ── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_structure::FileStructure;

    // Minimal valid ocx.lock that `ProjectLock::from_path` can parse.
    //
    // The `declaration_hash` value is not validated on load — only
    // `declaration_hash_version` is checked.  The `pinned` identifier must be
    // a fully-qualified `registry/repo@sha256:<hex>` form so that
    // `PinnedIdentifier::try_from` accepts it during deserialization.
    //
    // Registry must contain `.` or `:` or be "localhost" to be parsed as an
    // explicit registry (see `oci::identifier::has_explicit_registry`).
    // Using `localhost:5000` which carries a colon and is always valid.
    const LOCK_WITH_ONE_TOOL: &str = r#"
[metadata]
lock_version = 1
declaration_hash_version = 1
declaration_hash = "sha256:0000000000000000000000000000000000000000000000000000000000000000"
generated_by = "ocx test"
generated_at = "2026-01-01T00:00:00Z"

[[tool]]
name = "cmake"
group = "default"
pinned = "localhost:5000/cmake@sha256:aaaa0000000000000000000000000000000000000000000000000000000000bb"
"#;

    // A second distinct pinned digest used in multi-tool fixtures.
    const LOCK_WITH_TWO_TOOLS: &str = r#"
[metadata]
lock_version = 1
declaration_hash_version = 1
declaration_hash = "sha256:0000000000000000000000000000000000000000000000000000000000000000"
generated_by = "ocx test"
generated_at = "2026-01-01T00:00:00Z"

[[tool]]
name = "cmake"
group = "default"
pinned = "localhost:5000/cmake@sha256:aaaa0000000000000000000000000000000000000000000000000000000000bb"

[[tool]]
name = "shfmt"
group = "default"
pinned = "localhost:5000/shfmt@sha256:bbbb0000000000000000000000000000000000000000000000000000000000cc"
"#;

    /// `collect_project_roots` includes the pinned digest from
    /// `$OCX_HOME/ocx.lock` as a GC root even when there are no entries in
    /// the `$OCX_HOME/projects/` symlink ledger.
    ///
    /// Contract from `adr_global_toolchain_tier.md` D5 (amended 2026-05-19):
    /// the global lock is an **implicit** GC root; it must never be reaped
    /// even when no project is registered.
    #[tokio::test]
    async fn collect_roots_includes_global_lock_pinned_digest() {
        let dir = tempfile::tempdir().unwrap();
        let ocx_home = dir.path().to_path_buf();

        // Write the global lock at `$OCX_HOME/ocx.lock`.
        let lock_path = ocx_home.join("ocx.lock");
        tokio::fs::write(&lock_path, LOCK_WITH_ONE_TOOL).await.unwrap();

        // Empty projects/ directory — no ledger entries.
        tokio::fs::create_dir_all(ocx_home.join("projects")).await.unwrap();

        let file_structure = FileStructure::with_root(ocx_home.clone());
        let result = collect_project_roots(&ocx_home, &file_structure).await.unwrap();

        let roots = match result {
            CollectedRoots::Roots(roots) => roots,
            CollectedRoots::RetainAll => panic!("expected Roots, got RetainAll"),
        };

        // The global lock's pinned digest must appear as a root.
        assert_eq!(roots.len(), 1, "exactly one root (from the global lock)");
        let global_root = &roots[0];
        assert_eq!(
            global_root.ocx_lock_path, lock_path,
            "root's lock path must be $OCX_HOME/ocx.lock"
        );
        // The digest from the lock should be present.  `resolve_to_package_digests`
        // falls back to the original pinned id when the blob is absent locally
        // (the common case in a fresh temp dir).  Either way the digest is in roots.
        assert!(
            !global_root.digests.is_empty(),
            "global lock must contribute at least one digest root"
        );
        let digest_strs: Vec<String> = global_root.digests.iter().map(|p| p.to_string()).collect();
        assert!(
            digest_strs.iter().any(|s| s.contains("sha256:aaaa0000")),
            "cmake digest must be a GC root; got: {digest_strs:?}"
        );
    }

    /// When `$OCX_HOME/ocx.lock` is absent, `collect_project_roots` treats the
    /// global lock as a no-op: `from_path` returns `Ok(None)` for a missing file
    /// and the function neither errors nor adds any global roots.
    ///
    /// Contract: an absent global lock must never cause `ocx clean` to abort or
    /// change its exit code (exit 0; no-op).
    #[tokio::test]
    async fn collect_roots_absent_global_lock_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let ocx_home = dir.path().to_path_buf();

        // No ocx.lock written — `$OCX_HOME/ocx.lock` does not exist.
        // Empty projects/ directory.
        tokio::fs::create_dir_all(ocx_home.join("projects")).await.unwrap();

        let file_structure = FileStructure::with_root(ocx_home.clone());
        let result = collect_project_roots(&ocx_home, &file_structure).await.unwrap();

        let roots = match result {
            CollectedRoots::Roots(roots) => roots,
            CollectedRoots::RetainAll => panic!("expected Roots, got RetainAll"),
        };

        // No global lock → no global root; the function must succeed with an
        // empty root set (nothing for GC to protect from the global side).
        assert!(
            roots.is_empty(),
            "absent global lock must produce no roots; got: {roots:?}",
            roots = roots
                .iter()
                .map(|r| r.ocx_lock_path.display().to_string())
                .collect::<Vec<_>>()
        );
    }

    /// A global lock with two tools contributes both pinned digests as GC roots.
    ///
    /// Regression guard: the per-tool loop inside `collect_project_roots` must
    /// iterate all tools in the lock, not just the first.
    #[tokio::test]
    async fn collect_roots_global_lock_with_two_tools_yields_two_digests() {
        let dir = tempfile::tempdir().unwrap();
        let ocx_home = dir.path().to_path_buf();

        let lock_path = ocx_home.join("ocx.lock");
        tokio::fs::write(&lock_path, LOCK_WITH_TWO_TOOLS).await.unwrap();
        tokio::fs::create_dir_all(ocx_home.join("projects")).await.unwrap();

        let file_structure = FileStructure::with_root(ocx_home.clone());
        let result = collect_project_roots(&ocx_home, &file_structure).await.unwrap();

        let roots = match result {
            CollectedRoots::Roots(roots) => roots,
            CollectedRoots::RetainAll => panic!("expected Roots, got RetainAll"),
        };

        assert_eq!(roots.len(), 1, "one root entry (the global lock)");
        let global_root = &roots[0];
        // Both tool digests must be present.
        assert_eq!(
            global_root.digests.len(),
            2,
            "two-tool global lock must produce two digest roots; got: {:?}",
            global_root.digests.iter().map(|p| p.to_string()).collect::<Vec<_>>()
        );
        let digest_strs: Vec<String> = global_root.digests.iter().map(|p| p.to_string()).collect();
        assert!(
            digest_strs.iter().any(|s| s.contains("sha256:aaaa0000")),
            "cmake digest must be a GC root; got: {digest_strs:?}"
        );
        assert!(
            digest_strs.iter().any(|s| s.contains("sha256:bbbb0000")),
            "shfmt digest must be a GC root; got: {digest_strs:?}"
        );
    }

    // ── FIX C: shared-root resolution fails closed on unresolvable digests ──────

    /// A fully-qualified pinned identifier whose manifest blob is NOT present in
    /// the cleaner's blob store. Used to drive the blob-absent path.
    const SHARED_PEER_DIGEST: &str =
        "localhost:5000/peer@sha256:cccc0000000000000000000000000000000000000000000000000000000000dd";

    fn shared_peer_pinned() -> oci::PinnedIdentifier {
        let identifier = oci::Identifier::parse(SHARED_PEER_DIGEST).unwrap();
        oci::PinnedIdentifier::try_from(identifier).unwrap()
    }

    /// `resolve_shared_root` must return `None` (UNRESOLVABLE) when the manifest
    /// blob is absent on this cleaner — the peer's platform package digests
    /// cannot be enumerated, so the shared path must fail closed. This is the
    /// opposite of `resolve_to_package_digests`, which returns the benign
    /// `vec![pinned]` no-op fallback for the same input (asserted below for
    /// contrast — the local fallback must remain unchanged).
    #[tokio::test]
    async fn resolve_shared_root_none_on_missing_blob() {
        let dir = tempfile::tempdir().unwrap();
        let file_structure = FileStructure::with_root(dir.path().to_path_buf());
        let pinned = shared_peer_pinned();

        // No blob planted → blob read fails → Unresolvable.
        let shared = resolve_shared_root(&pinned, &file_structure).await;
        assert!(
            shared.is_none(),
            "resolve_shared_root must be None (fail closed) when the manifest blob is absent"
        );

        // Contrast: the local resolver keeps the benign no-op fallback so
        // existing project-root GC behavior is unchanged (Constraint).
        let local = resolve_to_package_digests(&pinned, &file_structure).await;
        assert_eq!(
            local,
            vec![pinned.clone()],
            "resolve_to_package_digests must keep its benign vec![pinned] fallback on a missing blob"
        );
    }

    /// `resolve_shared_root` must return `None` when the blob exists but is not
    /// a parseable manifest — the peer's digests still cannot be enumerated.
    #[tokio::test]
    async fn resolve_shared_root_none_on_unparseable_blob() {
        let dir = tempfile::tempdir().unwrap();
        let file_structure = FileStructure::with_root(dir.path().to_path_buf());
        let pinned = shared_peer_pinned();

        // Plant a non-JSON blob at the CAS data path for this digest.
        let blob_path = file_structure.blobs.data(pinned.registry(), &pinned.digest());
        tokio::fs::create_dir_all(blob_path.parent().unwrap()).await.unwrap();
        tokio::fs::write(&blob_path, b"not a manifest").await.unwrap();

        let shared = resolve_shared_root(&pinned, &file_structure).await;
        assert!(
            shared.is_none(),
            "resolve_shared_root must be None (fail closed) when the manifest blob is unparseable"
        );
    }

    /// `collect_shared_root_digests` must map an unresolvable shared digest to
    /// `CollectedRoots::RetainAll` — the fail-closed contract end to end. We
    /// plant a peer ledger referencing a digest whose blob is absent, with the
    /// shared-store flag enabled, and assert the cleaner retains everything.
    #[tokio::test]
    async fn collect_shared_roots_retain_all_on_unresolvable_peer_digest() {
        let env = crate::test::env::lock();
        env.set(crate::env::keys::OCX_SHARED_STORE, "true");

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let file_structure = FileStructure::with_root(root.clone());

        // The packages zone root is the parent of `packages/` — where the
        // `roots/` ledger lives (mirrors `collect_shared_root_digests`).
        let packages_zone_root = file_structure
            .packages
            .root()
            .parent()
            .unwrap_or_else(|| file_structure.packages.root())
            .to_path_buf();

        // Plant a peer ledger pinning a digest whose blob is NOT in the store.
        let peer = SharedRoots::new(&packages_zone_root, "instance-peer");
        peer.write("peerproj", &[SHARED_PEER_DIGEST.to_string()]).await.unwrap();

        let result = collect_shared_root_digests(&file_structure, file_structure.state_zone_root())
            .await
            .unwrap();

        assert!(
            matches!(result, CollectedRoots::RetainAll),
            "an unresolvable peer digest (blob absent) must yield RetainAll (fail closed) \
             so the cleaner never deletes the peer's package"
        );
    }

    /// `collect_shared_root_digests` must map a peer ledger entry whose string
    /// does NOT parse as an `oci::Identifier` to `CollectedRoots::RetainAll`.
    /// The version discriminant already gated the ledger as readable + current,
    /// so an unparseable entry signals identifier-grammar skew or corruption —
    /// skipping it would silently drop a peer root and let `clean` delete a live
    /// peer package. This pins the `Identifier::parse` Err arm (the sibling of
    /// the blob-unresolvable arm asserted above).
    ///
    /// `localhost:5000/Peer` fails to parse because the repository component is
    /// uppercase (`UppercaseRepository`) — an unconditional grammar check, so
    /// the input is deterministically rejected. (A naive "garbage" string like
    /// `not a valid identifier` would instead parse as a repo under the default
    /// registry — the parser is lenient — so a real grammar violation is
    /// required to exercise this arm.)
    #[tokio::test]
    async fn collect_shared_roots_retain_all_on_unparseable_peer_identifier() {
        let env = crate::test::env::lock();
        env.set(crate::env::keys::OCX_SHARED_STORE, "true");

        let dir = tempfile::tempdir().unwrap();
        let file_structure = FileStructure::with_root(dir.path().to_path_buf());

        let packages_zone_root = file_structure
            .packages
            .root()
            .parent()
            .unwrap_or_else(|| file_structure.packages.root())
            .to_path_buf();

        // Plant a peer ledger whose entry does not parse as an identifier
        // (uppercase repository — a deterministic grammar violation).
        let peer = SharedRoots::new(&packages_zone_root, "instance-peer");
        peer.write("peerproj", &["localhost:5000/Peer".to_string()])
            .await
            .unwrap();

        let result = collect_shared_root_digests(&file_structure, file_structure.state_zone_root())
            .await
            .unwrap();

        assert!(
            matches!(result, CollectedRoots::RetainAll),
            "an unparseable peer identifier must yield RetainAll (fail closed) so the cleaner \
             never deletes a live peer package against a root set missing that peer"
        );
    }

    /// `collect_shared_root_digests` must map a peer ledger entry that parses as
    /// an `oci::Identifier` but carries no digest (a tag-only reference) to
    /// `CollectedRoots::RetainAll`. This pins the `PinnedIdentifier::try_from`
    /// Err arm: a digestless entry cannot key a package path, so failing closed
    /// avoids dropping a peer root.
    ///
    /// `localhost:5000/peer:1.0` parses cleanly (registry `localhost:5000`, repo
    /// `peer`, tag `1.0`) but has no `@digest`, so `PinnedIdentifier::try_from`
    /// rejects it.
    #[tokio::test]
    async fn collect_shared_roots_retain_all_on_peer_identifier_without_digest() {
        let env = crate::test::env::lock();
        env.set(crate::env::keys::OCX_SHARED_STORE, "true");

        let dir = tempfile::tempdir().unwrap();
        let file_structure = FileStructure::with_root(dir.path().to_path_buf());

        let packages_zone_root = file_structure
            .packages
            .root()
            .parent()
            .unwrap_or_else(|| file_structure.packages.root())
            .to_path_buf();

        // Plant a peer ledger whose entry parses but has no digest component.
        let peer = SharedRoots::new(&packages_zone_root, "instance-peer");
        peer.write("peerproj", &["localhost:5000/peer:1.0".to_string()])
            .await
            .unwrap();

        let result = collect_shared_root_digests(&file_structure, file_structure.state_zone_root())
            .await
            .unwrap();

        assert!(
            matches!(result, CollectedRoots::RetainAll),
            "a digestless peer identifier must yield RetainAll (fail closed) so the cleaner \
             never deletes a live peer package against a root set missing that peer"
        );
    }
}
