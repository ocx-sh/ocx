// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use ocx_project::{ProjectLock, ProjectRegistry};
use ocx_store::file_structure::{
    DEFAULT_SHELL, FileStructure, RenderStampScope, RenderStampTarget, RenderStampV2, StaleEntry, StateStore,
    ToolchainHome,
};
use ocx_store::launcher_generation::LauncherGeneration;
use ocx_store::reference_manager::ReferenceManager;

use super::super::PackageManager;
use super::garbage_collection::{GarbageCollector, ProjectRootDigests, current_deferred};
use super::render_toolchain::{ScopedLockParameters, refuse_symlinked_home};

/// Bound on concurrent tool resolutions in [`collect_project_roots`].
const COLLECT_ROOTS_CONCURRENCY: usize = 50;

/// A single object-store entry surfaced by `ocx clean`.
#[derive(Debug, Clone)]
pub struct CleanedObject {
    pub path: PathBuf,
    /// Every `ocx.lock` pinning this object; non-empty only for the registry-held
    /// entries a dry run reports.
    pub held_by: Vec<PathBuf>,
}

/// Results of [`PackageManager::clean`] (`adr_clean_project_backlinks.md`).
pub struct CleanResult {
    pub objects: Vec<CleanedObject>,
    pub temp: Vec<PathBuf>,
    /// Swept `state/projects/<key>/` directories, reported so a clean never
    /// revokes consent silently.
    pub consent: Vec<PathBuf>,
    /// Launcher and deferred-tree directories of other generations, retired (or, in a dry run, reported).
    pub launchers: Vec<PathBuf>,
}

/// The roots a locked tool pins: its per-platform leaves that are present on
/// this machine, so never-pulled platforms are not rooted.
async fn collect_tool_roots(
    repository: &ocx_oci::Repository,
    platforms: &std::collections::BTreeMap<String, ocx_oci::Digest>,
    file_structure: &FileStructure,
) -> Vec<ocx_oci::PinnedPackageRef> {
    let mut roots = Vec::new();
    for leaf in platforms.values() {
        let child_pinned = repository.pin_untagged(leaf.clone());
        if leaf_present_in_any_tier(file_structure, &child_pinned).await {
            roots.push(child_pinned);
        }
    }
    roots
}

/// Whether a lock-pinned leaf is present as a package, a deferred tree or an older ocx's shim.
// Checks shims too, or a deferred tool (no package dir) loses its pin and its shim is collected.
// Never an unconditional `true`, or never-pulled leaves appear in `--dry-run`'s held report.
async fn leaf_present_in_any_tier(file_structure: &FileStructure, leaf: &ocx_oci::PinnedPackageRef) -> bool {
    ocx_util::fs::path_exists_lossy(&file_structure.packages.path(leaf)).await
        || ocx_util::fs::path_exists_lossy(&file_structure.shims.path(leaf)).await
        || match current_deferred(file_structure) {
            Some(deferred) => ocx_util::fs::path_exists_lossy(&deferred.path(leaf)).await,
            None => false,
        }
}

/// GC roots from every live registered project's `ocx.lock` and from
/// `$OCX_HOME/ocx.lock`, with their patch bases for `platform`; one unreadable
/// lock yields [`CollectedRoots::RetainAll`].
pub(crate) async fn collect_project_roots(
    ocx_home: &Path,
    file_structure: &FileStructure,
    platform: &ocx_oci::Platform,
) -> crate::Result<CollectedRoots> {
    let registry = ProjectRegistry::new(ocx_home);

    // The obsolete JSON ledger is benign, so it is removed at debug, never WARN.
    let legacy_json = ocx_home.join("projects.json");
    let legacy_lock = ocx_home.join(".projects.lock");
    let legacy_present =
        ocx_util::fs::path_exists_lossy(&legacy_json).await || ocx_util::fs::path_exists_lossy(&legacy_lock).await;
    if legacy_present {
        log::debug!(
            "Removing obsolete legacy project ledger files ('{}', '{}').",
            legacy_json.display(),
            legacy_lock.display()
        );
        let _ = tokio::fs::remove_file(&legacy_json).await;
        let _ = tokio::fs::remove_file(&legacy_lock).await;
    }

    // Propagated, never degraded to an empty root set, which would collect every live project's packages.
    let project_dirs = registry.live_projects().await?;
    let mut entries: Vec<PathBuf> = project_dirs.into_iter().map(|dir| dir.join("ocx.lock")).collect();

    // Barred from the ledger (`adr_project_gc_symlink_ledger.md`), so added here or the
    // global toolchain's packages are collected (`adr_global_toolchain_tier.md`).
    entries.push(ocx_home.join("ocx.lock"));

    // JoinSet order is nondeterministic, so results are sorted below: the reachability graph keys on it.
    let mut load_set: JoinSet<LockLoad> = JoinSet::new();
    for lock_path in entries {
        let platform = platform.clone();
        load_set.spawn(async move {
            match ProjectLock::from_path(&lock_path).await {
                Ok(Some(lock)) => {
                    // Fail closed: untagged fallback bases miss tag-scoped rules and collect their companions.
                    let config = match super::patch_sync::read_lock_config(&lock_path).await {
                        Ok(config) => config,
                        Err(e) => {
                            log::warn!(
                                "Project root '{}': ocx.toml unreadable; retaining all objects this run \
                                 (fail-closed): {e}",
                                lock_path.display()
                            );
                            return LockLoad::Indeterminate;
                        }
                    };
                    let patch_bases = super::patch_sync::lock_bases(&lock, config.as_ref(), &platform);
                    LockLoad::Loaded(LoadedLock {
                        lock_path,
                        tools: lock.tools,
                        patch_bases,
                    })
                }
                Ok(None) => {
                    log::debug!(
                        "Skipping project root '{}': lock file no longer present.",
                        lock_path.display()
                    );
                    LockLoad::Absent
                }
                Err(e) => {
                    // Fail closed: dropping this root would collect the project's pinned packages.
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
                load_set.abort_all();
                while load_set.join_next().await.is_some() {}
                return Ok(CollectedRoots::RetainAll);
            }
        }
    }

    let sem = Arc::new(Semaphore::new(COLLECT_ROOTS_CONCURRENCY));
    let mut resolve_set: JoinSet<(usize, String, String, Vec<ocx_oci::PinnedPackageRef>)> = JoinSet::new();
    for (index, loaded_lock) in loaded.iter().enumerate() {
        for tool in &loaded_lock.tools {
            let sem = Arc::clone(&sem);
            let repository = tool.repository.clone();
            let platforms = tool.platforms.clone();
            let group = tool.group.clone();
            let name = tool.name.clone();
            // `index` is the dense position in `loaded`; the `entries` index also counts absent locks
            // and would overflow `buckets`.
            let fs = file_structure.clone();
            resolve_set.spawn(async move {
                let _permit = sem.acquire_owned().await.expect("semaphore closed");
                let resolved = collect_tool_roots(&repository, &platforms, &fs).await;
                (index, group, name, resolved)
            });
        }
    }

    let mut buckets: Vec<Vec<(String, String, Vec<ocx_oci::PinnedPackageRef>)>> =
        (0..loaded.len()).map(|_| Vec::new()).collect();
    while let Some(join) = resolve_set.join_next().await {
        let (index, group, name, resolved) = join.expect("collect_project_roots resolve task panicked");
        buckets[index].push((group, name, resolved));
    }

    let patch_bases = loaded
        .iter()
        .flat_map(|loaded_lock| loaded_lock.patch_bases.iter().cloned())
        .collect();
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

    roots.sort_by(|a, b| a.ocx_lock_path.cmp(&b.ocx_lock_path));
    Ok(CollectedRoots::Roots(ProjectRoots {
        digests: roots,
        patch_bases,
    }))
}

/// A registered project's parsed `ocx.lock`.
struct LoadedLock {
    lock_path: PathBuf,
    tools: Vec<ocx_project::lock::LockedTool>,
    patch_bases: Vec<ocx_oci::PackageRef>,
}

/// Outcome of loading a single registered project's `ocx.lock`.
enum LockLoad {
    Loaded(LoadedLock),
    /// The lock is gone, a departed project: its root is dropped.
    Absent,
    /// A non-`NotFound` read error: the GC retains every object this run.
    Indeterminate,
}

/// Every readable project lock's GC roots and patch bases.
pub(crate) struct ProjectRoots {
    /// Per-project roots, deterministically ordered.
    pub(crate) digests: Vec<ProjectRootDigests>,
    /// Lock entries as `ocx lock` discovered their patch companions, for
    /// [`PackageManager::resolve_site_patch_roots`].
    pub(crate) patch_bases: Vec<ocx_oci::PackageRef>,
}

/// Result of [`collect_project_roots`].
pub(crate) enum CollectedRoots {
    Roots(ProjectRoots),
    /// A live project's lock was unreadable: the caller must retain every object,
    /// or a partial root set collects that project's packages.
    RetainAll,
}

impl PackageManager {
    /// Collects unreachable objects, stale temps and departed-project consent
    /// stamps; `force` ignores the project registry, so only install symlinks
    /// root packages (`adr_project_gc_symlink_ledger.md`).
    pub async fn clean(&self, dry_run: bool, force: bool) -> crate::Result<CleanResult> {
        let ocx_home = self.file_structure().root().to_path_buf();
        let host_platform = ocx_oci::Platform::current().unwrap_or_else(ocx_oci::Platform::any);

        // `--force` reads no project lock, so their patch companions lose their roots too.
        let project_roots = if force {
            ProjectRoots {
                digests: Vec::new(),
                patch_bases: Vec::new(),
            }
        } else {
            match collect_project_roots(&ocx_home, self.file_structure(), &host_platform).await? {
                CollectedRoots::Roots(roots) => roots,
                CollectedRoots::RetainAll => {
                    // Consent stamps are retained too: over-retention is the safe direction.
                    let temp = clean_temp(self.file_structure(), dry_run).await?;
                    return Ok(CleanResult {
                        objects: Vec::new(),
                        temp,
                        consent: Vec::new(),
                        launchers: Vec::new(),
                    });
                }
            }
        };

        // Snapshot pins too: compose resolves snapshot-first, so a record-only root set
        // collects what a frozen build still reads.
        let patch_roots = self
            .resolve_site_patch_roots(
                &host_platform,
                super::resolve::PatchRootScope::RecordedAndSnapshot,
                &project_roots.patch_bases,
            )
            .await?;
        let garbage_collector =
            GarbageCollector::build(self.file_structure(), &project_roots.digests, &patch_roots).await?;

        let targets = garbage_collector.unreachable_objects();
        let attribution = garbage_collector.roots_attribution();

        log::debug!(
            "Scanning for unreferenced entries{}: {} candidate(s).",
            if dry_run { " (dry run)" } else { "" },
            targets.len(),
        );

        let raw_objects = garbage_collector.delete_objects(&targets, dry_run).await?;
        let mut objects: Vec<CleanedObject> = raw_objects
            .into_iter()
            .map(|path| CleanedObject {
                path,
                held_by: Vec::new(),
            })
            .collect();

        // A dry run also reports registry-held objects, showing what `--force` would collect.
        if dry_run {
            for (held_path, held_by) in attribution {
                objects.push(CleanedObject {
                    path: held_path.clone(),
                    held_by: held_by.clone(),
                });
            }
        }

        let temp = clean_temp(self.file_structure(), dry_run).await?;
        // Runs under `--force` too: `--force` waives the project registry, which this sweep never reads.
        let consent = sweep_consent_stamps(&self.file_structure().state, dry_run).await?;
        let mut launchers = retire_other_generations(self.file_structure(), dry_run).await?;
        launchers.extend(retire_superseded_shells(self.file_structure(), dry_run).await?);
        Ok(CleanResult {
            objects,
            temp,
            consent,
            launchers,
        })
    }
}

/// Retires every `launchers/<G'>` of every package and every `deferred/<G'>` whose name is not this ocx's
/// generation, returning the paths removed (a dry run: the paths that would be). A host with no embedded blob
/// knows no current generation and retires nothing. `entrypoints/`, `shims/` and `.bin/ocx-shim` are never read.
// The helper step 2 reuses for `shells/default-*`: one `retire_dir` call per candidate, `LeftForLater` logged and not reported.
async fn retire_other_generations(fs: &FileStructure, dry_run: bool) -> crate::Result<Vec<PathBuf>> {
    let Some(generation) = ocx_store::launcher_generation::LauncherGeneration::host() else {
        return Ok(Vec::new());
    };
    let mut parents = vec![fs.deferred_root()];
    for package in fs.packages.list_all().await? {
        parents.push(package.launchers_root());
    }
    let mut retired = Vec::new();
    for parent in parents {
        let Ok(mut entries) = tokio::fs::read_dir(&parent).await else {
            continue;
        };
        let mut candidates = Vec::new();
        while let Ok(Some(entry)) = entries.next_entry().await {
            if entry.file_name() == generation.as_str() {
                continue;
            }
            // Only real directories are generation trees; `retire_dir` itself sweeps `.gc-*`/`.staging-*` debris.
            if entry.file_type().await.is_ok_and(|kind| kind.is_dir()) {
                candidates.push(entry.path());
            }
        }
        candidates.sort();
        for dir in candidates {
            retire_one(&dir, dry_run, &mut retired).await?;
        }
    }
    Ok(retired)
}

/// Retires, for every home a v2 stamp names, each `default-<16 hex>`, `.staging-*` and `.gc-*` under
/// `shells/` that is neither the stamp's `shell_dir` nor its `previous_shell_dir`
/// nor the name `active` points at. `default` and every other name stay. A home that cannot be locked or fails the symlink guard is skipped with a warning.
async fn retire_superseded_shells(fs: &FileStructure, dry_run: bool) -> crate::Result<Vec<PathBuf>> {
    let state = fs.state.clone();
    let homes = tokio::task::spawn_blocking(move || stamped_homes(&state))
        .await
        .map_err(|e| crate::Error::InternalFile(PathBuf::new(), std::io::Error::other(e)))?;
    let mut retired = Vec::new();
    for stamp in homes {
        let home = ToolchainHome::new(&stamp.home);
        // The lock is keyed on the home root's identity, so a home that was never rendered has none.
        if tokio::fs::symlink_metadata(home.root()).await.is_err() {
            continue;
        }
        let parameters = clean_lock_parameters(fs, home.root(), &stamp.scope);
        // Held for the whole home: `retire_dir` sweeps sibling `.staging-*`, which a live render owns.
        let _lock = match parameters.acquire().await {
            Ok(lock) => lock,
            Err(e) => {
                log::warn!(
                    "Toolchain home '{}' is locked; its render directories stay: {e}",
                    stamp.home.display()
                );
                continue;
            }
        };
        if let Err(e) = refuse_symlinked_home(&stamp.scope, &home) {
            log::warn!("Toolchain home '{}' is not cleaned: {e}", stamp.home.display());
            continue;
        }
        let Some(shells) = home.render_dir(DEFAULT_SHELL).parent().map(Path::to_path_buf) else {
            continue;
        };
        // The stamp is written after the `active` switch, so it can lag: retiring what `active` names would dangle it.
        let active = {
            let home = ToolchainHome::new(&stamp.home);
            tokio::task::spawn_blocking(move || home.active_target_name())
                .await
                .ok()
                .flatten()
        };
        let Ok(mut entries) = tokio::fs::read_dir(&shells).await else {
            continue;
        };
        let mut candidates = Vec::new();
        while let Ok(Some(entry)) = entries.next_entry().await {
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if !is_render_debris(&name)
                || name == stamp.shell_dir
                || name == stamp.previous_shell_dir
                || active.as_deref() == Some(name.as_str())
            {
                continue;
            }
            if entry.file_type().await.is_ok_and(|kind| kind.is_dir()) {
                candidates.push(entry.path());
            }
        }
        candidates.sort();
        for dir in candidates {
            retire_one(&dir, dry_run, &mut retired).await?;
        }
    }
    Ok(retired)
}

/// A render directory a clean may retire: a published `default-<16 hex>` or staging debris.
fn is_render_debris(name: &str) -> bool {
    name.strip_prefix("default-")
        .is_some_and(|generation| LauncherGeneration::from_hex(generation).is_some())
        || name.starts_with(".staging-")
        || name.starts_with(".gc-")
}

/// The v2 stamps of the global home and of every `state/projects/<key>/` whose scope agrees with its
/// key, in key order; blocking.
fn stamped_homes(state: &StateStore) -> Vec<RenderStampV2> {
    let mut found = Vec::new();
    if let Some(stamp) = state.render_stamp_v2(RenderStampTarget::Global) {
        if stamp.scope == RenderStampScope::Global {
            found.push(("global".to_string(), stamp));
        } else {
            log::debug!("Global render stamp names a project scope; skipped.");
        }
    }
    let mut keys = Vec::new();
    if let Ok(entries) = std::fs::read_dir(state.project_state_root()) {
        for entry in entries.flatten() {
            let Ok(key) = entry.file_name().into_string() else {
                continue;
            };
            if key.starts_with(STAGING_PREFIX) {
                continue;
            }
            // `symlink_metadata`: a symlinked key directory is never followed.
            if std::fs::symlink_metadata(entry.path()).is_ok_and(|metadata| metadata.is_dir()) {
                keys.push(key);
            }
        }
    }
    for key in keys {
        let Some(stamp) = state.render_stamp_v2(RenderStampTarget::Project(&key)) else {
            continue;
        };
        match &stamp.scope {
            RenderStampScope::Project(directory) if ReferenceManager::name_for_path(directory) == key => {
                found.push((key, stamp));
            }
            _ => log::debug!("Render stamp under key '{key}' names another scope; skipped."),
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found.into_iter().map(|(_, stamp)| stamp).collect()
}

/// The render lock of the home at `root`, spelled as `render_toolchain` spells it.
fn clean_lock_parameters(fs: &FileStructure, root: &Path, scope: &RenderStampScope) -> ScopedLockParameters {
    ScopedLockParameters {
        locks_root: fs.locks.clone(),
        scope: "toolchain-render",
        guarded_directory: root.to_path_buf(),
        discriminator: match scope {
            RenderStampScope::Global => "global".to_string(),
            RenderStampScope::Project(directory) => ReferenceManager::name_for_path(directory),
        },
        timeout: super::pull::PULL_LOCAL_LOCK_TIMEOUT,
    }
}

/// Retires `dir`; a refusal leaves it whole and unreported.
async fn retire_one(dir: &Path, dry_run: bool, retired: &mut Vec<PathBuf>) -> crate::Result<()> {
    if dry_run {
        log::info!("Would retire launcher directory: {}", dir.display());
        retired.push(dir.to_path_buf());
        return Ok(());
    }
    match ocx_util::fs::retire_dir(dir).await {
        Ok(ocx_util::fs::Retired::Removed) => {
            log::info!("Retired launcher directory: {}", dir.display());
            retired.push(dir.to_path_buf());
        }
        Ok(ocx_util::fs::Retired::LeftForLater) => {
            log::warn!(
                "Launcher directory '{}' is busy; left for a later clean.",
                dir.display()
            );
        }
        Err(e) => return Err(crate::Error::InternalFile(dir.to_path_buf(), e)),
    }
    Ok(())
}

/// Removes stale temp directories and orphan lock files, locking each where
/// possible so a concurrent install's directory is not swept.
async fn clean_temp(fs: &ocx_store::file_structure::FileStructure, dry_run: bool) -> crate::Result<Vec<PathBuf>> {
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
                // Released only after removal, so no install can claim the dir mid-delete.
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

/// Stamp schema version this binary understands; an unknown version is retained, never collected.
const UNDERSTOOD_STAMP_VERSION: u8 = 1;

/// Outcome of probing a stamp's recorded `project_dir`.
// Absent and Indeterminate stay apart: a transient error read as "gone" deletes consent and the project goes inert.
enum DirProbe {
    Present,
    /// An `Ok` probe proved nothing exists at the recorded path.
    Absent,
    /// A non-`NotFound` I/O error; the stamp is retained.
    Indeterminate,
}

/// Why a `state/projects/<key>/` directory is a sweep candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SweepReason {
    /// The recorded `project_dir` was absent; removal re-probes, since it may have been recreated.
    Departed,
    /// The stamp records `$OCX_HOME`, never a consent subject; a re-probe would retain it
    /// forever, since the ocx root always exists.
    OcxHome,
}

// `symlink_metadata`, not `metadata`: a dangling symlink where the project was must count as present.
async fn probe_project_dir(project_dir: &Path) -> DirProbe {
    match tokio::fs::symlink_metadata(project_dir).await {
        Ok(_) => DirProbe::Present,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => DirProbe::Absent,
        Err(_) => DirProbe::Indeterminate,
    }
}

/// Removes every `state/projects/<key>/` whose stamp records an absent
/// `project_dir` or `$OCX_HOME`, returned sorted; every ambiguous entry is retained.
///
/// # Errors
///
/// A removal I/O failure; an unenumerable sweep root sweeps nothing instead.
async fn sweep_consent_stamps(
    state: &ocx_store::file_structure::StateStore,
    dry_run: bool,
) -> crate::Result<Vec<PathBuf>> {
    let sweep_root = state.project_state_root();
    let mut read_dir = match tokio::fs::read_dir(&sweep_root).await {
        Ok(entries) => entries,
        // An absent root is ordinary, so debug, never a warning.
        Err(e) => {
            log::debug!(
                "Consent-stamp sweep: '{}' not enumerable, sweeping nothing this run: {e}",
                sweep_root.display()
            );
            return Ok(Vec::new());
        }
    };

    // Never derive keys from `live_projects()`: an `[env]`-only project is not in it, and would lose consent forever.
    // Must match `consent::record_in`'s derivation, or `$OCX_HOME` stamps are never recognised.
    let ocx_home = state.root().parent().unwrap_or_else(|| state.root()).to_path_buf();
    let mut candidates: Vec<(PathBuf, PathBuf, SweepReason)> = Vec::new();
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

        // Skipped, never followed: `remove_dir_all` through a link deletes its target.
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
        // Before the liveness probe, which would retain it as `Present`; sync is fine in a one-shot command.
        if ocx_util::fs::same_dir(&project_dir, &ocx_home).unwrap_or(false) {
            candidates.push((state_dir, project_dir, SweepReason::OcxHome));
            continue;
        }

        match probe_project_dir(&project_dir).await {
            DirProbe::Absent => candidates.push((state_dir, project_dir, SweepReason::Departed)),
            DirProbe::Present => {}
            DirProbe::Indeterminate => log::debug!(
                "Consent-stamp sweep: retaining '{}' — liveness of '{}' is indeterminate.",
                state_dir.display(),
                project_dir.display()
            ),
        }
    }

    candidates.sort();

    remove_departed_stamps(candidates, dry_run).await
}

/// Removes the candidates in order, re-probing each `Departed` one just before
/// removal, since deleting a recreated project's consent makes it go inert.
///
/// # Errors
///
/// Propagates a removal I/O failure, matching [`remove_stale_dir`].
async fn remove_departed_stamps(
    candidates: Vec<(PathBuf, PathBuf, SweepReason)>,
    dry_run: bool,
) -> crate::Result<Vec<PathBuf>> {
    let mut swept = Vec::new();
    for (state_dir, project_dir, reason) in candidates {
        if reason == SweepReason::Departed {
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
        }

        log::info!(
            "{} consent stamp for {} '{}': {}",
            if dry_run { "Would remove" } else { "Removing" },
            match reason {
                SweepReason::Departed => "departed project",
                SweepReason::OcxHome => "the ocx home, which is never a consent subject",
            },
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

/// The `project_dir` a stamp records, or `None` for an unusable stamp, which the
/// sweep retains: it may be consent a newer or rolled-back binary wrote.
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
    let stamp: ocx_project::consent::ConsentStamp = match serde_json::from_slice(&bytes) {
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
    use ocx_store::file_structure::FileStructure;

    // Minimal valid V3 ocx.lock that `ProjectLock::from_path` can parse.
    //
    // The `declaration_hash` value is not validated on load — only
    // `declaration_hash_version` is checked. `repository` is the bare
    // registry/repo coordinate; each `[tool.platforms]` entry is a leaf
    // digest keyed by the canonical grammar `Platform` string (D2).
    //
    // Registry must contain `.` or `:` or be "localhost" to be parsed as an
    // explicit registry (see `ocx_oci::package_ref::has_explicit_registry`).
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

    /// Build the `PinnedPackageRef` a `repository`/leaf-digest pair in the
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
    ) -> ocx_oci::PinnedPackageRef {
        let pinned = pinned_leaf(repository, registry, digest_hex);
        tokio::fs::create_dir_all(file_structure.packages.path(&pinned))
            .await
            .unwrap();
        pinned
    }

    /// The `PinnedPackageRef` a `repository`/leaf-digest pair in the fixtures
    /// above resolves to, with nothing seeded on disk.
    fn pinned_leaf(repository: &str, registry: &str, digest_hex: &str) -> ocx_oci::PinnedPackageRef {
        ocx_oci::PinnedPackageRef::try_from(
            ocx_oci::PackageRef::new_registry(repository, registry)
                .clone_with_digest(ocx_oci::Digest::Sha256(digest_hex.to_string())),
        )
        .unwrap()
    }

    /// The leaf digest both lock fixtures pin for `cmake`.
    const CMAKE_LEAF_HEX: &str = "aaaa0000000000000000000000000000000000000000000000000000000000bb";

    /// Pre-create the shim directory for `pinned` — its `bin/` child is the
    /// completeness marker, and its presence is the whole on-disk
    /// evidence a deferred tool leaves behind.
    async fn seed_pinned_shim_dir(file_structure: &FileStructure, pinned: &ocx_oci::PinnedPackageRef) {
        tokio::fs::create_dir_all(file_structure.shims.shim_dir(pinned).bin())
            .await
            .unwrap();
    }

    // ── the presence gate accepts either tier ──────────────────────

    /// A deferred tool has no package directory by
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

        let roots = match collect_project_roots(&ocx_home, &file_structure, &ocx_oci::Platform::any())
            .await
            .unwrap()
        {
            CollectedRoots::Roots(roots) => roots.digests,
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
        let roots = match collect_project_roots(&ocx_home, &file_structure, &ocx_oci::Platform::any())
            .await
            .unwrap()
        {
            CollectedRoots::Roots(roots) => roots.digests,
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
        let result = collect_project_roots(&ocx_home, &file_structure, &ocx_oci::Platform::any())
            .await
            .unwrap();

        let roots = match result {
            CollectedRoots::Roots(roots) => roots.digests,
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

    /// A lock whose `ocx.toml` cannot be read or parsed fails closed: the untagged fallback bases
    /// miss tag-scoped rules, so a partial root set would collect their companions. A missing
    /// `ocx.toml` is no error.
    #[tokio::test]
    async fn collect_roots_retains_all_when_the_lock_config_is_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let ocx_home = dir.path().to_path_buf();
        tokio::fs::write(ocx_home.join("ocx.lock"), LOCK_WITH_ONE_TOOL)
            .await
            .unwrap();
        let file_structure = FileStructure::with_root(ocx_home.clone());
        let platform = ocx_oci::Platform::any();
        let collect = || collect_project_roots(&ocx_home, &file_structure, &platform);

        assert!(
            matches!(collect().await.unwrap(), CollectedRoots::Roots(_)),
            "a missing ocx.toml reads as a stale lock, not an error"
        );

        tokio::fs::write(ocx_home.join("ocx.toml"), "[tools\n").await.unwrap();
        assert!(
            matches!(collect().await.unwrap(), CollectedRoots::RetainAll),
            "a malformed ocx.toml must retain everything"
        );

        tokio::fs::remove_file(ocx_home.join("ocx.toml")).await.unwrap();
        tokio::fs::create_dir(ocx_home.join("ocx.toml")).await.unwrap();
        assert!(
            matches!(collect().await.unwrap(), CollectedRoots::RetainAll),
            "an unreadable ocx.toml must retain everything"
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
        let result = collect_project_roots(&ocx_home, &file_structure, &ocx_oci::Platform::any())
            .await
            .unwrap();

        let roots = match result {
            CollectedRoots::Roots(roots) => roots.digests,
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
        let result = collect_project_roots(&ocx_home, &file_structure, &ocx_oci::Platform::any())
            .await
            .unwrap();

        let roots = match result {
            CollectedRoots::Roots(roots) => roots.digests,
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

    // ── consent-stamp sweep ─────────────────────────────

    /// Writes a stamp at `state/projects/<key>/consent.json` whose recorded
    /// `project_dir` is `project_dir`, and returns the state directory.
    fn write_stamp(
        state: &ocx_store::file_structure::StateStore,
        key: &str,
        version: u8,
        project_dir: &Path,
    ) -> PathBuf {
        let dir = state.project_state_dir(key);
        std::fs::create_dir_all(&dir).unwrap();
        let stamp = format!(
            r#"{{"v":{version},"project_dir":{dir_json},"sources":["ocx.sh/ocx"],"stamped_at":"2026-01-01T00:00:00Z"}}"#,
            dir_json = serde_json::to_string(project_dir).unwrap()
        );
        std::fs::write(state.consent_stamp_file(key), stamp).unwrap();
        dir
    }

    /// A stamp whose `project_dir` is gone is collected, and
    /// the reported paths are sorted (DATA-DET: filesystem readdir order never
    /// reaches the caller).
    /// EC-IDENT-006 — a stamp whose recorded project_dir no longer exists is collected.
    #[tokio::test]
    async fn consent_sweep_collects_stamps_whose_project_dir_is_gone() {
        let tmp = tempfile::tempdir().unwrap();
        let state = ocx_store::file_structure::StateStore::new(tmp.path().join("state"));
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

    /// The `[env]`-only project: a live `project_dir` with no
    /// `ocx.lock` and therefore no ledger entry. The sweep reads the stamp's own
    /// `project_dir`, never the ledger, so this stamp survives.
    /// EC-IDENT-007 — an `[env]`-only project has no ocx.lock and can never be listed by live_projects; its stamp is retained anyway.
    #[tokio::test]
    async fn consent_sweep_retains_a_stamp_whose_project_dir_is_live_without_a_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let state = ocx_store::file_structure::StateStore::new(tmp.path().join("state"));
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

    /// A stamp recording `$OCX_HOME` is collected even though the ocx
    /// root is live, and a real project's stamp in the same run is not.
    ///
    /// Pre-guard binaries wrote this stamp on any `--global` invocation of the
    /// consent writers (`ocx --global lock` first; `run` rewrote the same
    /// key), so it exists on installations that ran before
    /// `consent::record_in` learned to refuse it. Liveness cannot collect it:
    /// the ocx root exists by definition, so the ordinary `DirProbe::Absent`
    /// path retains it forever — measured, before this arm.
    ///
    /// Both halves are load-bearing. Without the surviving project, a sweep
    /// that deleted every stamp would pass; without the ocx-root stamp, the
    /// arm under test never runs. Red state: drop the `SweepReason::OcxHome`
    /// classification and the first assertion fails with the stamp still
    /// present; leave the classification but re-probe `OcxHome` candidates in
    /// `remove_departed_stamps` and it fails the same way, because the ocx root
    /// probes `Present`.
    #[tokio::test]
    async fn a44_consent_sweep_collects_a_stamp_recording_the_ocx_home() {
        let tmp = tempfile::tempdir().unwrap();
        // `StateStore`'s root is `$OCX_HOME/state`, so the ocx root is `tmp`.
        let state = ocx_store::file_structure::StateStore::new(tmp.path().join("state"));

        let stale = write_stamp(&state, "aaaa000000000000", 1, tmp.path());

        let live = tmp.path().join("real-project");
        std::fs::create_dir_all(&live).unwrap();
        let kept = write_stamp(&state, "bbbb000000000000", 1, &live);

        let swept = sweep_consent_stamps(&state, false).await.unwrap();

        assert_eq!(
            swept,
            vec![stale.clone()],
            "exactly the ocx-home stamp is swept; got {swept:?}"
        );
        assert!(
            !stale.exists(),
            "a stamp recording the ocx home must be collected even though the ocx root is live"
        );
        assert!(
            kept.exists(),
            "and a live project's stamp in the same run must survive — otherwise the sweep is deleting everything"
        );
    }

    /// **The assigned fault-injection guard.** `--dry-run` reports what
    /// it would remove and removes nothing.
    ///
    /// `assert!(dir.exists())` is the assertion a `dry_run`-ignoring sweep flips.
    #[tokio::test]
    async fn consent_sweep_dry_run_reports_without_removing() {
        let tmp = tempfile::tempdir().unwrap();
        let state = ocx_store::file_structure::StateStore::new(tmp.path().join("state"));
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

    /// A stamp that does not deserialize is RETAINED. "I cannot read it"
    /// is not "it is garbage"; the recorded `project_dir` is gone here, so only
    /// the parse precondition can save it.
    #[tokio::test]
    async fn consent_sweep_retains_a_malformed_stamp() {
        let tmp = tempfile::tempdir().unwrap();
        let state = ocx_store::file_structure::StateStore::new(tmp.path().join("state"));
        let dir = state.project_state_dir("aaaa000000000000");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(state.consent_stamp_file("aaaa000000000000"), b"{not json").unwrap();

        let swept = sweep_consent_stamps(&state, false).await.unwrap();

        assert!(swept.is_empty(), "a malformed stamp must be retained; got {swept:?}");
        assert!(dir.exists(), "a malformed stamp must be retained");
    }

    /// A stamp at a `v` this binary does not understand is RETAINED even
    /// though its `project_dir` is definitively gone. Under-retention would
    /// delete consent a newer or rolled-back binary wrote.
    #[tokio::test]
    async fn consent_sweep_retains_an_unknown_version_stamp() {
        let tmp = tempfile::tempdir().unwrap();
        let state = ocx_store::file_structure::StateStore::new(tmp.path().join("state"));
        let gone = tmp.path().join("departed");
        let dir = write_stamp(&state, "aaaa000000000000", 2, &gone);

        let swept = sweep_consent_stamps(&state, false).await.unwrap();

        assert!(swept.is_empty(), "an unknown-`v` stamp must be retained; got {swept:?}");
        assert!(dir.exists(), "an unknown-`v` stamp must be retained");
    }

    /// A stamp whose bytes cannot be read is RETAINED (transient
    /// permission or I/O fault). Skipped when the process can read a `0o000`
    /// file anyway — an observed cause, not an assumed one.
    #[cfg(unix)]
    /// EC-IDENT-011 — an unreadable stamp is retained, not collected: a transient permission flip must not revoke consent.
    #[tokio::test]
    async fn consent_sweep_retains_an_unreadable_stamp() {
        use std::os::unix::fs::PermissionsExt as _;

        let tmp = tempfile::tempdir().unwrap();
        let state = ocx_store::file_structure::StateStore::new(tmp.path().join("state"));
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

    /// A symlinked state directory is skipped, never followed
    /// into `remove_dir_all`.
    #[cfg(unix)]
    /// EC-IDENT-008 — a symlinked state directory is skipped, never followed.
    #[tokio::test]
    async fn consent_sweep_skips_a_symlinked_state_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let state = ocx_store::file_structure::StateStore::new(tmp.path().join("state"));
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

    /// `.tmp-*` staging names are skipped.
    #[tokio::test]
    async fn consent_sweep_skips_tmp_staging_names() {
        let tmp = tempfile::tempdir().unwrap();
        let state = ocx_store::file_structure::StateStore::new(tmp.path().join("state"));
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
    /// consent revoked — and a project whose consent is deleted goes inert,
    /// silently, with no error anywhere.
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
        let state = ocx_store::file_structure::StateStore::new(tmp.path().join("state"));
        let recreated = tmp.path().join("recreated");
        let stamp_dir = write_stamp(&state, "aaaa000000000000", 1, &recreated);

        // What the classification loop sees: definitively absent, so this stamp
        // becomes a removal candidate.
        assert!(
            matches!(probe_project_dir(&recreated).await, DirProbe::Absent),
            "the fixture must start as a genuine removal candidate"
        );
        let candidates = vec![(stamp_dir.clone(), recreated.clone(), SweepReason::Departed)];

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
        let state = ocx_store::file_structure::StateStore::new(tmp.path().join("state"));
        let gone = tmp.path().join("departed");
        let stamp_dir = write_stamp(&state, "aaaa000000000000", 1, &gone);

        let swept = remove_departed_stamps(vec![(stamp_dir.clone(), gone, SweepReason::Departed)], false)
            .await
            .unwrap();

        assert_eq!(swept, vec![stamp_dir.clone()], "a still-departed candidate is swept");
        assert!(!stamp_dir.exists(), "a still-departed candidate's stamp is removed");
    }

    /// An indeterminate probe of `project_dir` retains the
    /// stamp. The fixture stages `ENAMETOOLONG` (a 300-byte component), which
    /// is an `Err` that is not `NotFound` for every user including root.
    #[cfg(unix)]
    /// EC-IDENT-009 — an indeterminate liveness probe retains; only a determinate miss collects.
    #[tokio::test]
    async fn consent_sweep_retains_on_an_indeterminate_probe() {
        let tmp = tempfile::tempdir().unwrap();
        let state = ocx_store::file_structure::StateStore::new(tmp.path().join("state"));
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

    /// At the **re-probe**, an indeterminate second probe
    /// retains, exactly as the classification probe does.
    ///
    /// [`consent_sweep_retains_on_an_indeterminate_probe`] covers the *first*
    /// probe only. It reds when [`probe_project_dir`] collapses `Err` into
    /// `Absent`, but it stays green when [`remove_departed_stamps`] splits its
    /// `Present | Indeterminate` arm and lets the indeterminate half fall
    /// through to removal — and that half is the worst-case hazard: a
    /// project that is still there, momentarily unstattable (a
    /// permission flip, an unreachable mount), loses the consent it already
    /// gave and goes inert.
    ///
    /// Passing the candidate list in is what makes the second probe reachable
    /// at all — the same reason [`remove_departed_stamps`] takes it as a
    /// parameter rather than computing it.
    #[cfg(unix)]
    #[tokio::test]
    async fn consent_sweep_retains_a_candidate_indeterminate_at_the_re_probe() {
        let tmp = tempfile::tempdir().unwrap();
        let state = ocx_store::file_structure::StateStore::new(tmp.path().join("state"));
        let indeterminate = tmp.path().join("x".repeat(300));
        let probe = std::fs::symlink_metadata(&indeterminate).unwrap_err();
        assert_ne!(
            probe.kind(),
            std::io::ErrorKind::NotFound,
            "fixture must stage an indeterminate probe, not an absent one; got {probe:?}"
        );
        let stamp_dir = write_stamp(&state, "aaaa000000000000", 1, &indeterminate);

        let swept = remove_departed_stamps(vec![(stamp_dir.clone(), indeterminate, SweepReason::Departed)], false)
            .await
            .unwrap();

        assert!(swept.is_empty(), "an indeterminate re-probe must retain; got {swept:?}");
        assert!(
            stamp_dir.exists(),
            "an indeterminate re-probe must not remove the stamp"
        );
    }

    /// No `state/projects/` yet is the ordinary state of a fresh home:
    /// an empty sweep, no error, no warning.
    #[tokio::test]
    async fn consent_sweep_is_empty_when_the_sweep_root_is_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let state = ocx_store::file_structure::StateStore::new(tmp.path().join("state"));

        let swept = sweep_consent_stamps(&state, false).await.unwrap();

        assert!(swept.is_empty(), "an absent sweep root yields nothing; got {swept:?}");
    }

    // ── launcher generations (C-137) ─────────────────────────────────────

    use ocx_store::launcher_generation::LauncherGeneration;

    /// A 16-hex generation name that is not `current`.
    fn other_generation(current: &LauncherGeneration) -> String {
        ["0123456789abcdef", "fedcba9876543210"]
            .into_iter()
            .find(|name| *name != current.as_str())
            .unwrap()
            .to_string()
    }

    async fn touch_dir(dir: &Path) -> PathBuf {
        tokio::fs::create_dir_all(dir).await.unwrap();
        tokio::fs::write(dir.join("f"), b"x").await.unwrap();
        dunce::canonicalize(dir).unwrap()
    }

    /// A home whose global lock pins `cmake` (materialized: `content/`) and `shfmt` (deferred under `G`), and
    /// the paths the launcher tests inspect.
    struct LauncherHome {
        _tmp: tempfile::TempDir,
        manager: crate::PackageManager,
        generation: LauncherGeneration,
        package_current: PathBuf,
        package_other: PathBuf,
        deferred_current: PathBuf,
        deferred_other: PathBuf,
    }

    async fn launcher_home() -> Option<LauncherHome> {
        // No embedded blob on this host: no current generation, nothing to retire.
        let generation = LauncherGeneration::host()?;
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();
        tokio::fs::write(root.join("ocx.lock"), LOCK_WITH_TWO_TOOLS)
            .await
            .unwrap();
        let fs = FileStructure::with_root(root.clone());
        let cmake = pinned_leaf("cmake", "localhost:5000", CMAKE_LEAF_HEX);
        let shfmt = pinned_leaf(
            "shfmt",
            "localhost:5000",
            "bbbb0000000000000000000000000000000000000000000000000000000000cc",
        );
        tokio::fs::create_dir_all(fs.packages.path(&cmake).join("content"))
            .await
            .unwrap();
        let other = other_generation(&generation);
        let package_current = touch_dir(&fs.packages.launchers(&cmake, &generation)).await;
        let package_other = touch_dir(&fs.packages.launchers_root(&cmake).join(&other)).await;
        let deferred_current = touch_dir(&fs.deferred(&generation).shim_dir(&shfmt).bin()).await;
        let deferred_other = touch_dir(&fs.deferred_root().join(&other).join("some/tree/l")).await;
        let manager = crate::test_support::export_fixture::offline_manager(&root);
        Some(LauncherHome {
            _tmp: tmp,
            manager,
            generation,
            package_current,
            package_other,
            deferred_current,
            deferred_other,
        })
    }

    /// V-L41: the current generation comes from `LauncherGeneration::host()`; a constant would retire the wrong dir.
    #[tokio::test]
    async fn clean_retires_launcher_generations_other_than_the_hosts() {
        let Some(home) = launcher_home().await else { return };
        let result = home.manager.clean(false, false).await.unwrap();

        let mut reported = result.launchers.clone();
        reported.sort();
        let mut expected = vec![home.package_other.clone(), home.deferred_other.clone()];
        expected.sort();
        assert_eq!(reported.len(), 2, "got {reported:?}");
        assert!(
            reported.iter().zip(&expected).all(|(r, e)| e.starts_with(r)),
            "{reported:?} vs {expected:?}"
        );
        assert!(!home.package_other.exists());
        assert!(!home.deferred_other.exists());
        assert!(home.package_current.exists(), "launchers/<G> is kept");
        assert!(
            home.deferred_current.exists(),
            "a lock-pinned deferred/<G> tree is kept"
        );
        assert!(
            !result
                .objects
                .iter()
                .any(|o| o.path.starts_with(&home.deferred_current))
        );
        let _ = &home.generation;
    }

    #[tokio::test]
    async fn clean_dry_run_reports_other_launcher_generations_without_removing() {
        let Some(home) = launcher_home().await else { return };
        let result = home.manager.clean(true, false).await.unwrap();

        assert_eq!(result.launchers.len(), 2, "got {:?}", result.launchers);
        assert!(home.package_other.exists());
        assert!(home.deferred_other.exists());
    }

    /// An unrooted `deferred/<G>/…` tree is an object, collected like an unpinned shim.
    #[tokio::test]
    async fn clean_collects_an_unrooted_deferred_tree_of_the_current_generation() {
        let Some(home) = launcher_home().await else { return };
        tokio::fs::remove_file(home.manager.file_structure().root().join("ocx.lock"))
            .await
            .unwrap();
        let result = home.manager.clean(false, false).await.unwrap();

        assert!(!home.deferred_current.exists());
        assert!(
            result
                .objects
                .iter()
                .any(|o| home.deferred_current.starts_with(&o.path))
        );
    }

    /// C-132 half: `entrypoints/`, old `shims/…` of a pinned leaf and `.bin/ocx-shim/*` are never touched.
    #[tokio::test]
    async fn clean_leaves_entrypoints_old_shims_and_the_shim_binary_alone() {
        let Some(home) = launcher_home().await else { return };
        let fs = home.manager.file_structure().clone();
        let cmake = pinned_leaf("cmake", "localhost:5000", CMAKE_LEAF_HEX);
        let entrypoints = touch_dir(&fs.packages.entrypoints(&cmake)).await;
        let shim = touch_dir(&fs.shims.shim_dir(&cmake).bin()).await;
        let shim_binary = touch_dir(&fs.root().join(".bin").join("ocx-shim")).await;

        home.manager.clean(false, false).await.unwrap();

        for kept in [entrypoints, shim, shim_binary] {
            assert!(kept.join("f").exists(), "{} was touched", kept.display());
        }
    }

    /// V-L26 clean half: a held `temp/deferred-…` scratch survives a clean.
    #[tokio::test]
    async fn clean_keeps_a_held_deferred_scratch() {
        let Some(home) = launcher_home().await else { return };
        let held = home.manager.file_structure().temp.scratch("deferred").await.unwrap();
        let path = held.path().to_path_buf();

        home.manager.clean(false, false).await.unwrap();

        assert!(path.exists());
    }

    // ── superseded toolchain render directories ─────────────────────

    const GEN_A: &str = "aaaaaaaaaaaaaaaa";
    const GEN_B: &str = "bbbbbbbbbbbbbbbb";
    const GEN_C: &str = "cccccccccccccccc";
    const SHELL_NAMES: [&str; 8] = [
        "default",
        "default-aaaaaaaaaaaaaaaa",
        "default-bbbbbbbbbbbbbbbb",
        "default-cccccccccccccccc",
        ".staging-dddddddddddddddd",
        ".gc-eeeeeeeeeeeeeeee",
        "default-AAAAAAAAAAAAAAAA",
        "other",
    ];
    /// The three names a clean retires from [`SHELL_NAMES`] when the stamp keeps `GEN_A` and `GEN_B`.
    const RETIRED_SHELLS: [&str; 3] = [
        "default-cccccccccccccccc",
        ".staging-dddddddddddddddd",
        ".gc-eeeeeeeeeeeeeeee",
    ];

    fn stamp_for(
        home: &Path,
        scope: ocx_store::file_structure::RenderStampScope,
        shell_dir: &str,
        previous: &str,
    ) -> ocx_store::file_structure::RenderStampV2 {
        ocx_store::file_structure::RenderStampV2::new(
            home.to_path_buf(),
            scope,
            shell_dir.to_string(),
            previous.to_string(),
            "0".repeat(64),
            "g".to_string(),
            Default::default(),
            Default::default(),
        )
    }

    /// Creates `<home>/shells/<name>/bin/x` for each name in [`SHELL_NAMES`].
    async fn seed_shells(home: &Path) {
        for name in SHELL_NAMES {
            let dir = home.join("shells").join(name).join("bin");
            tokio::fs::create_dir_all(&dir).await.unwrap();
            tokio::fs::write(dir.join("x"), b"x").await.unwrap();
        }
    }

    fn manager_at(root: &Path) -> crate::PackageManager {
        crate::test_support::export_fixture::offline_manager(root)
    }

    async fn survivors(home: &Path) -> Vec<String> {
        let mut names = Vec::new();
        let mut entries = tokio::fs::read_dir(home.join("shells")).await.unwrap();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            names.push(entry.file_name().into_string().unwrap());
        }
        names.sort();
        names
    }

    fn expected_survivors() -> Vec<String> {
        let mut kept: Vec<String> = SHELL_NAMES
            .iter()
            .filter(|name| !RETIRED_SHELLS.contains(name))
            .map(|name| (*name).to_string())
            .collect();
        kept.sort();
        kept
    }

    /// A global home with a v2 stamp keeping `GEN_A` and `GEN_B`.
    async fn global_shells_home() -> (tempfile::TempDir, crate::PackageManager, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();
        let manager = manager_at(&root);
        let home = manager.file_structure().toolchain.home().root().to_path_buf();
        seed_shells(&home).await;
        let stamp = stamp_for(
            &home,
            ocx_store::file_structure::RenderStampScope::Global,
            &format!("default-{GEN_A}"),
            &format!("default-{GEN_B}"),
        );
        manager
            .file_structure()
            .state
            .set_render_stamp_v2(ocx_store::file_structure::RenderStampTarget::Global, &stamp)
            .unwrap();
        (tmp, manager, home)
    }

    #[tokio::test]
    async fn clean_retires_render_directories_the_stamp_does_not_keep() {
        let (_tmp, manager, home) = global_shells_home().await;
        let result = manager.clean(false, false).await.unwrap();

        assert_eq!(survivors(&home).await, expected_survivors());
        let mut reported: Vec<String> = result
            .launchers
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        reported.sort();
        let mut expected: Vec<String> = RETIRED_SHELLS.iter().map(|name| (*name).to_string()).collect();
        expected.sort();
        assert_eq!(reported, expected);
    }

    #[tokio::test]
    async fn clean_dry_run_reports_superseded_render_directories_without_removing() {
        let (_tmp, manager, home) = global_shells_home().await;
        let result = manager.clean(true, false).await.unwrap();

        assert_eq!(
            result.launchers.len(),
            RETIRED_SHELLS.len(),
            "got {:?}",
            result.launchers
        );
        assert_eq!(survivors(&home).await.len(), SHELL_NAMES.len());
    }

    /// A first render has an empty `previous_shell_dir`: only `shell_dir` is kept.
    #[tokio::test]
    async fn clean_keeps_only_shell_dir_when_previous_is_empty() {
        let (_tmp, manager, home) = global_shells_home().await;
        let stamp = stamp_for(
            &home,
            ocx_store::file_structure::RenderStampScope::Global,
            &format!("default-{GEN_A}"),
            "",
        );
        manager
            .file_structure()
            .state
            .set_render_stamp_v2(ocx_store::file_structure::RenderStampTarget::Global, &stamp)
            .unwrap();
        manager.clean(false, false).await.unwrap();

        let kept = survivors(&home).await;
        assert!(!kept.contains(&format!("default-{GEN_B}")));
        assert!(!kept.contains(&format!("default-{GEN_C}")));
        assert!(kept.contains(&format!("default-{GEN_A}")));
        assert!(kept.contains(&"default".to_string()));
    }

    /// The stamp is written after the `active` switch: until then (or after a failed stamp write) it
    /// names the old pair while `active` names a complete newer directory, which a clean must keep.
    #[tokio::test]
    async fn clean_keeps_the_directory_active_names_when_the_stamp_lags() {
        let (_tmp, manager, home) = global_shells_home().await;
        let target = format!("shells/default-{GEN_C}");
        ocx_util::fs::symlink::create(std::path::Path::new(&target), home.join("active")).unwrap();

        manager.clean(false, false).await.unwrap();

        let kept = survivors(&home).await;
        assert!(
            kept.contains(&format!("default-{GEN_C}")),
            "active's target survives: {kept:?}"
        );
        assert!(
            home.join("active").join("bin").join("x").exists(),
            "active is not dangling"
        );
        assert!(kept.contains(&format!("default-{GEN_A}")) && kept.contains(&format!("default-{GEN_B}")));
        assert!(!kept.contains(&".staging-dddddddddddddddd".to_string()));
    }

    #[tokio::test]
    async fn clean_treats_a_project_home_like_the_global_one() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();
        let manager = manager_at(&root);
        let project = root.join("proj");
        let home = project.join(".ocx").join("toolchain");
        seed_shells(&home).await;
        let key = ocx_store::reference_manager::ReferenceManager::name_for_path(&project);
        let stamp = stamp_for(
            &home,
            ocx_store::file_structure::RenderStampScope::Project(project.clone()),
            &format!("default-{GEN_A}"),
            &format!("default-{GEN_B}"),
        );
        manager
            .file_structure()
            .state
            .set_render_stamp_v2(ocx_store::file_structure::RenderStampTarget::Project(&key), &stamp)
            .unwrap();

        let result = manager.clean(false, false).await.unwrap();

        assert_eq!(survivors(&home).await, expected_survivors());
        assert_eq!(result.launchers.len(), RETIRED_SHELLS.len());
    }

    /// A stamp read under key K whose scope hashes to another key is not a stamp of that home.
    #[tokio::test]
    async fn clean_skips_a_project_stamp_whose_scope_disagrees_with_its_key() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();
        let manager = manager_at(&root);
        let home = root.join("proj").join(".ocx").join("toolchain");
        seed_shells(&home).await;
        let stamp = stamp_for(
            &home,
            ocx_store::file_structure::RenderStampScope::Project(root.join("proj")),
            &format!("default-{GEN_A}"),
            "",
        );
        manager
            .file_structure()
            .state
            .set_render_stamp_v2(
                ocx_store::file_structure::RenderStampTarget::Project("0123456789abcdef"),
                &stamp,
            )
            .unwrap();

        let result = manager.clean(false, false).await.unwrap();

        assert!(result.launchers.is_empty(), "got {:?}", result.launchers);
        assert_eq!(survivors(&home).await.len(), SHELL_NAMES.len());
    }

    #[tokio::test]
    async fn clean_leaves_a_home_without_a_v2_stamp_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();
        let manager = manager_at(&root);
        let home = manager.file_structure().toolchain.home().root().to_path_buf();
        seed_shells(&home).await;

        let result = manager.clean(false, false).await.unwrap();

        assert!(result.launchers.is_empty());
        assert_eq!(survivors(&home).await.len(), SHELL_NAMES.len());
    }

    /// A refusal leaves the directory whole and unreported: the parent is read-only.
    #[cfg(unix)]
    #[tokio::test]
    async fn clean_leaves_a_render_directory_whole_when_retiring_it_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let (_tmp, manager, home) = global_shells_home().await;
        let shells = home.join("shells");
        std::fs::set_permissions(&shells, std::fs::Permissions::from_mode(0o555)).unwrap();
        let writable = std::fs::File::create(shells.join(".probe")).is_ok();
        if writable {
            // Running as root: a read-only parent does not refuse anything.
            std::fs::set_permissions(&shells, std::fs::Permissions::from_mode(0o755)).unwrap();
            std::fs::remove_file(shells.join(".probe")).unwrap();
            return;
        }

        let result = manager.clean(false, false).await;
        std::fs::set_permissions(&shells, std::fs::Permissions::from_mode(0o755)).unwrap();

        let result = result.unwrap();
        assert!(result.launchers.is_empty(), "got {:?}", result.launchers);
        // `retire_dir` may empty a debris directory it cannot unlink; a published one stays whole.
        assert!(shells.join(RETIRED_SHELLS[0]).join("bin/x").exists());
    }

    /// Nothing an older ocx published, nor the active link, is written, renamed or deleted.
    #[cfg(unix)]
    #[tokio::test]
    async fn clean_leaves_what_an_older_ocx_published_byte_unchanged() {
        let (_tmp, manager, home) = global_shells_home().await;
        let fs = manager.file_structure().clone();
        let active = home.join("active");
        std::os::unix::fs::symlink("shells/default", &active).unwrap();
        let state = fs.state.global_render_stamp_file();
        tokio::fs::create_dir_all(state.parent().unwrap()).await.unwrap();
        tokio::fs::write(&state, br#"{"v":1}"#).await.unwrap();
        let v2 = fs.state.global_render_stamp_v2_file();
        // Rooted by the global lock, so the object collector keeps the package this test inspects.
        tokio::fs::write(fs.root().join("ocx.lock"), LOCK_WITH_ONE_TOOL)
            .await
            .unwrap();
        let cmake = seed_pinned_package_dir(&fs, "cmake", "localhost:5000", CMAKE_LEAF_HEX).await;
        let entrypoints = touch_dir(&fs.packages.entrypoints(&cmake)).await;
        let shim = touch_dir(&fs.shims.shim_dir(&cmake).bin()).await;
        let shim_binary = touch_dir(&fs.root().join(".bin").join("ocx-shim")).await;

        let snapshot = |paths: Vec<PathBuf>| -> Vec<(PathBuf, Vec<u8>)> {
            paths
                .into_iter()
                .map(|path| {
                    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
                    (path, bytes)
                })
                .collect()
        };
        let watched = vec![
            home.join("shells/default/bin/x"),
            state.clone(),
            v2.clone(),
            entrypoints.join("f"),
            shim.join("f"),
            shim_binary.join("f"),
        ];
        let before = snapshot(watched.clone());

        manager.clean(false, false).await.unwrap();

        assert_eq!(snapshot(watched), before);
        assert_eq!(std::fs::read_link(&active).unwrap(), PathBuf::from("shells/default"));
    }

    /// A symlinked `shells/` is refused: nothing under its target is removed.
    #[cfg(unix)]
    #[tokio::test]
    async fn clean_skips_a_home_whose_shells_directory_is_a_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();
        let manager = manager_at(&root);
        let home = manager.file_structure().toolchain.home().root().to_path_buf();
        let elsewhere = root.join("elsewhere");
        seed_shells(&elsewhere).await;
        tokio::fs::create_dir_all(&home).await.unwrap();
        std::os::unix::fs::symlink(elsewhere.join("shells"), home.join("shells")).unwrap();
        let stamp = stamp_for(
            &home,
            ocx_store::file_structure::RenderStampScope::Global,
            &format!("default-{GEN_A}"),
            "",
        );
        manager
            .file_structure()
            .state
            .set_render_stamp_v2(ocx_store::file_structure::RenderStampTarget::Global, &stamp)
            .unwrap();

        let result = manager.clean(false, false).await.unwrap();

        assert!(result.launchers.is_empty(), "got {:?}", result.launchers);
        assert_eq!(survivors(&elsewhere).await.len(), SHELL_NAMES.len());
    }

    /// Clean's lock spelling is the render's: a drift in scope, guarded directory or discriminator
    /// would let a clean run beside a live render.
    #[tokio::test]
    async fn clean_takes_the_lock_a_render_of_the_same_home_takes() {
        use super::super::render_toolchain::{RenderRequest, render_lock_parameters};
        use ocx_store::file_structure::{RenderStampScope, ToolchainHome};

        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();
        let fs = FileStructure::with_root(root.clone());
        tokio::fs::write(root.join("ocx.lock"), LOCK_WITH_ONE_TOOL)
            .await
            .unwrap();
        let lock = ProjectLock::from_path(&root.join("ocx.lock")).await.unwrap().unwrap();
        let platform = ocx_oci::Platform::any();
        let project = root.join("proj");
        let global_home = ToolchainHome::new(fs.toolchain.home().root());
        let project_home = ToolchainHome::new(project.join(".ocx").join("toolchain"));

        for (home, scope) in [
            (&global_home, RenderStampScope::Global),
            (&project_home, RenderStampScope::Project(project.clone())),
        ] {
            let rendered = render_lock_parameters(
                &fs,
                &RenderRequest {
                    unrendered: &[],
                    home,
                    scope: &scope,
                    lock: &lock,
                    surface: &[],
                    groups: &[],
                    pinned: false,
                    platform: &platform,
                    dry_run: false,
                },
            );
            let cleaned = clean_lock_parameters(&fs, home.root(), &scope);
            assert_eq!(cleaned.locks_root, rendered.locks_root);
            assert_eq!(cleaned.scope, rendered.scope);
            assert_eq!(cleaned.guarded_directory, rendered.guarded_directory);
            assert_eq!(cleaned.discriminator, rendered.discriminator);
        }
    }
}
