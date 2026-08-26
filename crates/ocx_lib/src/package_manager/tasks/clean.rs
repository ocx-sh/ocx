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
/// `consent` lists swept per-project consent stamps, so a real `ocx clean`
/// never revokes consent silently.
///
/// See [`adr_clean_project_backlinks.md`] for the full data-flow contract.
pub struct CleanResult {
    pub objects: Vec<CleanedObject>,
    pub temp: Vec<PathBuf>,
    /// `state/projects/<key>/` directories swept because the consent stamp
    /// inside recorded a project directory that no longer exists (or, in
    /// dry-run mode, would have been swept). Empty when the run retained
    /// everything.
    pub consent: Vec<PathBuf>,
}

/// Resolve a locked tool's per-platform leaf digests into the set of root
/// digests it pins, presence-gated against every tier the GC roots from.
///
/// Each leaf in `platforms` maps directly to a store key
/// (`repository.clone_with_digest(leaf)`) — no index-blob read needed, since
/// the lock stores platform-manifest digests directly (never the outer
/// image-index digest). Presence-gating means a single-platform machine does
/// not root phantom (never-pulled) platform leaves; see
/// [`leaf_present_in_any_tier`] for why the gate spans two tiers rather than
/// only the package store.
async fn collect_tool_roots(
    repository: &oci::Identifier,
    platforms: &std::collections::BTreeMap<String, oci::Digest>,
    file_structure: &FileStructure,
) -> Vec<oci::PinnedIdentifier> {
    let mut roots = Vec::new();
    for leaf in platforms.values() {
        let child_id = repository.clone_with_digest(leaf.clone());
        let child_pinned = match oci::PinnedIdentifier::try_from(child_id) {
            Ok(p) => p,
            Err(_) => continue,
        };
        if leaf_present_in_any_tier(file_structure, &child_pinned).await {
            roots.push(child_pinned);
        }
    }
    roots
}

/// Is this lock-pinned leaf present on this machine in any tier the GC roots
/// from — materialized as a package, or deferred as a shim?
/// (plan contract C-014, [#302](https://github.com/ocx-sh/ocx/issues/302))
///
/// This is the widened form of the gate [`collect_tool_roots`] applies to every
/// per-platform leaf. The narrow, package-only gate is a **silent shim
/// collector**: a deferred tool has no package directory by construction, so its
/// pin is dropped from [`ProjectRootDigests`] before the reachability graph ever
/// sees it, and the shim directory the lock still pins is collected on the next
/// `ocx clean`.
///
/// It stays a gate rather than becoming unconditional because the gate is what
/// keeps a phantom (never-pulled, foreign-platform) leaf out of the dry-run
/// report — `PackageManager::clean` turns every attribution key into a row. The
/// two tiers are probed independently and the graph re-checks each against the
/// walked entry set, so a pin present in one tier and absent in the other roots
/// only the tier that exists.
async fn leaf_present_in_any_tier(file_structure: &FileStructure, leaf: &oci::PinnedIdentifier) -> bool {
    crate::utility::fs::path_exists_lossy(&file_structure.packages.path(leaf)).await
        || crate::utility::fs::path_exists_lossy(&file_structure.shims.path(leaf)).await
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
/// The `file_structure` parameter is used to presence-gate each locked
/// tool's per-platform leaf digests against the on-disk package store, so
/// `ProjectRootDigests::digests` contains only the digests that actually map
/// to package-store paths on this machine.
///
/// Uses [`crate::project::registry::ProjectRegistry::live_projects`] — the flat
/// symlink store at `$OCX_HOME/projects/` (ADR: `adr_project_gc_symlink_ledger.md`).
/// There is no JSON parse surface: stale/broken links are silently pruned.
/// A corrupt-registry exit-78 branch is deliberately absent — eliminated with
/// the JSON ledger (ADR §Risks "Corrupt-state failure mode removed, not relocated").
pub async fn collect_project_roots(ocx_home: &Path, file_structure: &FileStructure) -> crate::Result<CollectedRoots> {
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
            let repository = tool.repository.clone();
            let platforms = tool.platforms.clone();
            let group = tool.group.clone();
            let name = tool.name.clone();
            // Dense post-filter position in `loaded` (Bug-R3): the resolve
            // buckets are sized `loaded.len()`, so the key MUST be the
            // survivor's dense index here, never the original `entries`
            // enumerate index (which spans `LockLoad::Absent` entries too and
            // would index `buckets` out of bounds).
            // `collect_tool_roots` borrows `&FileStructure`. Cloning is
            // cheap (the struct holds `Arc`-shared sub-stores).
            let fs = file_structure.clone();
            resolve_set.spawn(async move {
                let _permit = sem.acquire_owned().await.expect("semaphore closed");
                let resolved = collect_tool_roots(&repository, &platforms, &fs).await;
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

        // Collect project-registry roots unless --force suppresses the
        // registry. A transiently-unreachable *live* root makes the root set
        // indeterminate (plan A1/A2): fail closed by retaining every object
        // this run — skip object collection entirely and only sweep stale
        // temps. The run stays non-fatal (exit 0); `--force` is the
        // sanctioned override to GC against live install symlinks alone.
        let project_roots: Vec<ProjectRootDigests> = if force {
            Vec::new()
        } else {
            match collect_project_roots(&ocx_home, self.file_structure()).await? {
                CollectedRoots::Roots(roots) => roots,
                CollectedRoots::RetainAll => {
                    // Retain-all covers the consent stamps too: the run has
                    // already decided the store's liveness picture is
                    // untrustworthy, and over-retention is this sweep's safe
                    // direction. The next healthy run sweeps them.
                    let temp = clean_temp(self.file_structure(), dry_run).await?;
                    return Ok(CleanResult {
                        objects: Vec::new(),
                        temp,
                        consent: Vec::new(),
                    });
                }
            }
        };

        let host_platform = oci::Platform::current().unwrap_or_else(oci::Platform::any);
        // Retention, not observation: compose resolves a companion (and a
        // descriptor) snapshot-first, so an active freeze's pins are roots even
        // after an `ocx patch sync` has advanced the live record past them.
        let patch_roots = self
            .resolve_site_patch_roots(&host_platform, super::resolve::PatchRootScope::RecordedAndSnapshot)
            .await?;
        let garbage_collector = GarbageCollector::build(self.file_structure(), &project_roots, &patch_roots).await?;

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
        // The one exception to `state/` not being walked by `ocx clean`, and
        // the one call site that walks it. Runs under `--force` too: `--force`
        // waives the *project registry*, and this sweep never consults it —
        // its guards are the stamp's own recorded `project_dir`.
        let consent = sweep_consent_stamps(&self.file_structure().state, dry_run).await?;
        Ok(CleanResult { objects, temp, consent })
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

/// Prefix marking a half-written staging directory under the sweep root.
const STAGING_PREFIX: &str = ".tmp-";

/// Stamp schema version this binary understands.
// `ConsentStamp.v` is a bare `u8` owned by `project::consent` (WP-6), so the
// understood set is stated here rather than shared. The drift direction is
// safe: a bumped stamp version this sweep does not know about is retained, not
// collected. If WP-6 ever exports a version constant — or types `v` as a
// `serde_repr` enum, which moves the gate into the deserializer — this should
// point at it instead.
const UNDERSTOOD_STAMP_VERSION: u8 = 1;

/// Three-state outcome of probing a stamp's recorded `project_dir`.
///
/// The [`DirProbe::Absent`] / [`DirProbe::Indeterminate`] split is the whole
/// guard: collapsing a transient `Err` into "gone" makes the sweep delete
/// consent during a permission flip or an unreachable mount, and a project
/// whose consent is deleted goes inert (A-31).
enum DirProbe {
    /// Something exists at the recorded path — the project is not departed.
    Present,
    /// An `Ok` probe proved nothing exists at the recorded path.
    Absent,
    /// A non-`NotFound` I/O error. Liveness is unknown; the stamp is retained.
    Indeterminate,
}

/// Probes a stamp's recorded `project_dir`.
///
/// Uses `symlink_metadata`, not `metadata`: a dangling symlink left where the
/// project directory used to be is *something*, and retaining is the safe
/// direction on every ambiguity.
async fn probe_project_dir(project_dir: &Path) -> DirProbe {
    match tokio::fs::symlink_metadata(project_dir).await {
        Ok(_) => DirProbe::Present,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => DirProbe::Absent,
        Err(_) => DirProbe::Indeterminate,
    }
}

/// Removes every `state/projects/<key>/` whose consent stamp records a
/// `project_dir` that no longer exists, and returns the swept directories
/// sorted.
///
/// This is the **one** exception to `state/` not being walked by `ocx clean`.
/// Removal requires *both*: the stamp deserializes at a `v` this binary
/// understands, and a probe immediately before removal proves its recorded
/// `project_dir` definitively absent. Unreadable, malformed, unknown-`v`,
/// symlinked, `.tmp-*`-staged and indeterminate-probe entries are all retained,
/// one line at debug each.
///
/// Honours `dry_run` on the same terms as every other removal path: a dry run
/// reports what it would sweep and removes nothing.
///
/// # Errors
///
/// Propagates a removal I/O failure, matching [`remove_stale_dir`]. Failing to
/// *enumerate* the sweep root is not an error — the tree is deletable at any
/// time and an unreadable root means "sweep nothing this run".
async fn sweep_consent_stamps(state: &crate::file_structure::StateStore, dry_run: bool) -> crate::Result<Vec<PathBuf>> {
    let sweep_root = state.project_state_root();
    let mut read_dir = match tokio::fs::read_dir(&sweep_root).await {
        Ok(entries) => entries,
        // An absent sweep root is the ordinary state of a home that has never
        // stamped a project — never a warning. Any other enumeration failure
        // retains everything, which is this sweep's safe direction.
        Err(e) => {
            log::debug!(
                "Consent-stamp sweep: '{}' not enumerable, sweeping nothing this run: {e}",
                sweep_root.display()
            );
            return Ok(Vec::new());
        }
    };

    // Deliberately NOT `{ name_for_path(dir) | dir in live_projects() }`: the
    // ledger re-derives a key it already stores as the entry filename, and its
    // population rule is strictly narrower than the consent writers' — an
    // `[env]`-only project is never in the ledger, so a ledger-derived sweep
    // would revoke its consent on every `ocx clean`, silently, forever.
    let mut candidates: Vec<(PathBuf, PathBuf)> = Vec::new();
    loop {
        let entry = match read_dir.next_entry().await {
            Ok(Some(entry)) => entry,
            Ok(None) => break,
            Err(e) => {
                log::debug!(
                    "Consent-stamp sweep: stopping enumeration of '{}': {e}",
                    sweep_root.display()
                );
                break;
            }
        };

        // A key is 16 hex characters, so a non-UTF-8 entry name is not one —
        // and routing the two paths through the accessors keeps the layout
        // (directory name, stamp filename) owned solely by `StateStore`.
        let Some(key) = entry.file_name().to_str().map(str::to_owned) else {
            log::debug!(
                "Consent-stamp sweep: skipping '{}' — entry name is not a project key.",
                entry.path().display()
            );
            continue;
        };
        if key.starts_with(STAGING_PREFIX) {
            log::debug!("Consent-stamp sweep: skipping staging entry '{key}'.");
            continue;
        }
        let state_dir = state.project_state_dir(&key);

        // A symlinked state directory is skipped, never followed: `remove_dir_all`
        // through a link deletes whatever it points at.
        match tokio::fs::symlink_metadata(&state_dir).await {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                log::debug!(
                    "Consent-stamp sweep: skipping '{}' — not a real directory.",
                    state_dir.display()
                );
                continue;
            }
            Err(e) => {
                log::debug!(
                    "Consent-stamp sweep: retaining '{}', unreadable: {e}",
                    state_dir.display()
                );
                continue;
            }
        }

        let Some(project_dir) = read_stamped_project_dir(&state.consent_stamp_file(&key)).await else {
            continue;
        };
        match probe_project_dir(&project_dir).await {
            DirProbe::Absent => candidates.push((state_dir, project_dir)),
            DirProbe::Present => {}
            DirProbe::Indeterminate => log::debug!(
                "Consent-stamp sweep: retaining '{}' — liveness of '{}' is indeterminate.",
                state_dir.display(),
                project_dir.display()
            ),
        }
    }

    // Deterministic output: readdir order never reaches the caller.
    candidates.sort();

    remove_departed_stamps(candidates, dry_run).await
}

/// Removes the classified candidates, re-probing each one immediately before
/// it is removed, and returns what was swept in the order given.
///
/// Split out of [`sweep_consent_stamps`] because the re-probe is the whole
/// point of this half and it defends against a window that only exists
/// *between* the two functions: the classification loop proved `project_dir`
/// absent at some earlier instant, and the project may have been recreated
/// since (the CODEX-BLOCK-1 TOCTOU pattern, `project/registry.rs:105-133`).
/// Only a second definitive [`DirProbe::Absent`] is acted on — `Present` and
/// `Indeterminate` both retain, because deleting consent makes a live project
/// go inert (A-31) and over-retention is this sweep's safe direction.
///
/// Taking the candidate list as a parameter is what makes that window
/// reachable from a test: no in-process schedule can recreate a directory
/// between two `await`s inside one function, so a test that has to observe the
/// second probe has to be able to change the world between the two.
///
/// # Errors
///
/// Propagates a removal I/O failure, matching [`remove_stale_dir`].
async fn remove_departed_stamps(candidates: Vec<(PathBuf, PathBuf)>, dry_run: bool) -> crate::Result<Vec<PathBuf>> {
    let mut swept = Vec::new();
    for (state_dir, project_dir) in candidates {
        match probe_project_dir(&project_dir).await {
            DirProbe::Absent => {}
            DirProbe::Present | DirProbe::Indeterminate => {
                log::debug!(
                    "Consent-stamp sweep: retaining '{}' — '{}' is no longer definitively absent.",
                    state_dir.display(),
                    project_dir.display()
                );
                continue;
            }
        }

        log::info!(
            "{} consent stamp for departed project '{}': {}",
            if dry_run { "Would remove" } else { "Removing" },
            project_dir.display(),
            state_dir.display(),
        );
        if !dry_run {
            tokio::fs::remove_dir_all(&state_dir)
                .await
                .map_err(|e| crate::Error::InternalFile(state_dir.clone(), e))?;
        }
        swept.push(state_dir);
    }

    Ok(swept)
}

/// Reads a consent stamp and returns the `project_dir` it records, or `None`
/// when the stamp is unusable.
///
/// An unusable stamp — absent, unreadable, malformed, or at a `v` this binary
/// does not understand — is retained by the sweep: "I cannot read it" is not
/// "it is garbage", and an unusable stamp is already inert at `evaluate`, so
/// collecting it buys nothing while deleting consent a newer or rolled-back
/// binary wrote costs a working project (A-31).
async fn read_stamped_project_dir(stamp_path: &Path) -> Option<PathBuf> {
    let bytes = match tokio::fs::read(stamp_path).await {
        Ok(bytes) => bytes,
        Err(e) => {
            log::debug!(
                "Consent-stamp sweep: retaining '{}' — unreadable: {e}",
                stamp_path.display()
            );
            return None;
        }
    };
    let stamp: crate::project::consent::ConsentStamp = match serde_json::from_slice(&bytes) {
        Ok(stamp) => stamp,
        Err(e) => {
            log::debug!(
                "Consent-stamp sweep: retaining '{}' — stamp does not deserialize: {e}",
                stamp_path.display()
            );
            return None;
        }
    };
    if stamp.v != UNDERSTOOD_STAMP_VERSION {
        log::debug!(
            "Consent-stamp sweep: retaining '{}' — stamp version {} is not understood by this binary.",
            stamp_path.display(),
            stamp.v
        );
        return None;
    }
    Some(stamp.project_dir)
}

// ── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_structure::FileStructure;

    // Minimal valid V3 ocx.lock that `ProjectLock::from_path` can parse.
    //
    // The `declaration_hash` value is not validated on load — only
    // `declaration_hash_version` is checked. `repository` is the bare
    // registry/repo coordinate; each `[tool.platforms]` entry is a leaf
    // digest keyed by the canonical grammar `Platform` string (D2).
    //
    // Registry must contain `.` or `:` or be "localhost" to be parsed as an
    // explicit registry (see `oci::identifier::has_explicit_registry`).
    // Using `localhost:5000` which carries a colon and is always valid.
    const LOCK_WITH_ONE_TOOL: &str = r#"
[metadata]
lock_version = 3
declaration_hash_version = 1
declaration_hash = "sha256:0000000000000000000000000000000000000000000000000000000000000000"
generated_by = "ocx test"
generated_at = "2026-01-01T00:00:00Z"

[[tool]]
name = "cmake"
group = "default"
repository = "localhost:5000/cmake"

[tool.platforms]
"linux/amd64" = "sha256:aaaa0000000000000000000000000000000000000000000000000000000000bb"
"#;

    // A second distinct tool + leaf digest used in multi-tool fixtures.
    const LOCK_WITH_TWO_TOOLS: &str = r#"
[metadata]
lock_version = 3
declaration_hash_version = 1
declaration_hash = "sha256:0000000000000000000000000000000000000000000000000000000000000000"
generated_by = "ocx test"
generated_at = "2026-01-01T00:00:00Z"

[[tool]]
name = "cmake"
group = "default"
repository = "localhost:5000/cmake"

[tool.platforms]
"linux/amd64" = "sha256:aaaa0000000000000000000000000000000000000000000000000000000000bb"

[[tool]]
name = "shfmt"
group = "default"
repository = "localhost:5000/shfmt"

[tool.platforms]
"linux/amd64" = "sha256:bbbb0000000000000000000000000000000000000000000000000000000000cc"
"#;

    /// Build the `PinnedIdentifier` a `repository`/leaf-digest pair in the
    /// fixtures above resolves to, and pre-create its package-store
    /// directory so `collect_tool_roots`'s presence gate passes.
    ///
    /// `collect_tool_roots` only checks the path exists (no metadata/resolve
    /// validity needed — that's the GC reachability walker's concern, not
    /// this presence gate).
    async fn seed_pinned_package_dir(
        file_structure: &FileStructure,
        repository: &str,
        registry: &str,
        digest_hex: &str,
    ) -> oci::PinnedIdentifier {
        let pinned = pinned_leaf(repository, registry, digest_hex);
        tokio::fs::create_dir_all(file_structure.packages.path(&pinned))
            .await
            .unwrap();
        pinned
    }

    /// The `PinnedIdentifier` a `repository`/leaf-digest pair in the fixtures
    /// above resolves to, with nothing seeded on disk.
    fn pinned_leaf(repository: &str, registry: &str, digest_hex: &str) -> oci::PinnedIdentifier {
        oci::PinnedIdentifier::try_from(
            oci::Identifier::new_registry(repository, registry)
                .clone_with_digest(oci::Digest::Sha256(digest_hex.to_string())),
        )
        .unwrap()
    }

    /// The leaf digest both lock fixtures pin for `cmake`.
    const CMAKE_LEAF_HEX: &str = "aaaa0000000000000000000000000000000000000000000000000000000000bb";

    /// Pre-create the shim directory for `pinned` — its `bin/` child is the
    /// completeness marker (C-022), and its presence is the whole on-disk
    /// evidence a deferred tool leaves behind.
    async fn seed_pinned_shim_dir(file_structure: &FileStructure, pinned: &oci::PinnedIdentifier) {
        tokio::fs::create_dir_all(file_structure.shims.shim_dir(pinned).bin())
            .await
            .unwrap();
    }

    // ── C-014: the presence gate accepts either tier ──────────────────────

    /// C-014 trap (2): a deferred tool has no package directory by
    /// construction. The package-only gate drops its pin before the
    /// reachability graph ever sees it, and the shim the lock still pins is
    /// collected on the next `ocx clean`.
    #[tokio::test]
    async fn leaf_present_in_any_tier_accepts_a_deferred_leaf_with_only_a_shim_dir() {
        let dir = tempfile::tempdir().unwrap();
        let file_structure = FileStructure::with_root(dir.path().to_path_buf());
        let leaf = pinned_leaf("cmake", "localhost:5000", CMAKE_LEAF_HEX);
        seed_pinned_shim_dir(&file_structure, &leaf).await;

        assert!(
            !file_structure.packages.path(&leaf).exists(),
            "precondition: nothing is materialized — this is the 'install nothing, compose \
             lazily' state"
        );
        assert!(
            leaf_present_in_any_tier(&file_structure, &leaf).await,
            "C-014: a shim directory satisfies presence for a lock-pinned leaf"
        );
    }

    /// The widened gate must not lose the tier it already had.
    #[tokio::test]
    async fn leaf_present_in_any_tier_accepts_a_materialized_leaf_with_only_a_package_dir() {
        let dir = tempfile::tempdir().unwrap();
        let file_structure = FileStructure::with_root(dir.path().to_path_buf());
        let leaf = seed_pinned_package_dir(&file_structure, "cmake", "localhost:5000", CMAKE_LEAF_HEX).await;

        assert!(
            !file_structure.shims.path(&leaf).exists(),
            "precondition: no shim exists for a materialized tool"
        );
        assert!(
            leaf_present_in_any_tier(&file_structure, &leaf).await,
            "a package directory still satisfies presence"
        );
    }

    /// It stays a **gate**: a foreign-platform or never-pulled leaf is absent
    /// from both tiers and must not become a root, or `PackageManager::clean`
    /// renders a nonexistent path as a held object in `--dry-run`.
    #[tokio::test]
    async fn leaf_present_in_any_tier_rejects_a_leaf_absent_from_both_tiers() {
        let dir = tempfile::tempdir().unwrap();
        let file_structure = FileStructure::with_root(dir.path().to_path_buf());
        let leaf = pinned_leaf("cmake", "localhost:5000", CMAKE_LEAF_HEX);

        assert!(
            !leaf_present_in_any_tier(&file_structure, &leaf).await,
            "a pin present in neither tier is not present"
        );
    }

    /// The same contract one layer up, where it actually bites: with only a
    /// shim directory on disk, `collect_project_roots` must still surface the
    /// lock's pinned digest as a GC root.
    #[tokio::test]
    async fn collect_roots_includes_a_deferred_tools_leaf_with_only_a_shim_dir() {
        let dir = tempfile::tempdir().unwrap();
        let ocx_home = dir.path().to_path_buf();
        tokio::fs::write(ocx_home.join("ocx.lock"), LOCK_WITH_ONE_TOOL)
            .await
            .unwrap();
        tokio::fs::create_dir_all(ocx_home.join("projects")).await.unwrap();

        let file_structure = FileStructure::with_root(ocx_home.clone());
        let leaf = pinned_leaf("cmake", "localhost:5000", CMAKE_LEAF_HEX);
        seed_pinned_shim_dir(&file_structure, &leaf).await;
        assert!(
            !file_structure.packages.path(&leaf).exists(),
            "precondition: the tool is deferred, not materialized"
        );

        let roots = match collect_project_roots(&ocx_home, &file_structure).await.unwrap() {
            CollectedRoots::Roots(roots) => roots,
            CollectedRoots::RetainAll => panic!("expected Roots, got RetainAll"),
        };

        let digest_strs: Vec<String> = roots
            .iter()
            .flat_map(|root| root.digests.iter().map(|pinned| pinned.to_string()))
            .collect();
        assert!(
            digest_strs.iter().any(|entry| entry.contains("sha256:aaaa0000")),
            "C-014: a deferred tool's pin must survive the presence gate; got: {digest_strs:?}"
        );
    }

    /// Negative control for the widened gate: with neither tier on disk the
    /// pin is still dropped. An unconditional gate would pass this pin through
    /// and put a path that does not exist into the dry-run report.
    #[tokio::test]
    async fn collect_roots_excludes_a_leaf_absent_from_both_tiers() {
        let dir = tempfile::tempdir().unwrap();
        let ocx_home = dir.path().to_path_buf();
        tokio::fs::write(ocx_home.join("ocx.lock"), LOCK_WITH_ONE_TOOL)
            .await
            .unwrap();
        tokio::fs::create_dir_all(ocx_home.join("projects")).await.unwrap();

        let file_structure = FileStructure::with_root(ocx_home.clone());
        let roots = match collect_project_roots(&ocx_home, &file_structure).await.unwrap() {
            CollectedRoots::Roots(roots) => roots,
            CollectedRoots::RetainAll => panic!("expected Roots, got RetainAll"),
        };

        assert_eq!(roots.len(), 1, "the global lock still contributes an entry");
        assert!(
            roots[0].digests.is_empty(),
            "but it pins nothing on this machine; got: {:?}",
            roots[0].digests.iter().map(|p| p.to_string()).collect::<Vec<_>>()
        );
    }

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
        // The per-platform path presence-gates against the package store —
        // seed the leaf's package directory so the gate passes.
        seed_pinned_package_dir(
            &file_structure,
            "cmake",
            "localhost:5000",
            "aaaa0000000000000000000000000000000000000000000000000000000000bb",
        )
        .await;
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
        // The per-platform path presence-gates against the package store —
        // seed both leaves' package directories so the gate passes.
        seed_pinned_package_dir(
            &file_structure,
            "cmake",
            "localhost:5000",
            "aaaa0000000000000000000000000000000000000000000000000000000000bb",
        )
        .await;
        seed_pinned_package_dir(
            &file_structure,
            "shfmt",
            "localhost:5000",
            "bbbb0000000000000000000000000000000000000000000000000000000000cc",
        )
        .await;
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

    // ── consent-stamp sweep (C-023, S-033, A-31) ─────────────────────────────

    /// Writes a stamp at `state/projects/<key>/consent.json` whose recorded
    /// `project_dir` is `project_dir`, and returns the state directory.
    fn write_stamp(state: &crate::file_structure::StateStore, key: &str, version: u8, project_dir: &Path) -> PathBuf {
        let dir = state.project_state_dir(key);
        std::fs::create_dir_all(&dir).unwrap();
        let stamp = format!(
            r#"{{"v":{version},"project_dir":{dir_json},"sources":["ocx.sh/ocx"],"stamped_at":"2026-01-01T00:00:00Z"}}"#,
            dir_json = serde_json::to_string(project_dir).unwrap()
        );
        std::fs::write(state.consent_stamp_file(key), stamp).unwrap();
        dir
    }

    /// C-023 / S-033(b) — a stamp whose `project_dir` is gone is collected, and
    /// the reported paths are sorted (DATA-DET: filesystem readdir order never
    /// reaches the caller).
    /// EC-IDENT-006 — a stamp whose recorded project_dir no longer exists is collected.
    #[tokio::test]
    async fn consent_sweep_collects_stamps_whose_project_dir_is_gone() {
        let tmp = tempfile::tempdir().unwrap();
        let state = crate::file_structure::StateStore::new(tmp.path().join("state"));
        let gone = tmp.path().join("departed");
        let first = write_stamp(&state, "aaaa000000000000", 1, &gone);
        let second = write_stamp(&state, "bbbb000000000000", 1, &gone);

        let swept = sweep_consent_stamps(&state, false).await.unwrap();

        assert_eq!(
            swept,
            vec![first.clone(), second.clone()],
            "both stamps swept, in sorted order"
        );
        assert!(!first.exists(), "a stamp for a departed project must be removed");
        assert!(!second.exists(), "a stamp for a departed project must be removed");
    }

    /// C-023 / S-033(a) — the `[env]`-only project: a live `project_dir` with no
    /// `ocx.lock` and therefore no ledger entry. The sweep reads the stamp's own
    /// `project_dir`, never the ledger, so this stamp survives.
    /// EC-IDENT-007 — an `[env]`-only project has no ocx.lock and can never be listed by live_projects; its stamp is retained anyway.
    #[tokio::test]
    async fn consent_sweep_retains_a_stamp_whose_project_dir_is_live_without_a_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let state = crate::file_structure::StateStore::new(tmp.path().join("state"));
        let live = tmp.path().join("env-only-project");
        std::fs::create_dir_all(&live).unwrap();
        std::fs::write(live.join("ocx.toml"), b"[env]\n").unwrap();
        assert!(
            !live.join("ocx.lock").exists(),
            "fixture must have no lock (no ledger entry)"
        );
        let dir = write_stamp(&state, "aaaa000000000000", 1, &live);

        let swept = sweep_consent_stamps(&state, false).await.unwrap();

        assert!(
            swept.is_empty(),
            "a live project's stamp must not be swept; got {swept:?}"
        );
        assert!(dir.exists(), "a live project's stamp must survive the sweep");
    }

    /// C-023 — **the assigned fault-injection guard.** `--dry-run` reports what
    /// it would remove and removes nothing.
    ///
    /// `assert!(dir.exists())` is the assertion a `dry_run`-ignoring sweep flips.
    #[tokio::test]
    async fn consent_sweep_dry_run_reports_without_removing() {
        let tmp = tempfile::tempdir().unwrap();
        let state = crate::file_structure::StateStore::new(tmp.path().join("state"));
        let gone = tmp.path().join("departed");
        let dir = write_stamp(&state, "aaaa000000000000", 1, &gone);

        let swept = sweep_consent_stamps(&state, true).await.unwrap();

        assert_eq!(
            swept,
            vec![dir.clone()],
            "a dry run still reports the stamp it would sweep"
        );
        assert!(dir.exists(), "a dry run must not remove the consent stamp");
        assert!(
            state.consent_stamp_file("aaaa000000000000").exists(),
            "a dry run must not remove the stamp file"
        );
    }

    /// A-31 — a stamp that does not deserialize is RETAINED. "I cannot read it"
    /// is not "it is garbage"; the recorded `project_dir` is gone here, so only
    /// the parse precondition can save it.
    #[tokio::test]
    async fn consent_sweep_retains_a_malformed_stamp() {
        let tmp = tempfile::tempdir().unwrap();
        let state = crate::file_structure::StateStore::new(tmp.path().join("state"));
        let dir = state.project_state_dir("aaaa000000000000");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(state.consent_stamp_file("aaaa000000000000"), b"{not json").unwrap();

        let swept = sweep_consent_stamps(&state, false).await.unwrap();

        assert!(swept.is_empty(), "a malformed stamp must be retained; got {swept:?}");
        assert!(dir.exists(), "a malformed stamp must be retained");
    }

    /// A-31 — a stamp at a `v` this binary does not understand is RETAINED even
    /// though its `project_dir` is definitively gone. Under-retention would
    /// delete consent a newer or rolled-back binary wrote.
    #[tokio::test]
    async fn consent_sweep_retains_an_unknown_version_stamp() {
        let tmp = tempfile::tempdir().unwrap();
        let state = crate::file_structure::StateStore::new(tmp.path().join("state"));
        let gone = tmp.path().join("departed");
        let dir = write_stamp(&state, "aaaa000000000000", 2, &gone);

        let swept = sweep_consent_stamps(&state, false).await.unwrap();

        assert!(swept.is_empty(), "an unknown-`v` stamp must be retained; got {swept:?}");
        assert!(dir.exists(), "an unknown-`v` stamp must be retained");
    }

    /// A-31 — a stamp whose bytes cannot be read is RETAINED (transient
    /// permission or I/O fault). Skipped when the process can read a `0o000`
    /// file anyway — an observed cause, not an assumed one.
    #[cfg(unix)]
    /// EC-IDENT-011 — an unreadable stamp is retained, not collected: a transient permission flip must not revoke consent.
    #[tokio::test]
    async fn consent_sweep_retains_an_unreadable_stamp() {
        use std::os::unix::fs::PermissionsExt as _;

        let tmp = tempfile::tempdir().unwrap();
        let state = crate::file_structure::StateStore::new(tmp.path().join("state"));
        let gone = tmp.path().join("departed");
        let dir = write_stamp(&state, "aaaa000000000000", 1, &gone);
        let stamp = state.consent_stamp_file("aaaa000000000000");
        std::fs::set_permissions(&stamp, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&stamp).is_ok() {
            eprintln!("skipping: this process reads a 0o000 file (running as root), so the fault cannot be staged");
            return;
        }

        let swept = sweep_consent_stamps(&state, false).await.unwrap();

        assert!(swept.is_empty(), "an unreadable stamp must be retained; got {swept:?}");
        assert!(dir.exists(), "an unreadable stamp must be retained");
    }

    /// C-023 guard 1 — a symlinked state directory is skipped, never followed
    /// into `remove_dir_all`.
    #[cfg(unix)]
    /// EC-IDENT-008 — a symlinked state directory is skipped, never followed.
    #[tokio::test]
    async fn consent_sweep_skips_a_symlinked_state_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let state = crate::file_structure::StateStore::new(tmp.path().join("state"));
        let gone = tmp.path().join("departed");
        // A sweepable stamp, staged outside the sweep root and reachable only
        // through a symlink named like a key.
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(
            elsewhere.join("consent.json"),
            format!(
                r#"{{"v":1,"project_dir":{d},"sources":[],"stamped_at":"2026-01-01T00:00:00Z"}}"#,
                d = serde_json::to_string(&gone).unwrap()
            ),
        )
        .unwrap();
        std::fs::create_dir_all(state.project_state_root()).unwrap();
        let link = state.project_state_dir("aaaa000000000000");
        std::os::unix::fs::symlink(&elsewhere, &link).unwrap();

        let swept = sweep_consent_stamps(&state, false).await.unwrap();

        assert!(swept.is_empty(), "a symlinked state dir must be skipped; got {swept:?}");
        assert!(link.exists(), "the symlink itself must survive");
        assert!(
            elsewhere.join("consent.json").exists(),
            "the sweep must never follow a symlink into remove_dir_all"
        );
    }

    /// C-023 guard 3 — `.tmp-*` staging names are skipped.
    #[tokio::test]
    async fn consent_sweep_skips_tmp_staging_names() {
        let tmp = tempfile::tempdir().unwrap();
        let state = crate::file_structure::StateStore::new(tmp.path().join("state"));
        let gone = tmp.path().join("departed");
        let staging = write_stamp(&state, ".tmp-aaaa000000000000", 1, &gone);

        let swept = sweep_consent_stamps(&state, false).await.unwrap();

        assert!(swept.is_empty(), "a staging name must be skipped; got {swept:?}");
        assert!(staging.exists(), "a staging name must be skipped, not swept");
    }

    /// EC-IDENT-010 — the TOCTOU re-probe: a project recreated **after** the
    /// walk classified it as departed, but **before** the removal, is retained.
    ///
    /// The window is real, not theoretical: `ocx clean` enumerates the whole
    /// sweep root and stats every recorded `project_dir` before it removes the
    /// first stamp, so a `git clone` (or a restored mount, or a checkout that
    /// was mid-`git switch`) landing in that interval would otherwise have its
    /// consent revoked — and a project whose consent is deleted goes inert
    /// (A-31), silently, with no error anywhere.
    ///
    /// [`remove_departed_stamps`] is called directly because that is the only
    /// way to *be* in the window: the two probes live in different functions
    /// precisely so a test can change the world between them, and no in-process
    /// schedule can recreate a directory between two `await`s inside one.
    ///
    /// The classification the shipped walk would have reached is asserted
    /// first, so the fixture cannot pass by never having been a candidate —
    /// this test is about what the SECOND probe does, and a stamp that was
    /// never `Absent` would exercise neither.
    ///
    /// Red state: drop the re-probe from [`remove_departed_stamps`] and the
    /// live project's stamp is swept.
    #[tokio::test]
    async fn consent_sweep_retains_a_project_recreated_inside_the_toctou_window() {
        let tmp = tempfile::tempdir().unwrap();
        let state = crate::file_structure::StateStore::new(tmp.path().join("state"));
        let recreated = tmp.path().join("recreated");
        let stamp_dir = write_stamp(&state, "aaaa000000000000", 1, &recreated);

        // What the classification loop sees: definitively absent, so this stamp
        // becomes a removal candidate.
        assert!(
            matches!(probe_project_dir(&recreated).await, DirProbe::Absent),
            "the fixture must start as a genuine removal candidate"
        );
        let candidates = vec![(stamp_dir.clone(), recreated.clone())];

        // The window: the project comes back before the removal loop runs.
        std::fs::create_dir(&recreated).unwrap();

        let swept = remove_departed_stamps(candidates, false).await.unwrap();

        assert!(
            swept.is_empty(),
            "a project recreated inside the TOCTOU window must not be swept; got {swept:?}"
        );
        assert!(
            stamp_dir.exists(),
            "the recreated project's consent stamp must survive — deleting it makes a live \
             project go inert"
        );
    }

    /// EC-IDENT-010, the other side of the window: a candidate whose recorded
    /// `project_dir` is *still* absent at the re-probe IS removed.
    ///
    /// Without this, the retention test above is satisfied by a
    /// [`remove_departed_stamps`] that never removes anything at all — a green
    /// indistinguishable from the function having been gutted.
    #[tokio::test]
    async fn consent_sweep_removes_a_candidate_still_absent_at_the_re_probe() {
        let tmp = tempfile::tempdir().unwrap();
        let state = crate::file_structure::StateStore::new(tmp.path().join("state"));
        let gone = tmp.path().join("departed");
        let stamp_dir = write_stamp(&state, "aaaa000000000000", 1, &gone);

        let swept = remove_departed_stamps(vec![(stamp_dir.clone(), gone)], false)
            .await
            .unwrap();

        assert_eq!(swept, vec![stamp_dir.clone()], "a still-departed candidate is swept");
        assert!(!stamp_dir.exists(), "a still-departed candidate's stamp is removed");
    }

    /// C-023 guard 4 — an indeterminate probe of `project_dir` retains the
    /// stamp. The fixture stages `ENAMETOOLONG` (a 300-byte component), which
    /// is an `Err` that is not `NotFound` for every user including root.
    #[cfg(unix)]
    /// EC-IDENT-009 — an indeterminate liveness probe retains; only a determinate miss collects.
    #[tokio::test]
    async fn consent_sweep_retains_on_an_indeterminate_probe() {
        let tmp = tempfile::tempdir().unwrap();
        let state = crate::file_structure::StateStore::new(tmp.path().join("state"));
        let indeterminate = tmp.path().join("x".repeat(300));
        let probe = std::fs::symlink_metadata(&indeterminate).unwrap_err();
        assert_ne!(
            probe.kind(),
            std::io::ErrorKind::NotFound,
            "fixture must stage an indeterminate probe, not an absent one; got {probe:?}"
        );
        let dir = write_stamp(&state, "aaaa000000000000", 1, &indeterminate);

        let swept = sweep_consent_stamps(&state, false).await.unwrap();

        assert!(swept.is_empty(), "an indeterminate probe must retain; got {swept:?}");
        assert!(dir.exists(), "an indeterminate probe must retain the stamp");
    }

    /// C-023 — no `state/projects/` yet is the ordinary state of a fresh home:
    /// an empty sweep, no error, no warning.
    #[tokio::test]
    async fn consent_sweep_is_empty_when_the_sweep_root_is_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let state = crate::file_structure::StateStore::new(tmp.path().join("state"));

        let swept = sweep_consent_stamps(&state, false).await.unwrap();

        assert!(swept.is_empty(), "an absent sweep root yields nothing; got {swept:?}");
    }
}
