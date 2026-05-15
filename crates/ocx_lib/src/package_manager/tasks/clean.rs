// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::file_structure::{FileStructure, StaleEntry};
use crate::log;
use crate::oci;
use crate::project::{ProjectLock, ProjectRegistry};

use super::super::PackageManager;
use super::garbage_collection::{GarbageCollector, ProjectRootDigests};

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
/// error that would abort the operation.
async fn resolve_to_package_digests(
    pinned: &oci::PinnedIdentifier,
    file_structure: &FileStructure,
) -> Vec<oci::PinnedIdentifier> {
    let registry = pinned.registry();
    let digest = pinned.digest();
    let blob_path = file_structure.blobs.data(registry, &digest);

    let blob_bytes = match tokio::fs::read(&blob_path).await {
        Ok(bytes) => bytes,
        Err(_) => {
            // Blob not yet cached locally — return the original pinned id as
            // a best-effort fallback. It will not match any existing package
            // directory, so GC will be unaffected.
            log::debug!("Project root blob not found locally for '{}', using as-is.", pinned);
            return vec![pinned.clone()];
        }
    };

    let manifest: oci::Manifest = match serde_json::from_slice(&blob_bytes) {
        Ok(m) => m,
        Err(e) => {
            log::warn!("Failed to parse manifest blob for '{}': {e}", pinned);
            return vec![pinned.clone()];
        }
    };

    match manifest {
        // Flat image manifest: the locked digest IS the package-store key.
        oci::Manifest::Image(_) => vec![pinned.clone()],

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
                // No children present on disk — fall back to original digest
                // so GC decisions are unaffected.
                log::debug!(
                    "No ImageIndex children found on disk for '{}', using index digest as fallback.",
                    pinned
                );
                vec![pinned.clone()]
            } else {
                resolved
            }
        }
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
pub async fn collect_project_roots(
    ocx_home: &Path,
    file_structure: &FileStructure,
) -> crate::Result<Vec<ProjectRootDigests>> {
    let registry = ProjectRegistry::new(ocx_home);

    // Opportunistic legacy cleanup: the superseded JSON ledger
    // (`projects.json` + its `.projects.lock` advisory sentinel) is obsolete
    // under the flat symlink store. No migration of contents — the symlink
    // ledger is rebuilt by ordinary `ocx.lock` saves. Remove once if present,
    // a single debug line (never WARN — benign legacy artifact).
    let legacy_json = ocx_home.join("projects.json");
    let legacy_lock = ocx_home.join(".projects.lock");
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
    // failure mode removed, not relocated"). A filesystem I/O error remains
    // non-fatal: log and proceed with whatever roots were collected.
    let project_dirs = match registry.live_projects().await {
        Ok(dirs) => dirs,
        Err(e) => {
            log::warn!("Project registry unavailable, running GC without project roots: {e}");
            Vec::new()
        }
    };
    // The ledger targets the project *directory*; the lock is its canonical
    // sibling `<dir>/ocx.lock` (invariant
    // `lock_path_for(config) == <dir>/ocx.lock`, ARCH-4d). Derive the lock
    // path here so the downstream load/resolve pipeline (and the
    // `ProjectRootDigests.ocx_lock_path` diagnostic field) is unchanged.
    let entries: Vec<PathBuf> = project_dirs.into_iter().map(|dir| dir.join("ocx.lock")).collect();

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
    struct LoadedLock {
        index: usize,
        lock_path: PathBuf,
        tools: Vec<crate::project::lock::LockedTool>,
    }

    let mut load_set: JoinSet<Option<LoadedLock>> = JoinSet::new();
    for (index, lock_path) in entries.into_iter().enumerate() {
        load_set.spawn(async move {
            match ProjectLock::from_path(&lock_path).await {
                Ok(Some(lock)) => Some(LoadedLock {
                    index,
                    lock_path,
                    tools: lock.tools,
                }),
                Ok(None) => {
                    log::debug!(
                        "Skipping project root '{}': lock file no longer present.",
                        lock_path.display()
                    );
                    None
                }
                Err(e) => {
                    log::warn!(
                        "Skipping project root '{}': failed to read lock file: {e}",
                        lock_path.display()
                    );
                    None
                }
            }
        });
    }

    let mut loaded: Vec<LoadedLock> = Vec::new();
    while let Some(join) = load_set.join_next().await {
        if let Some(l) = join.expect("collect_project_roots load task panicked") {
            loaded.push(l);
        }
    }

    // Step 2 — resolve every (lock, tool) pair under a bounded semaphore.
    let sem = Arc::new(Semaphore::new(COLLECT_ROOTS_CONCURRENCY));
    let mut resolve_set: JoinSet<(usize, String, String, Vec<oci::PinnedIdentifier>)> = JoinSet::new();
    for loaded_lock in &loaded {
        for tool in &loaded_lock.tools {
            let sem = Arc::clone(&sem);
            let pinned = tool.pinned.clone();
            let group = tool.group.clone();
            let name = tool.name.clone();
            let index = loaded_lock.index;
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

    // Materialise into a per-entry buffer keyed by the load index so we can
    // sort tool-level results by (group, name) inside each entry without
    // depending on JoinSet completion order.
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
    Ok(roots)
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

        // Collect project-registry roots unless --force suppresses the registry.
        let project_roots: Vec<ProjectRootDigests> = if force {
            Vec::new()
        } else {
            collect_project_roots(&ocx_home, self.file_structure()).await?
        };

        let garbage_collector = GarbageCollector::build(self.file_structure(), &project_roots).await?;

        let targets = garbage_collector.unreachable_objects();
        let attribution = garbage_collector.roots_attribution();

        log::debug!(
            "Scanning for unreferenced entries{}: {} candidate(s).",
            if dry_run { " (dry run)" } else { "" },
            targets.len(),
        );

        let raw_objects = garbage_collector.delete_objects(&targets, dry_run).await?;
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

        let temp = clean_temp(self.file_structure(), dry_run).await?;
        Ok(CleanResult { objects, temp })
    }
}

/// Removes stale temp directories and orphan lock files.
///
/// Uses [`TempStore::stale_entries`] which discovers entries from both
/// `.lock` files and directories, acquiring locks where possible to
/// prevent races with concurrent installs.
async fn clean_temp(fs: &crate::file_structure::FileStructure, dry_run: bool) -> crate::Result<Vec<PathBuf>> {
    let stale = fs.temp.stale_entries()?;

    log::debug!(
        "Found {} stale temp entry/entries{}.",
        stale.len(),
        if dry_run { " (dry run)" } else { "" },
    );

    let mut removed = Vec::new();

    for entry in stale {
        match entry {
            StaleEntry::Locked(acquired) => {
                let dir_path = acquired.dir.dir.clone();
                remove_stale_dir(&dir_path, dry_run, "stale").await?;
                // Drop releases the OS lock and auto-deletes the .lock file.
                drop(acquired);
                removed.push(dir_path);
            }
            StaleEntry::Orphan(dir_path) => {
                remove_stale_dir(&dir_path, dry_run, "orphan").await?;
                removed.push(dir_path);
            }
        }
    }

    log::debug!(
        "{} {} stale temp entry/entries.",
        if dry_run { "Would remove" } else { "Removed" },
        removed.len(),
    );

    Ok(removed)
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
