// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx patch sync`: host-level site-patch-tier refresh (`adr_infrastructure_patches.md § The site tier`).

use crate::{
    error::PackageErrorKind, tasks::patch_discovery::PatchDescriptorScope, tasks::patch_discovery::PatchDiscoveryMode,
    tasks::patch_discovery::PatchDiscoveryState, tasks::patch_discovery::PatchTagMap,
    tasks::patch_discovery::global_descriptor_id, tasks::patch_discovery::patch_descriptor_id,
};

use super::super::PackageManager;

// ── Fatal-vs-best-effort classification ────────────────────────────────────────

/// Whether a per-base discovery error aborts the sync: only a required companion's failure does.
fn is_fatal_sync_error(err: &PackageErrorKind) -> bool {
    matches!(err, PackageErrorKind::RequiredCompanionFailed { .. })
}

// ── Public report type ────────────────────────────────────────────────────────

/// Summary of a completed [`PackageManager::sync_patches`] run.
#[derive(Debug, Clone, Default)]
pub struct PatchSyncReport {
    /// Installed bases checked, plus the global root.
    pub bases_checked: usize,
    /// Distinct descriptor sources whose upstream digest advanced.
    pub descriptors_updated: usize,
    pub companions_installed: usize,
}

// ── Shared base enumerator ────────────────────────────────────────────────────

/// Every installed base identifier in the symlink store, with slugified registry hostnames restored.
pub async fn enumerate_installed_bases(manager: &PackageManager) -> crate::Result<Vec<ocx_oci::PackageRef>> {
    use ocx_util::fs::path_exists_lossy;

    let file_structure = manager.file_structure();
    let snapshot = manager.effective_index_store();
    let snapshot = &snapshot;
    let symlink_root = file_structure.symlinks.root().to_path_buf();

    let mut slug_base_ids: Vec<ocx_oci::PackageRef> = Vec::new();

    if path_exists_lossy(&symlink_root).await {
        match tokio::fs::read_dir(&symlink_root).await {
            Ok(mut registry_entries) => {
                while let Some(registry_entry) = registry_entries
                    .next_entry()
                    .await
                    .map_err(|e| crate::Error::InternalFile(symlink_root.clone(), e))?
                {
                    let registry_slug_path = registry_entry.path();
                    let registry_slug = registry_entry.file_name().to_string_lossy().to_string();
                    super::resolve::collect_candidates_from_dir(
                        &registry_slug_path,
                        &registry_slug,
                        &mut Vec::new(),
                        &mut slug_base_ids,
                    )
                    .await?;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Vec::new());
            }
            Err(error) => return Err(crate::Error::InternalFile(symlink_root.clone(), error)),
        }
    }

    let mut real_base_ids: Vec<ocx_oci::PackageRef> = Vec::with_capacity(slug_base_ids.len());
    for slug_id in &slug_base_ids {
        real_base_ids.push(super::resolve::recover_base_with_real_registry(snapshot, slug_id).await);
    }

    Ok(real_base_ids)
}

// ── Descriptor-advance detection ─────────────────────────────────────────────

/// Whether a descriptor appeared or its digest changed; a removal is not an advance.
fn descriptor_digest_advanced(before: &PatchDiscoveryState, after: &PatchDiscoveryState) -> bool {
    match (before, after) {
        (
            PatchDiscoveryState::NeverLooked | PatchDiscoveryState::LookedNoDescriptor,
            PatchDiscoveryState::LookedHasDescriptor { .. },
        ) => true,
        (
            PatchDiscoveryState::LookedHasDescriptor {
                manifest_digest: before_digest,
            },
            PatchDiscoveryState::LookedHasDescriptor {
                manifest_digest: after_digest,
            },
        ) => before_digest != after_digest,
        _ => false,
    }
}

// ── Tag-state read helper ─────────────────────────────────────────────────────

/// Reads the patch tag-store state for `tags_path`, warning and falling back to `NeverLooked` on an I/O error.
async fn read_tag_state_best_effort(tags_path: &std::path::Path, id: &ocx_oci::PackageRef) -> PatchDiscoveryState {
    match PatchTagMap::read(tags_path).await {
        Ok(state) => state,
        Err(err) => {
            log::warn!(
                "patch sync: failed to read tag-store state for '{}' ({}): {err}; \
                 treating as NeverLooked for this pass — descriptor advance may not be counted",
                id,
                tags_path.display()
            );
            PatchDiscoveryState::NeverLooked
        }
    }
}

/// The `ocx.toml` beside `lock_path`; `None` when it is absent.
///
/// # Errors
///
/// Any read or parse failure other than a missing file.
pub(crate) async fn read_lock_config(
    lock_path: &std::path::Path,
) -> Result<Option<ocx_project::ProjectConfig>, ocx_project::Error> {
    match ocx_project::ProjectConfig::from_path(&lock_path.with_file_name("ocx.toml")).await {
        Ok(config) => Ok(Some(config)),
        Err(ocx_project::Error::Project(ocx_project::ProjectError {
            kind: ocx_project::ProjectErrorKind::Io(error),
            ..
        })) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// `lock`'s entries as `ocx lock` and `ocx exec` discover their companions for `platform`:
/// digest-bearing, tagged only while `lock` binds to `config`. An entry with no leaf there has none.
pub(crate) fn lock_bases(
    lock: &ocx_project::ProjectLock,
    config: Option<&ocx_project::ProjectConfig>,
    platform: &ocx_oci::Platform,
) -> Vec<ocx_oci::PackageRef> {
    lock.lenient_host_identifiers(config, platform)
        .into_iter()
        .filter_map(|(_, identifier)| identifier.ok().map(Into::into))
        .collect()
}

/// How [`load_project_locks`] treats a registered project's unreadable `ocx.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnreadableConfig {
    /// Warn and use the lock's untagged leaves: a sync only installs companions, so a missed tagged base costs nothing lasting.
    Fallback,
    /// Fail: a freeze would write a snapshot missing the companions that project's tagged bases discover.
    Refuse,
}

/// Every live registered project lock and the home lock, with their configs, never pruning the
/// ledger; an unreadable ledger or lock is skipped with a warning.
///
/// # Errors
///
/// [`PatchError::ProjectConfigUnreadable`](crate::patch::PatchError::ProjectConfigUnreadable)
/// under [`UnreadableConfig::Refuse`].
async fn load_project_locks(
    file_structure: &ocx_store::file_structure::FileStructure,
    unreadable_config: UnreadableConfig,
) -> Result<Vec<(ocx_project::ProjectLock, Option<ocx_project::ProjectConfig>)>, crate::patch::PatchError> {
    let ocx_home = file_structure.root();
    let mut dirs = match ocx_project::ProjectRegistry::new(ocx_home)
        .live_projects_read_only()
        .await
    {
        Ok(dirs) => dirs,
        Err(error) => {
            log::warn!("patch bases: skipping registered projects, the ledger is unreadable: {error}");
            Vec::new()
        }
    };
    dirs.push(ocx_home.to_path_buf());

    let mut locks = Vec::new();
    for dir in dirs {
        let lock_path = dir.join("ocx.lock");
        let lock = match ocx_project::ProjectLock::from_path(&lock_path).await {
            Ok(Some(lock)) => lock,
            Ok(None) => continue,
            Err(error) => {
                log::warn!("patch bases: skipping unreadable lock in '{}': {error}", dir.display());
                continue;
            }
        };
        let config = match read_lock_config(&lock_path).await {
            Ok(config) => config,
            Err(source) if unreadable_config == UnreadableConfig::Refuse => {
                return Err(crate::patch::PatchError::ProjectConfigUnreadable {
                    path: lock_path.with_file_name("ocx.toml"),
                    source: Box::new(source),
                });
            }
            Err(error) => {
                log::warn!(
                    "patch bases: ignoring unreadable '{}', using the lock's untagged leaves: {error}",
                    lock_path.with_file_name("ocx.toml").display()
                );
                None
            }
        };
        locks.push((lock, config));
    }
    Ok(locks)
}

/// `installed` followed by the `extra` bases it lacks.
pub(crate) fn union_bases(
    mut installed: Vec<ocx_oci::PackageRef>,
    extra: impl IntoIterator<Item = ocx_oci::PackageRef>,
) -> Vec<ocx_oci::PackageRef> {
    let mut seen: std::collections::HashSet<ocx_oci::PackageRef> = installed.iter().cloned().collect();
    installed.extend(extra.into_iter().filter(|base| seen.insert(base.clone())));
    installed
}

impl PackageManager {
    /// Project-lock bases for `platform` from the live registered projects and the home lock,
    /// read-only; an unreadable ledger or lock warns and is skipped.
    ///
    /// # Errors
    ///
    /// [`PatchError::ProjectConfigUnreadable`](crate::patch::PatchError::ProjectConfigUnreadable)
    /// when a project's `ocx.toml` cannot be read or parsed.
    pub async fn project_patch_bases(&self, platform: &ocx_oci::Platform) -> crate::Result<Vec<ocx_oci::PackageRef>> {
        Ok(load_project_locks(self.file_structure(), UnreadableConfig::Refuse)
            .await?
            .iter()
            .flat_map(|(lock, config)| lock_bases(lock, config.as_ref(), platform))
            .collect())
    }
}

// ── PackageManager::sync_patches ──────────────────────────────────────────────

impl PackageManager {
    /// Re-fetches every descriptor for the installed bases, the project-lock bases and the global
    /// root, never crawling the registry.
    ///
    /// One pass per concrete platform in `platforms`, each with the project-lock bases built for
    /// that platform's leaves; an empty slice resolves no companions.
    ///
    /// # Errors
    ///
    /// `SnapshotActive` under a patch snapshot, `OfflineMode` when offline, and `RequiredCompanionFailed` from any
    /// base; other discovery errors only warn.
    pub async fn sync_patches(&self, platforms: &[ocx_oci::Platform]) -> crate::Result<PatchSyncReport> {
        if self.patch_snapshot().is_some() {
            return Err(crate::patch::PatchError::SnapshotActive.into());
        }
        let _client = self.require_client()?;

        let Some(patches) = self.patches() else {
            return Ok(PatchSyncReport::default());
        };

        let installed = enumerate_installed_bases(self).await?;
        let project_locks = load_project_locks(self.file_structure(), UnreadableConfig::Fallback).await?;
        // Per platform: a project base carries that platform's leaf, as `ocx lock` discovered it.
        let bases_per_platform: Vec<(&ocx_oci::Platform, Vec<ocx_oci::PackageRef>)> = platforms
            .iter()
            .map(|platform| {
                let project = project_locks
                    .iter()
                    .flat_map(|(lock, config)| lock_bases(lock, config.as_ref(), platform));
                (platform, union_bases(installed.clone(), project))
            })
            .collect();
        let all_bases = union_bases(
            Vec::new(),
            bases_per_platform.iter().flat_map(|(_, bases)| bases.iter().cloned()),
        );
        let total_checked = all_bases.len() + 1; // +1 for the global root

        let file_structure = self.file_structure();

        let mut companions_installed: usize = 0;

        // Keyed by tag-store path: tags of one repository share a source, counted once, not once per tag.
        let global_id = global_descriptor_id(patches);
        let global_tags_path = file_structure.patch_descriptor_path(&global_id);

        let mut sources: std::collections::BTreeMap<std::path::PathBuf, (ocx_oci::PackageRef, PatchDiscoveryState)> =
            std::collections::BTreeMap::new();
        let global_before = read_tag_state_best_effort(&global_tags_path, &global_id).await;
        sources.insert(global_tags_path.clone(), (global_id.clone(), global_before));
        for base_id in &all_bases {
            let pkg_id = patch_descriptor_id(patches, base_id);
            let pkg_tags_path = file_structure.patch_descriptor_path(&pkg_id);
            if let std::collections::btree_map::Entry::Vacant(slot) = sources.entry(pkg_tags_path.clone()) {
                let before = read_tag_state_best_effort(&pkg_tags_path, base_id).await;
                slot.insert((base_id.clone(), before));
            }
        }

        for (platform, bases) in &bases_per_platform {
            // `GlobalOnly`: a synthetic base would expand into a package-specific source outside the known set.
            if bases.is_empty() {
                match self
                    .discover_and_install_patches_with_mode(
                        &global_id,
                        platform,
                        PatchDiscoveryMode::Sync,
                        PatchDescriptorScope::GlobalOnly,
                    )
                    .await
                {
                    Ok(count) => companions_installed += count,
                    Err(err) => {
                        if is_fatal_sync_error(&err) {
                            return Err(err.into());
                        }
                        log::warn!("patch sync: global descriptor check failed: {err}; continuing");
                    }
                }
            } else {
                // `Both`, not `PackageSpecificOnly`, or a required global companion is absent for later offline execs.
                for base_id in bases {
                    match self
                        .discover_and_install_patches_with_mode(
                            base_id,
                            platform,
                            PatchDiscoveryMode::Sync,
                            PatchDescriptorScope::Both,
                        )
                        .await
                    {
                        Ok(count) => companions_installed += count,
                        Err(err) => {
                            if is_fatal_sync_error(&err) {
                                return Err(err.into());
                            }
                            log::warn!(
                                "patch sync: descriptor check for '{}' failed: {err}; continuing",
                                base_id
                            );
                        }
                    }
                }
            }
        }

        let mut descriptors_updated: usize = 0;
        for (tags_path, (id, before)) in &sources {
            let after = read_tag_state_best_effort(tags_path, id).await;
            if descriptor_digest_advanced(before, &after) {
                descriptors_updated += 1;
            }
        }

        Ok(PatchSyncReport {
            bases_checked: total_checked,
            descriptors_updated,
            companions_installed,
        })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::TempDir;

    use crate::PackageManager;
    use ocx_config::patch::ResolvedPatchConfig;
    use ocx_index::{ChainMode, Index, LocalConfig, LocalIndex};
    use ocx_store::file_structure::FileStructure;

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn make_offline_manager(ocx_home: &Path) -> PackageManager {
        let fs = FileStructure::with_root(ocx_home.to_path_buf());
        let local_index = LocalIndex::new(LocalConfig {
            index_store: ocx_index::IndexStore::machine_local(&fs),
        });
        let index = Index::from_chained(local_index, vec![], ChainMode::Offline);
        PackageManager::new(fs, index, None, "localhost:5000")
    }

    fn make_online_manager(ocx_home: &Path) -> PackageManager {
        use ocx_oci::ClientBuilder;
        let fs = FileStructure::with_root(ocx_home.to_path_buf());
        let local_index = LocalIndex::new(LocalConfig {
            index_store: ocx_index::IndexStore::machine_local(&fs),
        });
        let index = Index::from_chained(local_index, vec![], ChainMode::Offline);
        let client = ClientBuilder::new().build();
        PackageManager::new(fs, index, Some(client), "localhost:5000")
    }

    fn test_patch_config() -> ResolvedPatchConfig {
        ResolvedPatchConfig {
            system_required: false,
            no_patches: std::collections::BTreeSet::new(),
            registry: "patches.corp.com".to_string(),
            path_template: "{registry}/{repository}".to_string(),
            required: true,
        }
    }

    /// The patch registry every test that dials through [`make_online_manager`]
    /// points at: a loopback port nothing can be listening on.
    ///
    /// Port 1 is privileged, so no unprivileged process can bind it and the
    /// kernel refuses the connect on the first SYN — no DNS lookup, no route,
    /// no wait, the same answer on every machine.
    ///
    /// These tests used to leave [`test_patch_config`]'s `patches.corp.com` in
    /// place, which made "there is no registry" a property of whatever network
    /// the test ran on. Where the lookup or the route *black-holes* instead of
    /// refusing — a sandbox with no DNS egress is the ordinary case — the
    /// connect cannot fail until `ocx_oci`'s `REGISTRY_CONNECT_TIMEOUT` expires,
    /// and that is a flat 30 s per test: exactly what CI measured for the four
    /// tests below. Every one of them asserts that the descriptor fetch was
    /// *attempted* and found nothing, never which transport error came back, so
    /// a refused port witnesses the same property in a millisecond.
    const UNREACHABLE_PATCH_REGISTRY: &str = "127.0.0.1:1";

    /// [`test_patch_config`] aimed at [`UNREACHABLE_PATCH_REGISTRY`].
    fn unreachable_patch_config() -> ResolvedPatchConfig {
        ResolvedPatchConfig {
            registry: UNREACHABLE_PATCH_REGISTRY.to_string(),
            ..test_patch_config()
        }
    }

    // ── D4: offline sync_patches returns OfflineMode error ────────────────────

    /// `sync_patches` must return `Err(OfflineMode)` when the manager is offline.
    ///
    /// An explicit `ocx patch sync` is an online action. When offline, the user
    /// must receive a clear error rather than a silent success (unlike lazy
    /// discovery, which silently defers on offline installs).
    ///
    /// Traces: PIECE A offline posture; Phase 5C D4.
    #[tokio::test(flavor = "multi_thread")]
    async fn sync_patches_offline_returns_error() {
        let tmp = TempDir::new().unwrap();
        let manager = make_offline_manager(tmp.path()).with_patches(Some(test_patch_config()));
        assert!(manager.is_offline(), "setup: manager must be offline");

        let result = manager.sync_patches(&[ocx_oci::Platform::any()]).await;
        assert!(result.is_err(), "sync_patches must return Err when offline; got Ok");
        // Verify the error is OfflineMode specifically.
        let error_debug = format!("{:?}", result.unwrap_err());
        assert!(
            error_debug.contains("OfflineMode") || error_debug.contains("offline"),
            "sync_patches offline error must be OfflineMode; got: {error_debug}"
        );
    }

    /// `sync_patches` when no patch tier is configured returns an empty report
    /// (not an error). The offline guard fires BEFORE the no-patches check, but
    /// when a client is present and patches is None, the function early-returns.
    ///
    /// Traces: PIECE B no-patches short-circuit.
    #[tokio::test(flavor = "multi_thread")]
    async fn sync_patches_no_patches_config_returns_empty_report() {
        let tmp = TempDir::new().unwrap();
        // Online but no patches config.
        let manager = make_online_manager(tmp.path()); // patches = None
        assert!(!manager.is_offline(), "setup: manager must be online");
        assert!(manager.patches().is_none(), "setup: patches must be None");

        let result = manager.sync_patches(&[ocx_oci::Platform::any()]).await;
        // No error; report is the default empty struct.
        let report = result.expect("sync_patches with no patches config must return Ok");
        assert_eq!(report.bases_checked, 0);
        assert_eq!(report.descriptors_updated, 0);
        assert_eq!(report.companions_installed, 0);
    }

    // ── D3: enumerate_installed_bases returns only known repos ────────────────

    /// `enumerate_installed_bases` on an empty symlink store returns an empty Vec.
    ///
    /// Proves the enumerator does NOT crawl the registry — only the local
    /// symlink store is walked. An empty store means zero bases.
    ///
    /// Traces: Phase 5C D3; PIECE B enumeration contract.
    #[tokio::test(flavor = "multi_thread")]
    async fn enumerate_installed_bases_empty_store_returns_empty() {
        let tmp = TempDir::new().unwrap();
        // No symlink directory created — should return empty, not error.
        let result = enumerate_installed_bases(&make_offline_manager(tmp.path())).await;
        let bases = result.expect("enumerate_installed_bases on empty store must not error");
        assert!(
            bases.is_empty(),
            "empty symlink store must yield zero installed bases; got: {bases:?}"
        );
    }

    // ── PatchDiscoveryMode enum sanity ────────────────────────────────────────

    /// `PatchDiscoveryMode` implements `PartialEq` correctly for the two variants.
    ///
    /// Traces: PIECE A PatchDiscoveryMode enum definition.
    #[test]
    fn patch_discovery_mode_equality() {
        assert_eq!(PatchDiscoveryMode::Lazy, PatchDiscoveryMode::Lazy);
        assert_eq!(PatchDiscoveryMode::Sync, PatchDiscoveryMode::Sync);
        assert_ne!(
            PatchDiscoveryMode::Lazy,
            PatchDiscoveryMode::Sync,
            "Lazy and Sync must not be equal"
        );
    }

    // ── D2: Sync mode plumbing re-fetches all states ─────────────────────────

    /// `discover_and_install_patches_with_mode` in Sync mode is callable and
    /// short-circuits offline correctly (same as Lazy mode — the offline guard
    /// fires before the mode selector).
    ///
    /// Full re-fetch verification against a stub registry is deferred to the
    /// Phase 6 acceptance suite; this unit test proves the Sync path compiles
    /// and the per-base call chain is wired correctly.
    ///
    /// Traces: Phase 5C D2 — Sync mode plumbing.
    #[tokio::test(flavor = "multi_thread")]
    async fn sync_mode_offline_short_circuits() {
        let tmp = TempDir::new().unwrap();
        let manager = make_offline_manager(tmp.path()).with_patches(Some(test_patch_config()));
        let base_id = ocx_oci::PackageRef::parse("ocx.sh/cmake:3.28").expect("valid identifier");

        // In Sync mode, offline still short-circuits to Ok(()) at the
        // `is_offline()` check inside the method — the caller (sync_patches)
        // is responsible for surfacing the OfflineMode error to the user.
        let result = manager
            .discover_and_install_patches_with_mode(
                &base_id,
                &ocx_oci::Platform::any(),
                PatchDiscoveryMode::Sync,
                PatchDescriptorScope::Both,
            )
            .await;
        assert!(
            result.is_ok(),
            "Sync mode with offline manager must short-circuit to Ok(()) in the method body; got: {result:?}"
        );
    }

    // ── F-A: fatal-vs-best-effort classification ──────────────────────────────

    /// A `RequiredCompanionFailed` from a base's discovery is fatal: an explicit
    /// `ocx patch sync` that cannot install a `required` companion must exit
    /// non-zero (fail-closed, C7), not warn-and-succeed.
    ///
    /// Traces: F-A — sync fail-closed on required-companion failure.
    #[test]
    fn required_companion_failure_is_fatal_for_sync() {
        let companion = ocx_oci::PackageRef::parse("patches.corp.com/corp-ca:1.0").expect("valid identifier");
        let err = PackageErrorKind::RequiredCompanionFailed {
            companion,
            source: Box::new(PackageErrorKind::NotFound),
        };
        assert!(
            is_fatal_sync_error(&err),
            "RequiredCompanionFailed must abort an explicit sync (fail-closed)"
        );
    }

    /// Transient / optional discovery errors stay best-effort: they warn and let
    /// the sync continue refreshing the other bases (they must NOT abort).
    ///
    /// Traces: F-A — best-effort per-base recovery preserved for non-required errors.
    #[test]
    fn transient_discovery_errors_are_not_fatal_for_sync() {
        // A plain not-found (e.g. descriptor absent) and a descriptor-domain
        // (fetch/parse) error are both best-effort — one base's blip must not stop
        // the whole sync.
        assert!(
            !is_fatal_sync_error(&PackageErrorKind::NotFound),
            "a not-found is best-effort, not fatal, for sync"
        );
        let patch_err = PackageErrorKind::PatchDiscovery(crate::patch::PatchError::UnsupportedVersion { version: 999 });
        assert!(
            !is_fatal_sync_error(&patch_err),
            "a descriptor-domain error is best-effort, not fatal, for sync"
        );
    }

    /// `PatchSyncReport` derives `Debug` and `Clone`; `Default` yields zero counts.
    ///
    /// Traces: PIECE B PatchSyncReport type definition.
    #[test]
    fn patch_sync_report_default_is_zero() {
        let report = PatchSyncReport::default();
        assert_eq!(report.bases_checked, 0);
        assert_eq!(report.descriptors_updated, 0);
        assert_eq!(report.companions_installed, 0);
        // Debug must not panic.
        let _ = format!("{report:?}");
    }

    // ── Phase 5C regression guards ────────────────────────────────────────────
    //
    // These tests pin the implemented `sync_patches` contract: both
    // `companions_installed` and `descriptors_updated` are threaded back from
    // `discover_and_install_patches_with_mode`.
    //
    // Coverage deferred to the Phase 6 acceptance suite (real or stub registry):
    //   - Full round-trip: descriptor digest advances on the upstream registry, sync
    //     writes new blobs, and the new companion env value appears in resolve_env.
    //   - Verify that ONLY the known-set repos are queried (no registry crawl)
    //     by intercepting OCI transport requests.

    // ── D1: bases_checked reflects the installed set + global ────────────────────

    /// `sync_patches` with one installed base returns `bases_checked == 2`
    /// (one base + one global root). The base is seeded as a candidate symlink.
    ///
    /// No descriptor is seeded, so `descriptors_updated == 0` — the counter only
    /// advances when an upstream digest changes. `bases_checked == 2` proves the
    /// known-set enumeration counts the base plus the global root.
    ///
    /// Traces: Phase 5C D1 — bases_checked count; PIECE B sync over known set.
    #[tokio::test(flavor = "multi_thread")]
    async fn sync_patches_one_installed_base_reports_correct_bases_checked() {
        let tmp = TempDir::new().unwrap();
        let manager = make_online_manager(tmp.path()).with_patches(Some(unreachable_patch_config()));
        assert!(!manager.is_offline(), "setup: manager must be online");

        // Seed one installed base as a candidate symlink:
        //   symlinks/ocx_sh/cmake/candidates/3.28
        // enumerate_installed_bases walks this tree and returns the base identifier.
        let symlink_store = ocx_store::file_structure::SymlinkStore::new(tmp.path().join("symlinks"));
        let base_id = ocx_oci::PackageRef::new_registry("cmake", "ocx.sh").clone_with_tag("3.28");
        let candidate_path = symlink_store.candidate(&base_id);
        tokio::fs::create_dir_all(candidate_path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&candidate_path, b"").await.unwrap();

        // Run sync (will attempt network calls and log warnings — that is OK
        // since sync is best-effort per-base).
        let report = manager
            .sync_patches(&[ocx_oci::Platform::any()])
            .await
            .expect("sync_patches with one installed base must return Ok");

        // total_checked = installed_bases.len() + 1 = 1 + 1 = 2 (base + global root).
        assert_eq!(
            report.bases_checked, 2,
            "sync_patches with one installed base must report bases_checked == 2 (base + global)"
        );

        // No descriptor seeded, so the advance count stays 0.
        assert_eq!(
            report.descriptors_updated, 0,
            "sync_patches with no seeded descriptors must report descriptors_updated == 0"
        );
    }

    // ── D1: resolve.json unchanged after sync ──────────────────────────

    /// `sync_patches` must NOT modify the base package's `resolve.json` or the
    /// install directory. Sync operates on the site-patch tier (descriptor blobs +
    /// companion installs) — it never rewrites base-package metadata.
    ///
    /// Setup: write a minimal `resolve.json` for an installed base package, then
    /// call `sync_patches`. The `resolve.json` bytes must be BYTE-IDENTICAL after
    /// the call.
    ///
    /// Regression guard: `sync_patches` never touches the package store, so the
    /// base package's `resolve.json` must be byte-identical after the call.
    ///
    /// Traces: Phase 5C D1 — base install dir + resolve.json are BYTE-UNCHANGED.
    #[tokio::test(flavor = "multi_thread")]
    async fn sync_patches_does_not_modify_base_resolve_json() {
        let tmp = TempDir::new().unwrap();
        let manager = make_online_manager(tmp.path()).with_patches(Some(unreachable_patch_config()));

        // Write a minimal resolve.json in a synthetic package directory.
        let pkg_dir = tmp.path().join("fake_pkg");
        tokio::fs::create_dir_all(&pkg_dir).await.unwrap();
        let resolve_json_path = pkg_dir.join("resolve.json");
        let resolve_json_content = r#"{"dependencies":[]}"#;
        tokio::fs::write(&resolve_json_path, resolve_json_content.as_bytes())
            .await
            .unwrap();

        // Seed a candidate symlink for enumerate_installed_bases.
        let symlink_store = ocx_store::file_structure::SymlinkStore::new(tmp.path().join("symlinks"));
        let base_id = ocx_oci::PackageRef::new_registry("cmake", "ocx.sh").clone_with_tag("3.28");
        let candidate_path = symlink_store.candidate(&base_id);
        tokio::fs::create_dir_all(candidate_path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&candidate_path, b"").await.unwrap();

        // Run sync.
        manager
            .sync_patches(&[ocx_oci::Platform::any()])
            .await
            .expect("sync_patches must not fail");

        // Verify resolve.json is byte-unchanged.
        let after = tokio::fs::read(&resolve_json_path)
            .await
            .expect("resolve.json must still exist after sync");
        assert_eq!(
            after,
            resolve_json_content.as_bytes(),
            "sync_patches must NOT modify resolve.json; bytes changed"
        );
    }

    // ── descriptors_updated stays 0 when the upstream digest is unchanged ────────

    /// `sync_patches` must report `descriptors_updated == 0` when the upstream
    /// serves the SAME descriptor digest already recorded in the tag-store — a
    /// re-fetch that finds no change is not an advance.
    ///
    /// Setup: seed the global tag-store + CAS with a descriptor at digest D, then
    /// have the stub registry serve that SAME manifest for the global id. The
    /// re-fetch succeeds (idempotent `write_has_descriptor` on the same digest),
    /// so `descriptor_digest_advanced(D, D)` is false and the count stays 0.
    ///
    /// Complements `tdd_d1_descriptors_updated_gt_zero_after_digest_advance`
    /// (the advance case): together they pin both directions of the counter.
    #[tokio::test(flavor = "multi_thread")]
    async fn sync_patches_descriptors_updated_stays_zero_when_digest_unchanged() {
        let tmp = TempDir::new().unwrap();
        let patch_config = test_patch_config();

        let fs = ocx_store::file_structure::FileStructure::with_root(tmp.path().to_path_buf());
        let blob_store = &fs.blobs;
        let tag_store = &fs;

        // OCI artifact manifests require a `config` field for valid serde deserialization.
        let empty_config_descriptor = serde_json::json!({
            "mediaType": "application/vnd.oci.empty.v1+json",
            "digest": "sha256:44136fa355ba77b9ad7b35f2cca4bb730ad02e2e8dc7f2af7a1b3e7c0ef5c6a7",
            "size": 2
        });

        // Build a single descriptor (no companions) at digest D.
        let layer_json = serde_json::json!({"version": 1, "rules": [{"match": "*", "packages": []}]}).to_string();
        let layer_bytes = layer_json.as_bytes();
        let layer_digest = ocx_oci::Algorithm::Sha256.hash(layer_bytes);
        let manifest_json = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "artifactType": "application/vnd.sh.ocx.patch.v1",
            "config": empty_config_descriptor,
            "layers": [{"mediaType": "application/vnd.sh.ocx.patch.descriptor.v1+json",
                "digest": layer_digest.to_string(), "size": layer_bytes.len()}]
        })
        .to_string();
        let manifest_bytes = manifest_json.as_bytes();
        let manifest_digest = ocx_oci::Algorithm::Sha256.hash(manifest_bytes);

        // Seed the descriptor into CAS + tag-store at digest D (LookedHasDescriptor).
        blob_store
            .write_blob("patches.corp.com", &manifest_digest, manifest_bytes)
            .await
            .unwrap();
        blob_store
            .write_blob("patches.corp.com", &layer_digest, layer_bytes)
            .await
            .unwrap();
        let global_id = crate::tasks::patch_discovery::global_descriptor_id(&patch_config);
        let global_tags_path = tag_store.patch_descriptor_path(&global_id);
        crate::tasks::patch_discovery::PatchTagMap::write_has_descriptor(
            &global_tags_path,
            &manifest_digest.to_string(),
        )
        .await
        .unwrap();

        // Stub serves the SAME manifest (digest D) for the global id → re-fetch
        // finds no change. No base is seeded → sync runs the GlobalOnly pass.
        use ocx_oci::Client;
        use ocx_oci::client::test_transport::{StubTransport, StubTransportData};
        let stub_data = StubTransportData::new();
        {
            let mut inner = stub_data.write();
            inner.manifests.insert(
                "patches.corp.com/global:__ocx.patch".to_string(),
                (manifest_bytes.to_vec(), manifest_digest.to_string()),
            );
            inner.blobs.insert(layer_digest.to_string(), layer_bytes.to_vec());
        }
        let stub_client = Client::with_transport(Box::new(StubTransport::new(stub_data)));
        let local_index = ocx_index::LocalIndex::new(ocx_index::LocalConfig {
            index_store: ocx_index::IndexStore::machine_local(tag_store),
        });
        let index = ocx_index::Index::from_chained(local_index, vec![], ocx_index::ChainMode::Offline);
        let manager =
            PackageManager::new(fs, index, Some(stub_client), "localhost:5000").with_patches(Some(patch_config));

        let report = manager
            .sync_patches(&[ocx_oci::Platform::any()])
            .await
            .expect("sync_patches must return Ok");

        // Zero installed bases + global root → one source checked.
        assert_eq!(
            report.bases_checked, 1,
            "sync_patches with no installed bases must report bases_checked = 1 (global root only)"
        );
        // The upstream digest matched the recorded one, so nothing advanced.
        assert_eq!(
            report.descriptors_updated, 0,
            "an unchanged upstream digest must not be counted as a descriptor advance"
        );
    }

    /// `descriptor_digest_advanced` counts exactly the transitions that represent
    /// a real upstream digest advance, and nothing else.
    ///
    /// This is the pure predicate behind `PatchSyncReport::descriptors_updated`:
    /// a new descriptor appearing or an existing digest changing is an advance;
    /// an identical digest, a regression, or a same-state no-op is not.
    #[test]
    fn descriptor_digest_advanced_counts_only_real_advances() {
        let has = |digest: &str| PatchDiscoveryState::LookedHasDescriptor {
            manifest_digest: digest.to_string(),
        };

        // Advances: no descriptor → has descriptor.
        assert!(descriptor_digest_advanced(
            &PatchDiscoveryState::NeverLooked,
            &has("sha256:aa")
        ));
        assert!(descriptor_digest_advanced(
            &PatchDiscoveryState::LookedNoDescriptor,
            &has("sha256:aa")
        ));
        // Advance: the recorded digest changed (upstream published a new descriptor).
        assert!(descriptor_digest_advanced(&has("sha256:old"), &has("sha256:new")));

        // Not advances: identical digest (idempotent re-fetch).
        assert!(!descriptor_digest_advanced(&has("sha256:aa"), &has("sha256:aa")));
        // Not advances: a regression (upstream removed the descriptor) is warned, not counted.
        assert!(!descriptor_digest_advanced(
            &has("sha256:aa"),
            &PatchDiscoveryState::LookedNoDescriptor
        ));
        // Not advances: still nothing to fetch.
        assert!(!descriptor_digest_advanced(
            &PatchDiscoveryState::NeverLooked,
            &PatchDiscoveryState::LookedNoDescriptor
        ));
    }

    // ── D2: Sync mode re-fetches LookedNoDescriptor + LookedHasDescriptor ──

    /// Lazy mode SKIPS a `LookedNoDescriptor` base (no tag-store change).
    /// Sync mode ATTEMPTS to re-fetch it (leaves `LookedNoDescriptor` on network
    /// miss, or writes `LookedHasDescriptor` on success).
    ///
    /// Test plan:
    ///   1. Seed `LookedNoDescriptor` for the global descriptor.
    ///   2. Run `discover_and_install_patches_with_mode` in LAZY mode → assert
    ///      the tag-store file is UNCHANGED (skip fires, no write).
    ///   3. Run `discover_and_install_patches_with_mode` in SYNC mode with an
    ///      online manager → the function ATTEMPTS to re-fetch. Since no real
    ///      registry is present, the fetch fails (network error from
    ///      `fetch_and_persist_descriptor`), so the method returns Err — proving
    ///      the re-fetch was attempted rather than skipped.
    ///
    /// This test pins the Sync-vs-Lazy plumbing (Sync does not skip
    /// `LookedNoDescriptor`). Full end-to-end verification (state transitions to
    /// LookedHasDescriptor against a stub OCI transport) is covered by the
    /// digest-advance tests below and the Phase 6 acceptance suite.
    ///
    /// Traces: Phase 5C D2 — Sync re-fetches all three states.
    #[tokio::test(flavor = "multi_thread")]
    async fn sync_mode_refetches_looked_no_descriptor_state() {
        let tmp = TempDir::new().unwrap();
        let patch_config = unreachable_patch_config();

        // Offline manager — we use it for the LAZY test.
        let offline_manager = make_offline_manager(tmp.path()).with_patches(Some(patch_config.clone()));
        let fs = ocx_store::file_structure::FileStructure::with_root(tmp.path().to_path_buf());
        let tag_store = &fs;

        let base_id = ocx_oci::PackageRef::parse("ocx.sh/cmake:3.28").expect("valid identifier");

        // Seed LookedNoDescriptor for the global descriptor.
        let global_id = crate::tasks::patch_discovery::global_descriptor_id(&patch_config);
        let global_tags_path = tag_store.patch_descriptor_path(&global_id);
        crate::tasks::patch_discovery::PatchTagMap::write_no_descriptor(&global_tags_path)
            .await
            .unwrap();

        // Verify the state is LookedNoDescriptor before calling.
        let state_before = crate::tasks::patch_discovery::PatchTagMap::read(&global_tags_path)
            .await
            .expect("read must succeed");
        assert_eq!(
            state_before,
            crate::tasks::patch_discovery::PatchDiscoveryState::LookedNoDescriptor,
            "setup: global descriptor must be LookedNoDescriptor"
        );

        // ── Step 1: Lazy mode must SKIP LookedNoDescriptor ──
        // With an offline manager, `discover_and_install_patches_with_mode` short-circuits
        // at `is_offline()` regardless of mode. So we use an offline manager here and
        // verify the state is unchanged (offline short-circuit is the skip mechanism in Lazy
        // offline mode; Lazy online mode is separately tested by Phase 3 tests).
        let result = offline_manager
            .discover_and_install_patches_with_mode(
                &base_id,
                &ocx_oci::Platform::any(),
                PatchDiscoveryMode::Lazy,
                PatchDescriptorScope::Both,
            )
            .await;
        // Offline: returns Ok(()) immediately regardless of state.
        assert!(
            result.is_ok(),
            "Lazy mode offline must return Ok(()) on LookedNoDescriptor; got: {result:?}"
        );

        // Tag-store state must be unchanged (offline short-circuit is not a write).
        let state_after_lazy = crate::tasks::patch_discovery::PatchTagMap::read(&global_tags_path)
            .await
            .expect("read must succeed");
        assert_eq!(
            state_after_lazy,
            crate::tasks::patch_discovery::PatchDiscoveryState::LookedNoDescriptor,
            "Lazy mode must not write to tag-store when offline (LookedNoDescriptor unchanged)"
        );

        // ── Step 2: Sync mode ATTEMPTS re-fetch ──
        // Use an online manager to bypass the is_offline() short-circuit.
        // Network will fail (no real registry), but the Sync mode branch MUST be
        // entered (proved by the fact that fetch_and_persist_descriptor is called,
        // which calls require_client() → succeeds for online manager, then calls
        // fetch_patch_descriptor_blobs → network error).
        //
        // The resulting error from fetch_and_persist_descriptor propagates as
        // PackageErrorKind::PatchDiscovery (or Internal depending on error type).
        // The important property: Sync mode DID NOT SKIP the LookedNoDescriptor state.
        //
        // Sync mode enters the LookedNoDescriptor branch and calls
        // fetch_and_persist_descriptor. This test verifies the plumbing (Sync does
        // not skip the state) — not the network outcome.
        let online_manager = make_online_manager(tmp.path()).with_patches(Some(patch_config.clone()));
        let result_sync = online_manager
            .discover_and_install_patches_with_mode(
                &base_id,
                &ocx_oci::Platform::any(),
                PatchDiscoveryMode::Sync,
                PatchDescriptorScope::Both,
            )
            .await;
        // With no real registry, the fetch fails. The method returns Err, proving
        // the re-fetch attempt was made (not skipped). If the Sync branch had skipped
        // LookedNoDescriptor (like Lazy does), it would have returned Ok(()) here
        // (because the LookedNoDescriptor offline short-circuit is not in play — we
        // are online; Sync mode's only skip is the is_offline() check at the top).
        //
        // POST-IMPLEMENTATION NOTE: when a real or stub registry is available, the
        // state should advance to LookedHasDescriptor (or stay LookedNoDescriptor if
        // no descriptor exists upstream). For now we assert the fetch was ATTEMPTED
        // by checking that an error was returned (network miss), not Ok(()).
        //
        // DEFERRED to Phase 6: verify state transitions with stub OCI transport.
        assert!(
            result_sync.is_err(),
            "Sync mode online with no real registry must return Err (fetch attempted, network failed); \
             got Ok — this means the LookedNoDescriptor state was SKIPPED, which violates the Sync contract"
        );
    }

    /// Lazy mode SKIPS a `LookedHasDescriptor` state (uses cached descriptor).
    /// Sync mode RE-FETCHES it (ignores cached state, contacts registry).
    ///
    /// Seed `LookedHasDescriptor` with old blobs in CAS, then:
    ///   - Lazy mode: returns Ok(()) quickly (loads from CAS, no network).
    ///   - Sync mode online: contacts registry (fails — no real registry),
    ///     proves the re-fetch was attempted.
    ///
    /// Traces: Phase 5C D2 — Sync re-fetches LookedHasDescriptor.
    #[tokio::test(flavor = "multi_thread")]
    async fn sync_mode_refetches_looked_has_descriptor_state() {
        let tmp = TempDir::new().unwrap();
        let patch_config = unreachable_patch_config();

        let fs = ocx_store::file_structure::FileStructure::with_root(tmp.path().to_path_buf());
        let blob_store = &fs.blobs;
        let tag_store = &fs;

        // Build a valid descriptor and seed it into CAS + tag-store.
        let layer_json = serde_json::json!({
            "version": 1,
            "rules": [{ "match": "*", "packages": [] }]
        })
        .to_string();
        let layer_bytes = layer_json.as_bytes();
        let layer_digest = ocx_oci::Algorithm::Sha256.hash(layer_bytes);
        let manifest_json = serde_json::json!({
            "schemaVersion": 2,
            "layers": [{"mediaType": "application/octet-stream", "digest": layer_digest.to_string(), "size": layer_bytes.len()}]
        })
        .to_string();
        let manifest_bytes = manifest_json.as_bytes();
        let manifest_digest = ocx_oci::Algorithm::Sha256.hash(manifest_bytes);

        // Seeded under the configured patch registry, not a literal: the CAS
        // namespace is derived from it, so a seed written elsewhere is a
        // descriptor the manager can never load and the `LookedHasDescriptor`
        // precondition this test is named for would be half-built.
        blob_store
            .write_blob(UNREACHABLE_PATCH_REGISTRY, &manifest_digest, manifest_bytes)
            .await
            .unwrap();
        blob_store
            .write_blob(UNREACHABLE_PATCH_REGISTRY, &layer_digest, layer_bytes)
            .await
            .unwrap();

        let global_id = crate::tasks::patch_discovery::global_descriptor_id(&patch_config);
        let global_tags_path = tag_store.patch_descriptor_path(&global_id);
        crate::tasks::patch_discovery::PatchTagMap::write_has_descriptor(
            &global_tags_path,
            &manifest_digest.to_string(),
        )
        .await
        .unwrap();

        let base_id = ocx_oci::PackageRef::parse("ocx.sh/cmake:3.28").expect("valid");

        // ── Lazy mode: loads from CAS (offline OK) ──
        let offline_manager = make_offline_manager(tmp.path()).with_patches(Some(patch_config.clone()));
        let lazy_result = offline_manager
            .discover_and_install_patches_with_mode(
                &base_id,
                &ocx_oci::Platform::any(),
                PatchDiscoveryMode::Lazy,
                PatchDescriptorScope::Both,
            )
            .await;
        // Offline short-circuit fires before state processing — Ok(()).
        assert!(
            lazy_result.is_ok(),
            "Lazy mode offline must return Ok(); got: {lazy_result:?}"
        );

        // ── Sync mode: MUST re-fetch from registry (not just load from CAS) ──
        let online_manager = make_online_manager(tmp.path()).with_patches(Some(patch_config.clone()));
        let sync_result = online_manager
            .discover_and_install_patches_with_mode(
                &base_id,
                &ocx_oci::Platform::any(),
                PatchDiscoveryMode::Sync,
                PatchDescriptorScope::Both,
            )
            .await;
        // Network fails (no real registry) → Err proves re-fetch was attempted.
        // If Sync mode had incorrectly loaded from CAS (like Lazy) it would return Ok(())
        // (no network call, no companion install attempt, no error).
        assert!(
            sync_result.is_err(),
            "Sync mode with LookedHasDescriptor and online manager must return Err \
             (re-fetch attempted, network failed) — got Ok, which means CAS was used instead"
        );
    }

    // ── D3: enumerate_installed_bases returns ONLY the known set ─────────────────

    /// `enumerate_installed_bases` with seeded symlink entries returns exactly
    /// the seeded identifiers — no registry crawl, no extra entries.
    ///
    /// Seeds two bases and verifies the returned Vec has exactly two elements
    /// with the expected registry and repository values.
    ///
    /// Regression guard: `enumerate_installed_bases` walks only the symlink store,
    /// so the enumeration stays bounded to installed bases and never acquires
    /// network access.
    ///
    /// Traces: Phase 5C D3 — enumeration contract: only known repos queried.
    #[tokio::test(flavor = "multi_thread")]
    async fn enumerate_installed_bases_returns_only_seeded_bases() {
        let tmp = TempDir::new().unwrap();

        // Seed two distinct installed bases.
        let symlink_store = ocx_store::file_structure::SymlinkStore::new(tmp.path().join("symlinks"));
        let base_cmake = ocx_oci::PackageRef::new_registry("cmake", "ocx.sh").clone_with_tag("3.28");
        let base_ninja = ocx_oci::PackageRef::new_registry("ninja", "ocx.sh").clone_with_tag("1.12");

        for base_id in [&base_cmake, &base_ninja] {
            let candidate_path = symlink_store.candidate(base_id);
            tokio::fs::create_dir_all(candidate_path.parent().unwrap())
                .await
                .unwrap();
            tokio::fs::write(&candidate_path, b"").await.unwrap();
        }

        let bases = enumerate_installed_bases(&make_offline_manager(tmp.path()))
            .await
            .expect("enumerate must succeed");

        // Returns exactly the seeded bases.
        assert_eq!(
            bases.len(),
            2,
            "enumerate_installed_bases must return exactly the 2 seeded bases; got: {bases:?}"
        );

        // Both seeded repositories must appear (order may vary due to dir iteration).
        let repos: Vec<&str> = bases.iter().map(|id| id.repository()).collect();
        assert!(
            repos.contains(&"cmake"),
            "cmake must be in the enumerated bases; got: {repos:?}"
        );
        assert!(
            repos.contains(&"ninja"),
            "ninja must be in the enumerated bases; got: {repos:?}"
        );

        // Registries must be recovered (no slugified form like `ocx_sh`).
        for base in &bases {
            assert!(
                base.registry().contains('.') || base.registry().contains(':'),
                "registry must be a real hostname (not a slug); got: {}",
                base.registry()
            );
        }
    }

    // ── D4 (PASSES — regression guard): offline sync returns OfflineMode, not Ok ──
    //
    // The existing `sync_patches_offline_returns_error` test above covers D4.
    // This additional test proves the distinction: an offline `ocx patch sync`
    // returns Err(OfflineMode) while lazy discovery (side-effect of install) would
    // return Ok(()) on offline (different design intent).
    //
    // This regression guard ensures an offline sync is NEVER silently a no-op.

    /// Offline `sync_patches` returns `Err(OfflineMode)` — never `Ok(empty_report)`.
    ///
    /// Distinguishes sync from lazy discovery: lazy discovery silently defers on
    /// offline (it is a side-effect of install); sync is an explicit user action
    /// that must fail loudly when the network is unavailable.
    ///
    /// Regression guard for the offline-error posture.
    ///
    /// Traces: Phase 5C D4 — offline posture is Err, not silent Ok.
    #[tokio::test(flavor = "multi_thread")]
    async fn sync_patches_offline_is_err_not_silent_ok() {
        let tmp = TempDir::new().unwrap();
        let manager = make_offline_manager(tmp.path()).with_patches(Some(test_patch_config()));

        let result = manager.sync_patches(&[ocx_oci::Platform::any()]).await;
        // The result must be Err — any Ok result violates the offline posture.
        assert!(
            result.is_err(),
            "offline sync_patches must return Err; a silent Ok would hide the network unavailability from the user"
        );

        // Verify the error is NOT a data corruption or config error — it must be
        // the specific OfflineMode error indicating the network was unavailable.
        let error = result.unwrap_err();
        let error_debug = format!("{error:?}");
        assert!(
            error_debug.contains("OfflineMode") || error_debug.contains("offline") || error_debug.contains("Offline"),
            "offline sync_patches error must be OfflineMode, not a different error type; got: {error_debug}"
        );
    }

    // ── Phase 5C descriptor-advance regression guards ────────────────────────
    //
    // The tests below pin the descriptor-advance counting through StubTransport:
    // an advanced upstream digest bumps `descriptors_updated` and rewrites the
    // tag-store pointer.
    //
    // Coverage deferred to the Phase 6 acceptance suite:
    //   - `resolve_env` re-derives the NEW companion value after sync — requires
    //     a companion package installed in the object store.

    // ── D1: descriptors_updated > 0 when StubTransport serves new descriptor ──

    /// `sync_patches` must return `descriptors_updated > 0` when the stub registry
    /// serves a descriptor with a DIFFERENT digest than the one recorded in the
    /// tag-store (upstream digest advanced).
    ///
    /// Setup:
    ///   1. Seed the global tag-store with an old manifest digest (LookedHasDescriptor).
    ///   2. Seed the StubTransport with a NEW manifest (new digest) + its layer blob.
    ///   3. Run `sync_patches` with the stub-backed online manager.
    ///   4. Assert `report.descriptors_updated >= 1`.
    ///
    /// The Reference key used by `StubTransport::pull_manifest_raw` is the
    /// canonical string form of the identifier. The global descriptor lives at
    /// the reserved `global` repository, so the key is the unambiguous
    /// `"patches.corp.com/global:__ocx.patch"`.
    ///
    /// Traces: Phase 5C D1 — descriptors_updated counter.
    #[tokio::test(flavor = "multi_thread")]
    async fn tdd_d1_descriptors_updated_gt_zero_after_digest_advance() {
        let tmp = TempDir::new().unwrap();
        let patch_config = test_patch_config();

        let fs = ocx_store::file_structure::FileStructure::with_root(tmp.path().to_path_buf());
        let blob_store = &fs.blobs;
        let tag_store = &fs;

        // OCI artifact manifests require a `config` field for valid serde deserialization.
        // We use the standard empty-artifact config descriptor per OCI Image Spec v1.1.
        // `fetch_patch_descriptor_blobs` only fetches `layers[0]`, not the config blob,
        // so we do not need to seed the config blob in the CAS or StubTransport.
        let empty_config_descriptor = serde_json::json!({
            "mediaType": "application/vnd.oci.empty.v1+json",
            "digest": "sha256:44136fa355ba77b9ad7b35f2cca4bb730ad02e2e8dc7f2af7a1b3e7c0ef5c6a7",
            "size": 2
        });

        // ── Build the OLD descriptor (already in CAS + tag-store) ──
        let old_layer_json = serde_json::json!({
            "version": 1,
            "rules": [{ "match": "*", "packages": [] }]
        })
        .to_string();
        let old_layer_bytes = old_layer_json.as_bytes();
        let old_layer_digest = ocx_oci::Algorithm::Sha256.hash(old_layer_bytes);
        let old_manifest_json = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "artifactType": "application/vnd.sh.ocx.patch.v1",
            "config": empty_config_descriptor,
            "layers": [{
                "mediaType": "application/vnd.sh.ocx.patch.descriptor.v1+json",
                "digest": old_layer_digest.to_string(),
                "size": old_layer_bytes.len()
            }]
        })
        .to_string();
        let old_manifest_bytes = old_manifest_json.as_bytes();
        let old_manifest_digest = ocx_oci::Algorithm::Sha256.hash(old_manifest_bytes);

        // Seed the OLD descriptor into CAS.
        blob_store
            .write_blob("patches.corp.com", &old_manifest_digest, old_manifest_bytes)
            .await
            .unwrap();
        blob_store
            .write_blob("patches.corp.com", &old_layer_digest, old_layer_bytes)
            .await
            .unwrap();

        // Seed the global tag-store with LookedHasDescriptor (old digest).
        let global_id = crate::tasks::patch_discovery::global_descriptor_id(&patch_config);
        let global_tags_path = tag_store.patch_descriptor_path(&global_id);
        crate::tasks::patch_discovery::PatchTagMap::write_has_descriptor(
            &global_tags_path,
            &old_manifest_digest.to_string(),
        )
        .await
        .unwrap();

        // ── Build the NEW descriptor (what the stub registry now serves) ──
        // NOTE: `packages` is a flat list of identifier strings (not objects).
        // See PatchRule.packages: Vec<PackageRef> which deserializes from strings.
        //
        // The companion is marked `required: false` because this test measures the
        // `descriptors_updated` counter, not companion enforcement — the stub
        // registry serves the descriptor manifest but not the companion package, so
        // a `required` companion would (correctly) fail the sync closed (F-A) and
        // mask the descriptor-advance assertion. Required-companion fail-closed
        // during sync is covered separately by `required_companion_failure_is_fatal_for_sync`
        // and the acceptance suite.
        let new_layer_json = serde_json::json!({
            "version": 1,
            "rules": [{ "match": "*", "packages": ["patches.corp.com/ca-certs:latest"], "required": false }]
        })
        .to_string();
        let new_layer_bytes = new_layer_json.as_bytes();
        let new_layer_digest = ocx_oci::Algorithm::Sha256.hash(new_layer_bytes);
        let new_manifest_json = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "artifactType": "application/vnd.sh.ocx.patch.v1",
            "config": empty_config_descriptor,
            "layers": [{
                "mediaType": "application/vnd.sh.ocx.patch.descriptor.v1+json",
                "digest": new_layer_digest.to_string(),
                "size": new_layer_bytes.len()
            }]
        })
        .to_string();
        let new_manifest_bytes = new_manifest_json.as_bytes();
        let new_manifest_digest = ocx_oci::Algorithm::Sha256.hash(new_manifest_bytes);
        assert_ne!(
            old_manifest_digest, new_manifest_digest,
            "setup: old and new descriptors must have different digests for this test to be meaningful"
        );

        // ── Seed the StubTransport with the NEW manifest + layer blob ──
        //
        // `StubTransport::pull_manifest_raw` keys manifests by the transport
        // reference string. The global descriptor lives at the reserved `global`
        // repository, so the key is the unambiguous
        // `patches.corp.com/global:__ocx.patch`.
        use ocx_oci::Client;
        use ocx_oci::client::test_transport::{StubTransport, StubTransportData};

        let stub_data = StubTransportData::new();
        {
            let mut inner = stub_data.write();
            let manifest_pair = (new_manifest_bytes.to_vec(), new_manifest_digest.to_string());
            inner
                .manifests
                .insert("patches.corp.com/global:__ocx.patch".to_string(), manifest_pair);
            // Seed the layer blob so `fetch_single_layer_artifact` can pull it.
            inner
                .blobs
                .insert(new_layer_digest.to_string(), new_layer_bytes.to_vec());
        }

        // Build the stub-backed online client.
        let stub_client = Client::with_transport(Box::new(StubTransport::new(stub_data)));

        // Reuse `fs` (built above, pointing at the same tmp dir where we seeded
        // tag-store and blobs) for the manager's stores.
        let local_index = ocx_index::LocalIndex::new(ocx_index::LocalConfig {
            index_store: ocx_index::IndexStore::machine_local(tag_store),
        });
        let index = ocx_index::Index::from_chained(local_index, vec![], ocx_index::ChainMode::Offline);
        let manager =
            PackageManager::new(fs, index, Some(stub_client), "localhost:5000").with_patches(Some(patch_config));

        // ── Run sync ──
        let report = manager
            .sync_patches(&[ocx_oci::Platform::any()])
            .await
            .expect("sync_patches with stub registry must not fail");

        // POST-IMPLEMENTATION: descriptors_updated must be >= 1 because the
        // upstream served a new manifest digest for the global descriptor.
        assert!(
            report.descriptors_updated >= 1,
            "sync_patches must report descriptors_updated >= 1 when the upstream descriptor advanced; \
             got descriptors_updated = {}, \
             old_manifest_digest = {old_manifest_digest}, \
             new_manifest_digest = {new_manifest_digest}",
            report.descriptors_updated,
        );
    }

    // ── D1: tag-store updated to new digest after sync ──

    /// After `sync_patches` discovers a new descriptor, the tag-store must hold
    /// the NEW manifest digest (not the old one).
    ///
    /// Traces: Phase 5C D1 — tag-store advance after sync.
    #[tokio::test(flavor = "multi_thread")]
    async fn tdd_d1_tag_store_updated_to_new_digest_after_sync() {
        let tmp = TempDir::new().unwrap();
        let patch_config = test_patch_config();

        let fs = ocx_store::file_structure::FileStructure::with_root(tmp.path().to_path_buf());
        let blob_store = &fs.blobs;
        let tag_store = &fs;

        // OCI artifact manifests require a `config` field for valid serde deserialization.
        // We use the standard empty-artifact config descriptor per OCI Image Spec v1.1.
        let empty_config_descriptor = serde_json::json!({
            "mediaType": "application/vnd.oci.empty.v1+json",
            "digest": "sha256:44136fa355ba77b9ad7b35f2cca4bb730ad02e2e8dc7f2af7a1b3e7c0ef5c6a7",
            "size": 2
        });

        // ── Build OLD descriptor ──
        let old_layer_json = serde_json::json!({"version": 1, "rules": []}).to_string();
        let old_layer_bytes = old_layer_json.as_bytes();
        let old_layer_digest = ocx_oci::Algorithm::Sha256.hash(old_layer_bytes);
        let old_manifest_json = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "artifactType": "application/vnd.sh.ocx.patch.v1",
            "config": empty_config_descriptor,
            "layers": [{"mediaType": "application/vnd.sh.ocx.patch.descriptor.v1+json",
                "digest": old_layer_digest.to_string(), "size": old_layer_bytes.len()}]
        })
        .to_string();
        let old_manifest_bytes = old_manifest_json.as_bytes();
        let old_manifest_digest = ocx_oci::Algorithm::Sha256.hash(old_manifest_bytes);
        blob_store
            .write_blob("patches.corp.com", &old_manifest_digest, old_manifest_bytes)
            .await
            .unwrap();
        blob_store
            .write_blob("patches.corp.com", &old_layer_digest, old_layer_bytes)
            .await
            .unwrap();

        let global_id = crate::tasks::patch_discovery::global_descriptor_id(&patch_config);
        let global_tags_path = tag_store.patch_descriptor_path(&global_id);
        crate::tasks::patch_discovery::PatchTagMap::write_has_descriptor(
            &global_tags_path,
            &old_manifest_digest.to_string(),
        )
        .await
        .unwrap();

        // ── Build NEW descriptor ──
        let new_layer_json = serde_json::json!({"version": 1, "rules": [{"match": "*", "packages": []}]}).to_string();
        let new_layer_bytes = new_layer_json.as_bytes();
        let new_layer_digest = ocx_oci::Algorithm::Sha256.hash(new_layer_bytes);
        let new_manifest_json = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "artifactType": "application/vnd.sh.ocx.patch.v1",
            "config": empty_config_descriptor,
            "layers": [{"mediaType": "application/vnd.sh.ocx.patch.descriptor.v1+json",
                "digest": new_layer_digest.to_string(), "size": new_layer_bytes.len()}]
        })
        .to_string();
        let new_manifest_bytes = new_manifest_json.as_bytes();
        let new_manifest_digest = ocx_oci::Algorithm::Sha256.hash(new_manifest_bytes);
        assert_ne!(
            old_manifest_digest, new_manifest_digest,
            "setup: digests must differ to test advance"
        );

        // ── Seed StubTransport with the new manifest + layer ──
        use ocx_oci::Client;
        use ocx_oci::client::test_transport::{StubTransport, StubTransportData};
        let stub_data = StubTransportData::new();
        {
            let mut inner = stub_data.write();
            let manifest_pair = (new_manifest_bytes.to_vec(), new_manifest_digest.to_string());
            // Global descriptor lives at the reserved `global` repository.
            inner
                .manifests
                .insert("patches.corp.com/global:__ocx.patch".to_string(), manifest_pair);
            inner
                .blobs
                .insert(new_layer_digest.to_string(), new_layer_bytes.to_vec());
        }
        let stub_client = Client::with_transport(Box::new(StubTransport::new(stub_data)));
        // Reuse `fs` (built above, pointing at the same tmp dir where we seeded
        // tag-store and blobs) for the manager's stores.
        let local_index = ocx_index::LocalIndex::new(ocx_index::LocalConfig {
            index_store: ocx_index::IndexStore::machine_local(tag_store),
        });
        let index = ocx_index::Index::from_chained(local_index, vec![], ocx_index::ChainMode::Offline);
        let manager = PackageManager::new(fs, index, Some(stub_client), "localhost:5000")
            .with_patches(Some(patch_config.clone()));

        // ── Run sync ──
        let _report = manager
            .sync_patches(&[ocx_oci::Platform::any()])
            .await
            .expect("sync_patches must not fail");

        // After sync, the tag-store entry must hold the NEW manifest digest.
        let state_after = crate::tasks::patch_discovery::PatchTagMap::read(&global_tags_path)
            .await
            .expect("PatchTagMap::read must succeed");
        match state_after {
            crate::tasks::patch_discovery::PatchDiscoveryState::LookedHasDescriptor { manifest_digest } => {
                assert_eq!(
                    manifest_digest,
                    new_manifest_digest.to_string(),
                    "after sync with an advanced descriptor, tag-store must hold the NEW manifest digest"
                );
            }
            other => {
                panic!("tag-store state after sync must be LookedHasDescriptor with new digest; got: {other:?}");
            }
        }
    }

    // ── T1 (plan_patch_review_fixes): deferred re-sync advance ─────────────────

    /// T1 REGRESSION (hybrid commit — deferred leg): a RE-SYNC descriptor advance
    /// must be deferred with respect to its required companions. When the upstream
    /// serves a NEW descriptor digest whose REQUIRED companion cannot be installed
    /// (a transient failure — the companion is unresolvable), the tag-store MUST
    /// stay pinned at the PRIOR (v1) descriptor digest. Advancing the recorded
    /// digest before the required companion is in place strands the base: the
    /// tag-store then points at a v2 descriptor whose companion is missing, and a
    /// later OFFLINE compose cannot fall back to the previously-installed v1
    /// companion.
    ///
    /// Setup mirrors `tdd_d1_tag_store_updated_to_new_digest_after_sync`, but the
    /// NEW descriptor references a `required: true` companion that the manager
    /// cannot install (its identifier resolves through the manager's Offline index,
    /// which has no sources, so `pull` fails). The required-companion failure makes
    /// the sync fail closed (F-A); the invariant under test is that the tag-store
    /// digest is unchanged after that failure.
    ///
    /// The prior state here is `LookedHasDescriptor` (v1), so the hybrid commit
    /// strategy DEFERS the advance: `fetch_and_persist_descriptor` records the v2
    /// pointer in the pending accumulator and `discover_and_install_patches_with_mode`
    /// returns on `RequiredCompanionFailed` before the deferred commit loop runs,
    /// leaving the tag-store at v1. (A first-discovery `NeverLooked` source commits
    /// eagerly instead — that leg is covered by
    /// `a3_first_discovery_required_failure_advances_tag_and_compose_fails_closed`.)
    ///
    /// Traces: plan_patch_review_fixes T1 — deferred re-sync advance.
    #[tokio::test(flavor = "multi_thread")]
    async fn tdd_t1_required_companion_failure_does_not_advance_descriptor_digest() {
        let tmp = TempDir::new().unwrap();
        // test_patch_config() has required = true (tier default), so an unmarked
        // companion is required; we also mark the rule required to be explicit.
        let patch_config = test_patch_config();

        let fs = ocx_store::file_structure::FileStructure::with_root(tmp.path().to_path_buf());
        let blob_store = &fs.blobs;
        let tag_store = &fs;

        // OCI artifact manifests require a `config` field for valid serde deserialization.
        let empty_config_descriptor = serde_json::json!({
            "mediaType": "application/vnd.oci.empty.v1+json",
            "digest": "sha256:44136fa355ba77b9ad7b35f2cca4bb730ad02e2e8dc7f2af7a1b3e7c0ef5c6a7",
            "size": 2
        });

        // ── OLD descriptor: the prior recorded state (v1), with NO companions. ──
        // It stands in for the descriptor whose already-installed companion an
        // offline compose still depends on; the regression is that its recorded
        // digest must survive a failed advance.
        let old_layer_json = serde_json::json!({"version": 1, "rules": [{"match": "*", "packages": []}]}).to_string();
        let old_layer_bytes = old_layer_json.as_bytes();
        let old_layer_digest = ocx_oci::Algorithm::Sha256.hash(old_layer_bytes);
        let old_manifest_json = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "artifactType": "application/vnd.sh.ocx.patch.v1",
            "config": empty_config_descriptor,
            "layers": [{"mediaType": "application/vnd.sh.ocx.patch.descriptor.v1+json",
                "digest": old_layer_digest.to_string(), "size": old_layer_bytes.len()}]
        })
        .to_string();
        let old_manifest_bytes = old_manifest_json.as_bytes();
        let old_manifest_digest = ocx_oci::Algorithm::Sha256.hash(old_manifest_bytes);
        blob_store
            .write_blob("patches.corp.com", &old_manifest_digest, old_manifest_bytes)
            .await
            .unwrap();
        blob_store
            .write_blob("patches.corp.com", &old_layer_digest, old_layer_bytes)
            .await
            .unwrap();

        let global_id = crate::tasks::patch_discovery::global_descriptor_id(&patch_config);
        let global_tags_path = tag_store.patch_descriptor_path(&global_id);
        crate::tasks::patch_discovery::PatchTagMap::write_has_descriptor(
            &global_tags_path,
            &old_manifest_digest.to_string(),
        )
        .await
        .unwrap();

        // ── NEW descriptor (v2): references a REQUIRED companion the manager
        //    cannot install. The companion package is deliberately NOT seeded
        //    anywhere, so `pull` fails and the required companion fails closed. ──
        let new_layer_json = serde_json::json!({
            "version": 1,
            "rules": [{
                "match": "*",
                "packages": ["patches.corp.com/ca-certs:latest"],
                "required": true
            }]
        })
        .to_string();
        let new_layer_bytes = new_layer_json.as_bytes();
        let new_layer_digest = ocx_oci::Algorithm::Sha256.hash(new_layer_bytes);
        let new_manifest_json = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "artifactType": "application/vnd.sh.ocx.patch.v1",
            "config": empty_config_descriptor,
            "layers": [{"mediaType": "application/vnd.sh.ocx.patch.descriptor.v1+json",
                "digest": new_layer_digest.to_string(), "size": new_layer_bytes.len()}]
        })
        .to_string();
        let new_manifest_bytes = new_manifest_json.as_bytes();
        let new_manifest_digest = ocx_oci::Algorithm::Sha256.hash(new_manifest_bytes);
        assert_ne!(
            old_manifest_digest, new_manifest_digest,
            "setup: old and new descriptor digests must differ for the advance to be meaningful"
        );

        // ── Stub serves the NEW descriptor manifest + layer for the global id,
        //    but NOT the companion package → the required companion install fails. ──
        use ocx_oci::Client;
        use ocx_oci::client::test_transport::{StubTransport, StubTransportData};
        let stub_data = StubTransportData::new();
        {
            let mut inner = stub_data.write();
            inner.manifests.insert(
                "patches.corp.com/global:__ocx.patch".to_string(),
                (new_manifest_bytes.to_vec(), new_manifest_digest.to_string()),
            );
            inner
                .blobs
                .insert(new_layer_digest.to_string(), new_layer_bytes.to_vec());
            // Intentionally NO manifest for `patches.corp.com/ca-certs:latest`.
        }
        let stub_client = Client::with_transport(Box::new(StubTransport::new(stub_data)));

        let local_index = ocx_index::LocalIndex::new(ocx_index::LocalConfig {
            index_store: ocx_index::IndexStore::machine_local(tag_store),
        });
        let index = ocx_index::Index::from_chained(local_index, vec![], ocx_index::ChainMode::Offline);
        let manager = PackageManager::new(fs, index, Some(stub_client), "localhost:5000")
            .with_patches(Some(patch_config.clone()));

        // ── Run sync: the required companion fails, so sync fails closed (F-A). ──
        let result = manager.sync_patches(&[ocx_oci::Platform::any()]).await;
        assert!(
            result.is_err(),
            "a required-companion install failure during sync must fail closed (F-A); got Ok: {result:?}"
        );

        // ── Regression assertion: the tag-store must STILL hold the OLD digest. ──
        // The re-sync advance is deferred, so the required-companion failure returns
        // before the deferred commit loop and the recorded digest stays at v1.
        let state_after = crate::tasks::patch_discovery::PatchTagMap::read(&global_tags_path)
            .await
            .expect("PatchTagMap::read must succeed");
        match state_after {
            crate::tasks::patch_discovery::PatchDiscoveryState::LookedHasDescriptor { manifest_digest } => {
                assert_eq!(
                    manifest_digest,
                    old_manifest_digest.to_string(),
                    "T1: after a failed required-companion advance, the tag-store must stay pinned at the \
                     PRIOR (v1) descriptor digest — the advance to {new_manifest_digest} must be rolled back \
                     so an offline compose can still use the previously-installed companion"
                );
            }
            other => {
                panic!(
                    "T1: tag-store must remain LookedHasDescriptor(old digest) after a failed advance; got: {other:?}"
                );
            }
        }
    }

    // ── A1 hybrid-commit helpers (shared by A3/A4/A5) ─────────────────────────

    /// Build an OCI artifact manifest + descriptor layer from a `rules` JSON array.
    ///
    /// Returns `(manifest_bytes, manifest_digest, layer_bytes, layer_digest)`.
    /// Mirrors the inline descriptor construction used by the D1/T1 tests but
    /// keeps the three hybrid-commit regression tests below DRY.
    fn build_descriptor_artifact(rules: serde_json::Value) -> (Vec<u8>, ocx_oci::Digest, Vec<u8>, ocx_oci::Digest) {
        // OCI artifact manifests require a `config` field for valid deserialization.
        let empty_config = serde_json::json!({
            "mediaType": "application/vnd.oci.empty.v1+json",
            "digest": "sha256:44136fa355ba77b9ad7b35f2cca4bb730ad02e2e8dc7f2af7a1b3e7c0ef5c6a7",
            "size": 2
        });
        let layer_json = serde_json::json!({ "version": 1, "rules": rules }).to_string();
        let layer_bytes = layer_json.into_bytes();
        let layer_digest = ocx_oci::Algorithm::Sha256.hash(&layer_bytes);
        let manifest_json = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "artifactType": "application/vnd.sh.ocx.patch.v1",
            "config": empty_config,
            "layers": [{
                "mediaType": "application/vnd.sh.ocx.patch.descriptor.v1+json",
                "digest": layer_digest.to_string(),
                "size": layer_bytes.len()
            }]
        })
        .to_string();
        let manifest_bytes = manifest_json.into_bytes();
        let manifest_digest = ocx_oci::Algorithm::Sha256.hash(&manifest_bytes);
        (manifest_bytes, manifest_digest, layer_bytes, layer_digest)
    }

    /// Seed a minimal root package into `manager`'s package store and return an
    /// `Arc<InstallInfo>` for it — always admitted by `compose` (roots are always
    /// in the admitted set), so a global `"*"` rule matches it.
    fn seed_root_install_info(
        manager: &PackageManager,
        repo: &str,
    ) -> std::sync::Arc<ocx_package::install_info::InstallInfo> {
        use ocx_package::resolved_package::ResolvedPackage;
        let store = manager.file_structure().packages.clone();
        let digest = ocx_oci::Algorithm::Sha256.hash(repo.as_bytes());
        let pinned = ocx_oci::PinnedPackageRef::try_from(
            ocx_oci::PackageRef::new_registry(repo, "ocx.sh").clone_with_digest(digest),
        )
        .expect("pinned identifier must build");
        let pkg_path = store.path(&pinned);
        std::fs::create_dir_all(pkg_path.join("content")).unwrap();
        let meta = serde_json::json!({ "type": "bundle", "version": 1 });
        std::fs::write(pkg_path.join("metadata.json"), meta.to_string()).unwrap();
        std::fs::write(
            pkg_path.join("resolve.json"),
            serde_json::to_string(&ResolvedPackage::new()).unwrap(),
        )
        .unwrap();
        let metadata: ocx_package::metadata::Metadata =
            serde_json::from_str(&std::fs::read_to_string(pkg_path.join("metadata.json")).unwrap()).unwrap();
        std::sync::Arc::new(ocx_package::install_info::InstallInfo::new(
            pinned,
            metadata,
            ResolvedPackage::new(),
            ocx_store::file_structure::PackageDir { dir: pkg_path },
        ))
    }

    // ── A3: first-discovery required failure advances the tag EAGERLY ─────────

    /// A3 REGRESSION (hybrid commit — eager leg): a FIRST discovery (prior
    /// `NeverLooked`) whose REQUIRED companion cannot be installed must EAGERLY
    /// advance the tag-store to `LookedHasDescriptor`, so a later offline compose
    /// FAILS CLOSED on the missing required companion instead of silently skipping
    /// it (fail-open).
    ///
    /// Under the round-1 pure-deferral strategy this regressed: `NeverLooked` stayed
    /// `NeverLooked` on the required failure, so an offline compose found no recorded
    /// descriptor and produced a partial (fail-OPEN) environment. The hybrid strategy
    /// commits the first-discovery pointer eagerly — there is no prior good digest to
    /// preserve, and the recorded descriptor keeps compose fail-closed.
    ///
    /// Traces: plan_patch_review_fixes A1 hybrid commit — eager leg; C7 fail-closed.
    #[tokio::test(flavor = "multi_thread")]
    async fn a3_first_discovery_required_failure_advances_tag_and_compose_fails_closed() {
        let tmp = TempDir::new().unwrap();
        let patch_config = test_patch_config(); // required = true at tier level.
        let fs = ocx_store::file_structure::FileStructure::with_root(tmp.path().to_path_buf());
        let tag_store = &fs;

        let global_id = global_descriptor_id(&patch_config);
        let global_tags_path = tag_store.patch_descriptor_path(&global_id);
        // Prior state: NeverLooked — the tag-store file does not exist yet.
        assert!(
            !global_tags_path.exists(),
            "setup: global tag-store must be NeverLooked (absent) before first discovery"
        );

        // Stub serves a global descriptor whose "*" rule references a REQUIRED
        // companion that is NOT published anywhere → the companion install fails.
        let (manifest_bytes, manifest_digest, layer_bytes, layer_digest) = build_descriptor_artifact(
            serde_json::json!([{ "match": "*", "packages": ["patches.corp.com/ca-certs:latest"], "required": true }]),
        );

        use ocx_oci::Client;
        use ocx_oci::client::test_transport::{StubTransport, StubTransportData};
        let stub_data = StubTransportData::new();
        {
            let mut inner = stub_data.write();
            inner.manifests.insert(
                global_id.to_string(),
                (manifest_bytes.clone(), manifest_digest.to_string()),
            );
            inner.blobs.insert(layer_digest.to_string(), layer_bytes.clone());
            // Intentionally NO manifest for patches.corp.com/ca-certs:latest.
        }
        let stub_client = Client::with_transport(Box::new(StubTransport::new(stub_data)));
        let local_index = ocx_index::LocalIndex::new(ocx_index::LocalConfig {
            index_store: ocx_index::IndexStore::machine_local(tag_store),
        });
        let index = ocx_index::Index::from_chained(local_index, vec![], ocx_index::ChainMode::Offline);
        let manager = PackageManager::new(fs.clone(), index, Some(stub_client), "localhost:5000")
            .with_patches(Some(patch_config.clone()));

        // Zero installed bases → sync runs the GlobalOnly first-discovery pass.
        let result = manager.sync_patches(&[ocx_oci::Platform::any()]).await;
        assert!(
            result.is_err(),
            "A3: a first-discovery required-companion failure must fail closed; got {result:?}"
        );

        // ── A3 primary: the tag ADVANCED to LookedHasDescriptor (eager commit). ──
        let state_after = PatchTagMap::read(&global_tags_path).await.expect("read must succeed");
        match &state_after {
            PatchDiscoveryState::LookedHasDescriptor {
                manifest_digest: recorded,
            } => {
                assert_eq!(
                    recorded,
                    &manifest_digest.to_string(),
                    "A3: first discovery must EAGERLY commit the descriptor digest so offline compose fails closed"
                );
            }
            other => panic!(
                "A3: tag must advance to LookedHasDescriptor after the eager first-discovery commit; got {other:?}"
            ),
        }

        // ── A3 secondary: a later OFFLINE compose fails closed (not a silent skip). ──
        // The eager fetch already persisted the descriptor blob to CAS, so an offline
        // manager over the SAME FileStructure loads it, finds the required companion
        // missing, and fails closed with RequiredCompanionFailed.
        let offline_manager = make_offline_manager(tmp.path()).with_patches(Some(patch_config));
        let root = seed_root_install_info(&offline_manager, "rootpkg");
        let compose_result = offline_manager
            .resolve_env(
                &[root],
                false,
                crate::tasks::resolve::EnvScope::package_tier(),
                &ocx_oci::Platform::current().unwrap_or_else(ocx_oci::Platform::any),
            )
            .await;
        assert!(
            compose_result.is_err(),
            "A3: offline compose must fail closed on the recorded required companion (not silently skip); got Ok"
        );
        let err = format!("{:?}", compose_result.unwrap_err());
        assert!(
            err.contains("RequiredCompanionFailed") || err.contains("required-companion"),
            "A3: offline compose failure must reference the required companion; got {err}"
        );
    }

    // ── A4: mixed re-sync — required failure keeps the digest at v1 ───────────

    /// A4 (hybrid commit — deferred leg, mixed descriptor): a RE-SYNC over an
    /// existing `LookedHasDescriptor` (v1) whose v2 descriptor carries BOTH an
    /// optional and a required companion must keep the recorded digest pinned at
    /// v1 when the required companion fails — the optional companion is handled
    /// (fail-open, before the required abort) without committing the deferred
    /// advance. The deferred commit is a single post-loop step, so no
    /// earlier-in-loop companion handling can advance the pointer early.
    ///
    /// The optional companion is exercised via the fail-open (warn + continue)
    /// path: a genuinely-successful companion install requires the full `pull`
    /// manifest chain, which has no lightweight unit harness — the acceptance
    /// suite covers real installs. What A4 pins is that an optional companion
    /// handled before a required failure does NOT commit the re-sync advance.
    ///
    /// Traces: plan_patch_review_fixes A1 hybrid commit — deferred leg (mixed).
    #[tokio::test(flavor = "multi_thread")]
    async fn a4_resync_required_failure_keeps_digest_v1_with_mixed_companions() {
        let tmp = TempDir::new().unwrap();
        // Tier required=false so the per-rule `required` flags decide companion posture.
        let patch_config = ResolvedPatchConfig {
            system_required: false,
            no_patches: std::collections::BTreeSet::new(),
            registry: "patches.corp.com".to_string(),
            path_template: "{registry}/{repository}".to_string(),
            required: false,
        };
        let fs = ocx_store::file_structure::FileStructure::with_root(tmp.path().to_path_buf());
        let blob_store = &fs.blobs;
        let tag_store = &fs;

        // ── v1: the prior recorded descriptor (unique content, no companions). ──
        let (v1_manifest_bytes, v1_manifest_digest, v1_layer_bytes, v1_layer_digest) =
            build_descriptor_artifact(serde_json::json!([{ "match": "v1-marker", "packages": [] }]));
        blob_store
            .write_blob("patches.corp.com", &v1_manifest_digest, &v1_manifest_bytes)
            .await
            .unwrap();
        blob_store
            .write_blob("patches.corp.com", &v1_layer_digest, &v1_layer_bytes)
            .await
            .unwrap();
        let global_id = global_descriptor_id(&patch_config);
        let global_tags_path = tag_store.patch_descriptor_path(&global_id);
        PatchTagMap::write_has_descriptor(&global_tags_path, &v1_manifest_digest.to_string())
            .await
            .unwrap();

        // ── v2: an optional companion (required=false) + a required companion
        //    (required=true), neither published → optional warns, required fails. ──
        let (v2_manifest_bytes, v2_manifest_digest, v2_layer_bytes, v2_layer_digest) =
            build_descriptor_artifact(serde_json::json!([
                { "match": "*", "packages": ["patches.corp.com/opt-tool:latest"], "required": false },
                { "match": "*", "packages": ["patches.corp.com/req-ca:latest"], "required": true }
            ]));
        assert_ne!(
            v1_manifest_digest, v2_manifest_digest,
            "setup: v1 and v2 descriptor digests must differ for the advance to be meaningful"
        );

        use ocx_oci::Client;
        use ocx_oci::client::test_transport::{StubTransport, StubTransportData};
        let stub_data = StubTransportData::new();
        {
            let mut inner = stub_data.write();
            inner.manifests.insert(
                global_id.to_string(),
                (v2_manifest_bytes.clone(), v2_manifest_digest.to_string()),
            );
            inner.blobs.insert(v2_layer_digest.to_string(), v2_layer_bytes.clone());
            // Intentionally NO companion manifests (opt-tool + req-ca absent).
        }
        let stub_client = Client::with_transport(Box::new(StubTransport::new(stub_data)));
        let local_index = ocx_index::LocalIndex::new(ocx_index::LocalConfig {
            index_store: ocx_index::IndexStore::machine_local(tag_store),
        });
        let index = ocx_index::Index::from_chained(local_index, vec![], ocx_index::ChainMode::Offline);
        let manager =
            PackageManager::new(fs, index, Some(stub_client), "localhost:5000").with_patches(Some(patch_config));

        // Zero installed bases → GlobalOnly re-sync pass; required companion fails closed.
        let result = manager.sync_patches(&[ocx_oci::Platform::any()]).await;
        assert!(
            result.is_err(),
            "A4: a required-companion failure during re-sync must fail closed; got {result:?}"
        );

        // ── A4 assertion: the recorded digest is STILL v1 (deferred advance rolled back). ──
        let state_after = PatchTagMap::read(&global_tags_path).await.expect("read must succeed");
        match state_after {
            PatchDiscoveryState::LookedHasDescriptor { manifest_digest } => {
                assert_eq!(
                    manifest_digest,
                    v1_manifest_digest.to_string(),
                    "A4: after a failed required advance the tag must stay pinned at v1 (deferred advance to v2 rolled back)"
                );
            }
            other => {
                panic!("A4: tag must remain LookedHasDescriptor(v1) after the failed re-sync advance; got {other:?}")
            }
        }
    }

    // ── A5: both-scope re-sync — required failure rolls back BOTH advances ────

    /// A5 (hybrid commit — deferred leg, two sources): a `Both`-scope re-sync over
    /// an installed base re-fetches the GLOBAL and the PACKAGE-SPECIFIC descriptors
    /// (both prior `LookedHasDescriptor` v1) into the SAME deferred accumulator. A
    /// required-companion failure returns before the deferred commit loop, so BOTH
    /// sources stay pinned at their v1 digests — only the deferred re-sync advances
    /// roll back, never a partial commit.
    ///
    /// Traces: plan_patch_review_fixes A1 hybrid commit — deferred leg (both-scope,
    /// shared accumulator).
    #[tokio::test(flavor = "multi_thread")]
    async fn a5_both_scope_resync_required_failure_rolls_back_both_advances() {
        let tmp = TempDir::new().unwrap();
        let patch_config = test_patch_config(); // required = true at tier level.
        let fs = ocx_store::file_structure::FileStructure::with_root(tmp.path().to_path_buf());
        let blob_store = &fs.blobs;
        let tag_store = &fs;

        // Seed one installed base candidate → sync uses the Both scope for it.
        let symlink_store = ocx_store::file_structure::SymlinkStore::new(tmp.path().join("symlinks"));
        let base_id = ocx_oci::PackageRef::new_registry("cmake", "ocx.sh").clone_with_tag("3.28");
        let candidate_path = symlink_store.candidate(&base_id);
        tokio::fs::create_dir_all(candidate_path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&candidate_path, b"").await.unwrap();

        // ── Global v1 + package-specific v1 (distinct content → distinct digests). ──
        let (g1_manifest_bytes, g1_manifest_digest, g1_layer_bytes, g1_layer_digest) =
            build_descriptor_artifact(serde_json::json!([{ "match": "global-v1", "packages": [] }]));
        let (p1_manifest_bytes, p1_manifest_digest, p1_layer_bytes, p1_layer_digest) =
            build_descriptor_artifact(serde_json::json!([{ "match": "pkg-v1", "packages": [] }]));
        for (mb, md, lb, ld) in [
            (
                &g1_manifest_bytes,
                &g1_manifest_digest,
                &g1_layer_bytes,
                &g1_layer_digest,
            ),
            (
                &p1_manifest_bytes,
                &p1_manifest_digest,
                &p1_layer_bytes,
                &p1_layer_digest,
            ),
        ] {
            blob_store.write_blob("patches.corp.com", md, mb).await.unwrap();
            blob_store.write_blob("patches.corp.com", ld, lb).await.unwrap();
        }
        let global_id = global_descriptor_id(&patch_config);
        let global_tags_path = tag_store.patch_descriptor_path(&global_id);
        let pkg_id = patch_descriptor_id(&patch_config, &base_id);
        let pkg_tags_path = tag_store.patch_descriptor_path(&pkg_id);
        PatchTagMap::write_has_descriptor(&global_tags_path, &g1_manifest_digest.to_string())
            .await
            .unwrap();
        PatchTagMap::write_has_descriptor(&pkg_tags_path, &p1_manifest_digest.to_string())
            .await
            .unwrap();

        // ── Global v2: "*" rule → REQUIRED companion (absent) → fails closed.
        //    Package-specific v2: distinct content, no companions. ──
        let (g2_manifest_bytes, g2_manifest_digest, g2_layer_bytes, g2_layer_digest) = build_descriptor_artifact(
            serde_json::json!([{ "match": "*", "packages": ["patches.corp.com/req-ca:latest"], "required": true }]),
        );
        let (p2_manifest_bytes, p2_manifest_digest, p2_layer_bytes, p2_layer_digest) =
            build_descriptor_artifact(serde_json::json!([{ "match": "pkg-v2", "packages": [] }]));
        assert_ne!(
            g1_manifest_digest, g2_manifest_digest,
            "setup: global v1/v2 digests must differ"
        );
        assert_ne!(
            p1_manifest_digest, p2_manifest_digest,
            "setup: pkg v1/v2 digests must differ"
        );

        use ocx_oci::Client;
        use ocx_oci::client::test_transport::{StubTransport, StubTransportData};
        let stub_data = StubTransportData::new();
        {
            let mut inner = stub_data.write();
            inner.manifests.insert(
                global_id.to_string(),
                (g2_manifest_bytes.clone(), g2_manifest_digest.to_string()),
            );
            inner.blobs.insert(g2_layer_digest.to_string(), g2_layer_bytes.clone());
            inner.manifests.insert(
                pkg_id.to_string(),
                (p2_manifest_bytes.clone(), p2_manifest_digest.to_string()),
            );
            inner.blobs.insert(p2_layer_digest.to_string(), p2_layer_bytes.clone());
            // Intentionally NO manifest for patches.corp.com/req-ca:latest.
        }
        let stub_client = Client::with_transport(Box::new(StubTransport::new(stub_data)));
        let local_index = ocx_index::LocalIndex::new(ocx_index::LocalConfig {
            index_store: ocx_index::IndexStore::machine_local(tag_store),
        });
        let index = ocx_index::Index::from_chained(local_index, vec![], ocx_index::ChainMode::Offline);
        let manager =
            PackageManager::new(fs, index, Some(stub_client), "localhost:5000").with_patches(Some(patch_config));

        let result = manager.sync_patches(&[ocx_oci::Platform::any()]).await;
        assert!(
            result.is_err(),
            "A5: a required-companion failure during a both-scope re-sync must fail closed; got {result:?}"
        );

        // ── A5 assertion: BOTH sources stay pinned at their v1 digests. ──
        let global_after = PatchTagMap::read(&global_tags_path).await.expect("read must succeed");
        match global_after {
            PatchDiscoveryState::LookedHasDescriptor { manifest_digest } => assert_eq!(
                manifest_digest,
                g1_manifest_digest.to_string(),
                "A5: the global descriptor must stay at v1 after the required-companion failure"
            ),
            other => panic!("A5: global tag must remain LookedHasDescriptor(v1); got {other:?}"),
        }
        let pkg_after = PatchTagMap::read(&pkg_tags_path).await.expect("read must succeed");
        match pkg_after {
            PatchDiscoveryState::LookedHasDescriptor { manifest_digest } => assert_eq!(
                manifest_digest,
                p1_manifest_digest.to_string(),
                "A5: the package-specific descriptor must stay at v1 (shared accumulator rolls back both)"
            ),
            other => panic!("A5: package-specific tag must remain LookedHasDescriptor(v1); got {other:?}"),
        }
    }

    // ── A6: corrupt prior descriptor heals EAGERLY, compose fails closed ──────

    /// A6 REGRESSION (corrupt-cache heal — eager leg): a lazy discovery over a prior
    /// `LookedHasDescriptor` whose recorded digest is valid in FORMAT but whose CAS
    /// blob is MISSING/corrupt must re-fetch and advance the tag EAGERLY. There is no
    /// last-known-good blob to preserve, so the re-fetch commits immediately and the
    /// tag points at the freshly-persisted valid manifest digest. A later offline
    /// compose then FAILS CLOSED on the descriptor's `required: true` companion
    /// instead of silently skipping it.
    ///
    /// This exercises the CAS-load-fail corrupt-cache site: under the round-1 pure
    /// deferral strategy that site committed the advance with
    /// `DescriptorCommit::Deferred`, so a required-companion failure rolled the
    /// pointer BACK — onto the corrupt/missing blob. An offline compose then found no
    /// loadable descriptor and produced a partial (fail-OPEN) environment. The eager
    /// commit heals the corrupt pointer to the valid blob and keeps compose fail-closed.
    ///
    /// The tier is `required: false`; the descriptor rule carries an explicit
    /// `required: true` companion — the exact non-required-tier / rule-required-companion
    /// combination the corrupt-cache flip protects.
    ///
    /// Traces: plan_patch_review_fixes A1 hybrid commit — corrupt-cache heal (eager leg).
    #[tokio::test(flavor = "multi_thread")]
    async fn a6_corrupt_prior_descriptor_heals_eagerly_and_compose_fails_closed() {
        let tmp = TempDir::new().unwrap();
        // Tier required=false so the per-rule `required: true` flag decides posture.
        let patch_config = ResolvedPatchConfig {
            system_required: false,
            no_patches: std::collections::BTreeSet::new(),
            registry: "patches.corp.com".to_string(),
            path_template: "{registry}/{repository}".to_string(),
            required: false,
        };
        let fs = ocx_store::file_structure::FileStructure::with_root(tmp.path().to_path_buf());
        let tag_store = &fs;

        // ── Prior state: LookedHasDescriptor with a valid-FORMAT digest whose CAS
        //    blob is deliberately NEVER written. `Digest::try_from` succeeds, so lazy
        //    discovery reaches the CAS-load-fail site (not the invalid-digest site). ──
        let corrupt_digest = ocx_oci::Algorithm::Sha256.hash(b"corrupt-prior-descriptor");
        let global_id = global_descriptor_id(&patch_config);
        let global_tags_path = tag_store.patch_descriptor_path(&global_id);
        PatchTagMap::write_has_descriptor(&global_tags_path, &corrupt_digest.to_string())
            .await
            .unwrap();

        // ── NEW valid descriptor: "*" rule → REQUIRED companion (not published). ──
        let (manifest_bytes, manifest_digest, layer_bytes, layer_digest) = build_descriptor_artifact(
            serde_json::json!([{ "match": "*", "packages": ["patches.corp.com/req-ca:latest"], "required": true }]),
        );
        assert_ne!(
            corrupt_digest, manifest_digest,
            "setup: corrupt prior digest and healed digest must differ"
        );

        use ocx_oci::Client;
        use ocx_oci::client::test_transport::{StubTransport, StubTransportData};
        let stub_data = StubTransportData::new();
        {
            let mut inner = stub_data.write();
            inner.manifests.insert(
                global_id.to_string(),
                (manifest_bytes.clone(), manifest_digest.to_string()),
            );
            inner.blobs.insert(layer_digest.to_string(), layer_bytes.clone());
            // Intentionally NO manifest for patches.corp.com/req-ca:latest.
        }
        let stub_client = Client::with_transport(Box::new(StubTransport::new(stub_data)));
        let local_index = ocx_index::LocalIndex::new(ocx_index::LocalConfig {
            index_store: ocx_index::IndexStore::machine_local(tag_store),
        });
        let index = ocx_index::Index::from_chained(local_index, vec![], ocx_index::ChainMode::Offline);
        let manager = PackageManager::new(fs.clone(), index, Some(stub_client), "localhost:5000")
            .with_patches(Some(patch_config.clone()));

        // Lazy discovery (Both scope) over a real base: the global corrupt-cache heal
        // re-fetches EAGERLY; the required companion then fails → fail closed.
        let base_id = ocx_oci::PackageRef::new_registry("cmake", "ocx.sh").clone_with_tag("3.28");
        let result = manager
            .discover_and_install_patches_with_mode(
                &base_id,
                &ocx_oci::Platform::any(),
                PatchDiscoveryMode::Lazy,
                PatchDescriptorScope::Both,
            )
            .await;
        assert!(
            result.is_err(),
            "A6: a required-companion failure after the corrupt-cache heal must fail closed; got {result:?}"
        );

        // ── A6 primary: the corrupt pointer HEALED to the new valid digest (eager). ──
        let state_after = PatchTagMap::read(&global_tags_path).await.expect("read must succeed");
        match &state_after {
            PatchDiscoveryState::LookedHasDescriptor {
                manifest_digest: recorded,
            } => {
                assert_eq!(
                    recorded,
                    &manifest_digest.to_string(),
                    "A6: a corrupt prior descriptor must heal EAGERLY to the freshly-persisted valid digest \
                     (no last-known-good blob to preserve), NOT roll back onto the corrupt pointer"
                );
            }
            other => panic!(
                "A6: tag must advance to LookedHasDescriptor(new digest) after the eager corrupt-cache heal; got {other:?}"
            ),
        }

        // ── A6 secondary: a later OFFLINE compose fails closed on the required companion. ──
        // The eager heal persisted the valid descriptor blobs to CAS, so an offline
        // manager over the SAME FileStructure loads the healed descriptor, finds the
        // required companion missing, and fails closed with RequiredCompanionFailed.
        let offline_manager = make_offline_manager(tmp.path()).with_patches(Some(patch_config));
        let root = seed_root_install_info(&offline_manager, "rootpkg");
        let compose_result = offline_manager
            .resolve_env(
                &[root],
                false,
                crate::tasks::resolve::EnvScope::package_tier(),
                &ocx_oci::Platform::current().unwrap_or_else(ocx_oci::Platform::any),
            )
            .await;
        assert!(
            compose_result.is_err(),
            "A6: offline compose must fail closed on the healed required companion (not silently skip); got Ok"
        );
        let err = format!("{:?}", compose_result.unwrap_err());
        assert!(
            err.contains("RequiredCompanionFailed") || err.contains("required-companion"),
            "A6: offline compose failure must reference the required companion; got {err}"
        );
    }

    // ── Project-lock bases: the identity `ocx lock` discovered companions under ──

    /// Every `warn!` this test binary emits; a process-global logger, so each test
    /// searches for a needle unique to its own fixture and never drains the buffer.
    static CAPTURED_WARNINGS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

    struct CapturingLogger;

    impl log::Log for CapturingLogger {
        fn enabled(&self, metadata: &log::Metadata) -> bool {
            metadata.level() <= log::Level::Warn
        }
        fn log(&self, record: &log::Record) {
            if self.enabled(record.metadata()) {
                CAPTURED_WARNINGS.lock().unwrap().push(record.args().to_string());
            }
        }
        fn flush(&self) {}
    }

    fn install_warning_capture() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            log::set_boxed_logger(Box::new(CapturingLogger))
                .expect("nothing else in this test binary installs a logger");
            log::set_max_level(log::LevelFilter::Warn);
        });
    }

    fn warned_about(needle: &str) -> bool {
        CAPTURED_WARNINGS
            .lock()
            .unwrap()
            .iter()
            .any(|line| line.contains(needle))
    }

    const BASE_CONFIG: &str = "[tools]\ncmake = \"localhost:5000/cmake:3.28\"\n";
    const BASE_LEAF: &str = "sha256:aaaa0000000000000000000000000000000000000000000000000000000000bb";

    fn linux() -> ocx_oci::Platform {
        "linux/amd64".parse().expect("valid platform")
    }

    /// An `ocx.lock` pinning `cmake` at [`BASE_LEAF`], recording `declaration_hash`.
    fn base_lock(declaration_hash: &str) -> String {
        format!(
            r#"
[metadata]
lock_version = 3
declaration_hash_version = 1
declaration_hash = "{declaration_hash}"
generated_by = "ocx test"
generated_at = "2026-01-01T00:00:00Z"

[[tool]]
name = "cmake"
group = "default"
repository = "localhost:5000/cmake"

[tool.platforms]
"linux/amd64" = "{BASE_LEAF}"
"#
        )
    }

    /// `dir/ocx.toml` = [`BASE_CONFIG`] and an `ocx.lock` bound to it.
    async fn write_current_project(dir: &Path) {
        let config = ocx_project::ProjectConfig::from_toml_str(BASE_CONFIG).expect("parse ocx.toml");
        tokio::fs::write(dir.join("ocx.toml"), BASE_CONFIG).await.unwrap();
        tokio::fs::write(dir.join("ocx.lock"), base_lock(config.declaration_hash_cached()))
            .await
            .unwrap();
    }

    /// A lock bound to its `ocx.toml` yields `repo:tag@leaf`, the identifier `ocx lock` discovered.
    #[tokio::test]
    async fn project_patch_bases_carry_the_declared_tag_on_the_lock_leaf() {
        let home = TempDir::new().unwrap();
        write_current_project(home.path()).await;

        let bases = make_offline_manager(home.path())
            .project_patch_bases(&linux())
            .await
            .unwrap();

        let expected: ocx_oci::PackageRef = format!("localhost:5000/cmake:3.28@{BASE_LEAF}").parse().unwrap();
        assert_eq!(bases, vec![expected]);
    }

    /// A stale lock, or one without `ocx.toml`, yields `repo@leaf`, never the bare `repo`
    /// a matcher reads as `:latest`.
    #[tokio::test]
    async fn project_patch_bases_of_a_stale_lock_are_digest_bearing_and_untagged() {
        let stale = TempDir::new().unwrap();
        tokio::fs::write(stale.path().join("ocx.toml"), BASE_CONFIG)
            .await
            .unwrap();
        tokio::fs::write(
            stale.path().join("ocx.lock"),
            base_lock(&format!("sha256:{}", "0".repeat(64))),
        )
        .await
        .unwrap();
        let no_config = TempDir::new().unwrap();
        tokio::fs::write(
            no_config.path().join("ocx.lock"),
            base_lock(&format!("sha256:{}", "0".repeat(64))),
        )
        .await
        .unwrap();

        for home in [&stale, &no_config] {
            let bases = make_offline_manager(home.path())
                .project_patch_bases(&linux())
                .await
                .unwrap();
            let expected: ocx_oci::PackageRef = format!("localhost:5000/cmake@{BASE_LEAF}").parse().unwrap();
            assert_eq!(bases, vec![expected], "{}", home.path().display());
            assert!(bases.iter().all(|base| base.digest().is_some() && base.tag().is_none()));
        }
    }

    /// A platform the lock has no leaf for yields no base for that entry.
    #[tokio::test]
    async fn project_patch_bases_skip_an_entry_without_a_leaf_for_the_platform() {
        let home = TempDir::new().unwrap();
        write_current_project(home.path()).await;

        let bases = make_offline_manager(home.path())
            .project_patch_bases(&"darwin/arm64".parse().unwrap())
            .await
            .unwrap();

        assert!(bases.is_empty(), "{bases:?}");
    }

    /// Registers a project whose `ocx.lock` is readable and whose `ocx.toml` is a directory
    /// (a non-`NotFound` read error on every platform), beside an unreadable home lock.
    async fn project_with_unreadable_config(home: &TempDir) -> TempDir {
        tokio::fs::create_dir_all(home.path().join("ocx.lock")).await.unwrap();
        let project = TempDir::new().unwrap();
        tokio::fs::write(
            project.path().join("ocx.lock"),
            base_lock(&format!("sha256:{}", "0".repeat(64))),
        )
        .await
        .unwrap();
        tokio::fs::create_dir_all(project.path().join("ocx.toml"))
            .await
            .unwrap();
        ocx_project::ProjectRegistry::new(home.path())
            .register(project.path())
            .await
            .expect("register");
        project
    }

    /// Under `Fallback` (`patch sync`) an unreadable lock is skipped and an unreadable
    /// `ocx.toml` falls back to the lock's untagged leaves, each with a warning.
    #[tokio::test]
    async fn sync_project_locks_warn_and_skip_an_unreadable_lock_or_config() {
        install_warning_capture();
        let home = TempDir::new().unwrap();
        let project = project_with_unreadable_config(&home).await;

        let locks = load_project_locks(
            make_offline_manager(home.path()).file_structure(),
            UnreadableConfig::Fallback,
        )
        .await
        .expect("fallback never fails");

        let bases: Vec<ocx_oci::PackageRef> = locks
            .iter()
            .flat_map(|(lock, config)| lock_bases(lock, config.as_ref(), &linux()))
            .collect();
        let expected: ocx_oci::PackageRef = format!("localhost:5000/cmake@{BASE_LEAF}").parse().unwrap();
        assert_eq!(bases, vec![expected]);
        let unreadable_lock = format!("skipping unreadable lock in '{}'", home.path().display());
        assert!(warned_about(&unreadable_lock), "missing warning: {unreadable_lock}");
        let config_path = dunce::canonicalize(project.path()).unwrap().join("ocx.toml");
        let unreadable_config = format!("ignoring unreadable '{}'", config_path.display());
        assert!(warned_about(&unreadable_config), "missing warning: {unreadable_config}");
    }

    /// `patch freeze` refuses an unreadable `ocx.toml` instead of snapshotting untagged
    /// leaves, which would miss the companions that project's tagged bases discover.
    #[tokio::test]
    async fn project_patch_bases_refuse_an_unreadable_config() {
        let home = TempDir::new().unwrap();
        let project = project_with_unreadable_config(&home).await;

        let error = make_offline_manager(home.path())
            .project_patch_bases(&linux())
            .await
            .expect_err("an unreadable ocx.toml must fail the freeze");

        let crate::Error::Patch(patch_error) = &error else {
            panic!("expected a patch error, got {error:?}");
        };
        let crate::patch::PatchError::ProjectConfigUnreadable { path, .. } = patch_error.as_ref() else {
            panic!("expected ProjectConfigUnreadable, got {patch_error:?}");
        };
        assert_eq!(path, &dunce::canonicalize(project.path()).unwrap().join("ocx.toml"));
    }

    /// `patch sync` and `patch freeze` read the ledger without pruning a departed project's link.
    #[tokio::test]
    async fn project_patch_bases_never_prune_the_ledger() {
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        write_current_project(project.path()).await;
        ocx_project::ProjectRegistry::new(home.path())
            .register(project.path())
            .await
            .expect("register");
        tokio::fs::remove_file(project.path().join("ocx.lock")).await.unwrap();
        let projects_dir = home.path().join("projects");
        let links_before = std::fs::read_dir(&projects_dir).unwrap().count();

        let bases = make_offline_manager(home.path())
            .project_patch_bases(&linux())
            .await
            .unwrap();

        assert!(bases.is_empty(), "{bases:?}");
        assert_eq!(std::fs::read_dir(&projects_dir).unwrap().count(), links_before);
        assert_eq!(links_before, 1);
    }
}
