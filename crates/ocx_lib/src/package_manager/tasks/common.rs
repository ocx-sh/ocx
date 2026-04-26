// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Shared utilities for task modules.
//!
//! Free functions only — no `impl PackageManager`. Since `tasks` is a private
//! module, these helpers are invisible to external consumers.
//!
//! ## Selection-state lock order
//!
//! Selection-state mutations (the per-repo `current` symlink plus the
//! per-registry `.entrypoints-index.json`) are guarded by two layered locks.
//! **Lock order MUST be `index_lock` → `select_lock`** — the reverse risks
//! deadlock under concurrent installs that target distinct repos in the same
//! registry.
//!
//! - **`{symlinks/{registry}/.entrypoints-index.lock}`** (per registry) — held
//!   for the full read → collision check → symlink update → index write
//!   sequence inside [`wire_selection`].
//! - **`{symlinks/{registry}/{repo}}/.select.lock`** (per repo) — held for the
//!   actual symlink writes/rollback. Acquired *after* the index lock.
//!
//! `deselect` and `uninstall --deselect` follow the same order: index lock
//! first, repo `.select.lock` second.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use tokio::task::JoinSet;

use crate::{
    file_lock::FileLock,
    file_structure::{self, PackageStore},
    log, oci,
    package::{install_info::InstallInfo, metadata, resolved_package::ResolvedPackage},
    package_manager::error::{self, PackageError, PackageErrorKind},
    prelude::SerdeExt,
    reference_manager::ReferenceManager,
    utility,
};

/// Finds a package in the object store without index resolution.
///
/// The identifier must carry a digest. Returns the installed package info if
/// present, or `None` if the object is absent. Also serves as defense layer 2
/// in the concurrent pull safety model.
///
/// Metadata read from disk is validated through [`metadata::ValidMetadata`]
/// before the [`InstallInfo`] is constructed — defense in depth against stale
/// or tampered on-disk metadata that predates current validation rules.
pub async fn find_in_store(
    objects: &PackageStore,
    identifier: &oci::PinnedIdentifier,
) -> Result<Option<InstallInfo>, PackageErrorKind> {
    let pkg = file_structure::PackageDir {
        dir: objects.path(identifier),
    };
    let content = pkg.content();
    let metadata_path = pkg.metadata();
    let resolve_path = pkg.resolve();
    if utility::fs::path_exists_lossy(&content).await
        && utility::fs::path_exists_lossy(&metadata_path).await
        && utility::fs::path_exists_lossy(&resolve_path).await
    {
        let (metadata_result, resolved_result): (crate::Result<metadata::Metadata>, crate::Result<ResolvedPackage>) = tokio::join!(
            metadata::Metadata::read_json(&metadata_path),
            ResolvedPackage::read_json(&resolve_path),
        );
        let metadata = metadata_result.map_err(PackageErrorKind::Internal)?;
        let metadata = metadata::ValidMetadata::try_from(metadata)
            .map_err(PackageErrorKind::Internal)?
            .into();
        let resolved = resolved_result.map_err(PackageErrorKind::Internal)?;
        Ok(Some(InstallInfo {
            identifier: identifier.clone(),
            metadata,
            resolved,
            content,
        }))
    } else {
        Ok(None)
    }
}

/// Reconstructs the [`PinnedIdentifier`](oci::PinnedIdentifier) for a package
/// loaded through an install symlink (candidate or current).
///
/// Registry and repository come from the caller-supplied [`oci::Identifier`].
/// The digest is read from the shared package directory's `digest` file so the
/// result is the truly installed content digest, not whatever first installer
/// happened to win a cross-repo dedup race.
///
/// Tag handling depends on `kind`:
/// - [`SymlinkKind::Candidate`]: caller's tag is preserved — candidate symlinks
///   are keyed by tag, so the tag and digest always agree by construction.
/// - [`SymlinkKind::Current`]: caller's tag is stripped. `current` points at
///   whatever digest was most recently selected, which may have been installed
///   under a different tag than the caller supplied. Keeping the caller's tag
///   would produce a hybrid identifier (`pkg:old-tag@new-digest`) that never
///   existed as a real install.
pub async fn identifier_for_symlink(
    objects: &PackageStore,
    symlink_path: &Path,
    identifier: &oci::Identifier,
    kind: file_structure::SymlinkKind,
) -> Result<oci::PinnedIdentifier, crate::Error> {
    let digest_path = objects.digest_file_for_content(symlink_path)?;
    let digest = file_structure::read_digest_file(&digest_path).await?;
    let base = match kind {
        file_structure::SymlinkKind::Candidate => identifier.clone(),
        file_structure::SymlinkKind::Current => identifier.without_tag(),
    };
    Ok(oci::PinnedIdentifier::try_from(base.clone_with_digest(digest))?)
}

/// Loads metadata.json and resolve.json for an existing content path.
///
/// Uses `PackageStore::metadata_for_content` / `resolve_for_content` which
/// follow symlinks, making this safe for both direct object paths and install
/// symlinks.
///
/// Metadata is validated via [`metadata::ValidMetadata`] before being returned
/// — every on-disk metadata blob the package_manager loads goes through the
/// same validation as a publish-time blob, so consumers operate on metadata
/// that is structurally and semantically well-formed.
pub async fn load_object_data(
    objects: &PackageStore,
    content_path: &Path,
) -> Result<(metadata::Metadata, ResolvedPackage), crate::Error> {
    let metadata_path = objects.metadata_for_content(content_path)?;
    let resolve_path = objects.resolve_for_content(content_path)?;
    let (metadata_result, resolved_result): (crate::Result<metadata::Metadata>, crate::Result<ResolvedPackage>) = tokio::join!(
        metadata::Metadata::read_json(&metadata_path),
        ResolvedPackage::read_json(&resolve_path),
    );
    let metadata = metadata::ValidMetadata::try_from(metadata_result?)?.into();
    Ok((metadata, resolved_result?))
}

/// Drains a [`JoinSet`] of package tasks and collects results preserving
/// the order given by `packages`.
///
/// Tasks whose `JoinHandle` reports a panic are recorded as
/// [`PackageErrorKind::TaskPanicked`]. If any errors accumulated, they are
/// wrapped with `error_ctor` and returned as a single batch error.
pub async fn drain_package_tasks<T: 'static>(
    packages: &[oci::Identifier],
    mut tasks: JoinSet<(oci::Identifier, Result<T, PackageErrorKind>)>,
    error_ctor: fn(Vec<PackageError>) -> error::Error,
) -> Result<Vec<T>, error::Error> {
    let mut pending: HashSet<oci::Identifier> = packages.iter().cloned().collect();
    let mut results: HashMap<oci::Identifier, T> = HashMap::with_capacity(packages.len());
    let mut errors: Vec<PackageError> = Vec::new();

    while let Some(join_result) = tasks.join_next().await {
        match join_result {
            Ok((id, Ok(info))) => {
                pending.remove(&id);
                results.insert(id, info);
            }
            Ok((id, Err(kind))) => {
                pending.remove(&id);
                errors.push(PackageError::new(id, kind));
            }
            Err(e) => log::error!("Task panicked: {}", e),
        }
    }

    for id in pending {
        errors.push(PackageError::new(id, PackageErrorKind::TaskPanicked));
    }

    // Preserve input order.
    let mut infos = Vec::with_capacity(packages.len());
    for package in packages {
        if let Some(info) = results.remove(package) {
            infos.push(info);
        }
    }

    if !errors.is_empty() {
        return Err(error_ctor(errors));
    }

    Ok(infos)
}

/// Creates a [`ReferenceManager`] from a [`FileStructure`].
pub fn reference_manager(fs: &file_structure::FileStructure) -> ReferenceManager {
    ReferenceManager::new(fs.clone())
}

/// Acquires the per-repo selection lock for `package`.
///
/// The lock file lives at `{symlinks/{registry}/{repo}}/.select.lock` and
/// serializes mutations of the per-repo `current` symlink across
/// `install --select`, `deselect`, and `uninstall --deselect`. The returned
/// [`FileLock`] guard releases the lock on drop.
pub async fn acquire_select_lock(
    fs: &file_structure::FileStructure,
    package: &oci::Identifier,
) -> Result<FileLock, PackageErrorKind> {
    let lock_path = fs.symlinks.select_lock(package);
    if let Some(parent) = lock_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| PackageErrorKind::Internal(crate::error::file_error(parent, e)))?;
    }
    let file = tokio::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .await
        .map_err(|e| PackageErrorKind::Internal(crate::error::file_error(&lock_path, e)))?
        .into_std()
        .await;
    FileLock::lock_exclusive(file)
        .await
        .map_err(|e| PackageErrorKind::Internal(crate::error::file_error(&lock_path, e)))
}

/// Acquires the per-registry entry-points index lock.
///
/// Lock order: this lock MUST be acquired *before* [`acquire_select_lock`]
/// when both are needed. See module docs.
async fn acquire_index_lock(fs: &file_structure::FileStructure, registry: &str) -> Result<FileLock, PackageErrorKind> {
    let lock_path = fs.symlinks.entrypoints_index_lock(registry);
    if let Some(parent) = lock_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| PackageErrorKind::Internal(crate::error::file_error(parent, e)))?;
    }
    let file = tokio::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .await
        .map_err(|e| PackageErrorKind::Internal(crate::error::file_error(&lock_path, e)))?
        .into_std()
        .await;
    FileLock::lock_exclusive(file)
        .await
        .map_err(|e| PackageErrorKind::Internal(crate::error::file_error(&lock_path, e)))
}

/// On-disk shape of the per-registry entry-points ownership index.
///
/// File: `{symlinks/{registry_slug}}/.entrypoints-index.json`. Maps every
/// currently-selected launcher name (under this registry) to the package that
/// owns it. Replaces the prior O(N·M) directory walk with a single O(1) map
/// lookup and lets cross-repo collision detection run atomically under a
/// single registry-scoped lock.
///
/// Lifecycle: written under [`acquire_index_lock`] in [`wire_selection`],
/// updated by [`update_index_for_package`] on deselect/uninstall, removed
/// implicitly when the parent registry directory is cleaned up.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct EntrypointsIndex {
    /// Schema version — set to `1` on write. Reserved for future migrations.
    #[serde(default = "default_schema_version")]
    pub schema_version: u8,
    /// Launcher name → owning package. Sorted by name for deterministic on-disk
    /// output (uses `BTreeMap`, which serializes keys in sort order).
    #[serde(default)]
    pub entries: BTreeMap<String, IndexOwner>,
}

/// Manual `Default` so a freshly-created index pinned at the documented
/// `schema_version = 1` contract — `#[derive(Default)]` would emit `0`,
/// which the missing-file path used to persist on first write.
impl Default for EntrypointsIndex {
    fn default() -> Self {
        Self {
            schema_version: default_schema_version(),
            entries: BTreeMap::new(),
        }
    }
}

fn default_schema_version() -> u8 {
    1
}

/// One row in [`EntrypointsIndex::entries`]. Identifies the owning package by
/// its OCI registry + repository + (current) digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct IndexOwner {
    pub registry: String,
    pub repository: String,
    pub digest: String,
}

impl EntrypointsIndex {
    /// Reads the index from disk, returning a fresh empty instance when the
    /// file is absent. Any other I/O or parse failure surfaces as a hard error.
    async fn read(path: &Path) -> Result<Self, PackageErrorKind> {
        match tokio::fs::try_exists(path).await {
            Ok(true) => Self::read_json(path).await.map_err(PackageErrorKind::Internal),
            Ok(false) => Ok(Self::default()),
            Err(e) => Err(PackageErrorKind::Internal(crate::error::file_error(path, e))),
        }
    }

    /// Writes the index atomically (temp file in the same directory, then
    /// rename). Same-directory rename keeps the swap on a single filesystem.
    async fn write_atomic(&self, path: &Path) -> Result<(), PackageErrorKind> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| PackageErrorKind::Internal(crate::error::file_error(parent, e)))?;
        }
        let tmp_path = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| PackageErrorKind::Internal(e.into()))?;
        tokio::fs::write(&tmp_path, bytes)
            .await
            .map_err(|e| PackageErrorKind::Internal(crate::error::file_error(&tmp_path, e)))?;
        tokio::fs::rename(&tmp_path, path)
            .await
            .map_err(|e| PackageErrorKind::Internal(crate::error::file_error(path, e)))?;
        Ok(())
    }

    /// Removes every entry whose owner matches `registry` + `repository`.
    ///
    /// Used on deselect/uninstall and as the first step of re-select to clear
    /// stale launcher names before publishing the new package's entries.
    fn remove_owner(&mut self, registry: &str, repository: &str) {
        self.entries
            .retain(|_, owner| !(owner.registry == registry && owner.repository == repository));
    }
}

/// Outcome of [`wire_selection`] for the caller's reporting.
#[derive(Debug, Clone)]
pub struct WireSelectionOutcome {
    /// Path to the `current` symlink that was written (or refreshed).
    pub current: std::path::PathBuf,
}

/// Wires the per-repo `current` selection symlink for `package`, updates the
/// per-registry entry-points ownership index atomically, and optionally writes
/// the candidate symlink first. Both symlinks target the package root, so
/// consumers traverse `<symlink>/content/`, `<symlink>/entrypoints/`, or
/// `<symlink>/metadata.json` from a single anchor.
///
/// Shared by [`super::install::create_install_symlinks`] and the CLI `select`
/// command so both paths run identical lock acquisition, collision detection,
/// and update logic.
///
/// # Lock order
///
/// 1. `entrypoints_index_lock` — registry-scoped — held for the full sequence.
/// 2. `select_lock` — repo-scoped — held only across the symlink write and
///    rollback window. Acquired *after* (1). See module docs.
///
/// # Errors
///
/// - [`PackageErrorKind::EntrypointNameCollision`] when any new launcher name
///   is already owned by a different `(registry, repository)` in the index.
/// - [`PackageErrorKind::Internal`] for I/O, JSON, or symlink failures.
#[allow(clippy::result_large_err)]
pub async fn wire_selection(
    fs: &file_structure::FileStructure,
    package: &oci::Identifier,
    info: &InstallInfo,
    candidate: bool,
    select: bool,
) -> Result<WireSelectionOutcome, PackageErrorKind> {
    let rm = reference_manager(fs);

    // Both `current` and `candidates/{tag}` target the package root.
    // `info.content` is `<pkg_root>/content` by construction (see
    // `tasks::common::find_in_store` and the pull pipeline), so the parent is
    // always the package root. Fall back to the original content path only as
    // a defensive guard against a future content layout that drops the
    // `content/` child — in practice this branch never fires.
    let pkg_root = info.content.parent().unwrap_or(&info.content);

    if candidate {
        let link_path = fs.symlinks.candidate(package);
        log::debug!("Creating candidate symlink at '{}'.", link_path.display());
        rm.link(&link_path, pkg_root).map_err(PackageErrorKind::Internal)?;
    }

    let current_path = fs.symlinks.current(package);
    if !select {
        return Ok(WireSelectionOutcome { current: current_path });
    }

    // Entrypoint names this package wants to publish (empty when the package
    // declares no entrypoints or selects a version without them).
    let new_names: Vec<String> = info
        .metadata
        .bundle_entrypoints()
        .map(|eps| eps.iter().map(|e| e.name.as_str().to_string()).collect())
        .unwrap_or_default();
    let needs_entrypoints = !new_names.is_empty();

    // Phase 1: acquire registry-index-lock. Serializes ALL select/deselect
    // operations across every repo under the registry so collision detection
    // and the symlink update form a single critical section.
    let _index_guard = acquire_index_lock(fs, package.registry()).await?;
    let index_path = fs.symlinks.entrypoints_index(package.registry());
    let mut index = EntrypointsIndex::read(&index_path).await?;

    // Phase 2: collision check against the index. Skip names this package
    // already owns from a prior --select (idempotent re-select must not
    // collide with itself).
    if needs_entrypoints {
        for name in &new_names {
            if let Some(owner) = index.entries.get(name)
                && !(owner.registry == package.registry() && owner.repository == package.repository())
            {
                let other_id = oci::Identifier::new_registry(owner.repository.clone(), owner.registry.clone());
                return Err(PackageErrorKind::EntrypointNameCollision {
                    name: name.clone(),
                    existing_package: other_id,
                });
            }
        }
    }

    // Phase 3: acquire the per-repo .select.lock for the symlink write.
    let _select_guard = acquire_select_lock(fs, package).await?;

    // Snapshot the pre-mutation state of every byte we are about to change so
    // rollback can restore the registry to exactly what was on disk before the
    // call:
    //
    // - the prior `current` symlink target (or absence), so it can be rewound,
    // - the entry-points index file contents (or absence), so an index swap
    //   that succeeds before the symlink write fails can be undone.
    //
    // Without the index snapshot, the previous order — symlink first, index
    // last — would either publish stale ownership (if the index write failed
    // after the symlink succeeded) or strand the symlink while the index
    // claimed cross-repo ownership of the launcher set.
    let prior_current_target = tokio::fs::read_link(&current_path).await.ok();
    let prior_index_bytes: Option<Vec<u8>> = match tokio::fs::read(&index_path).await {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            return Err(PackageErrorKind::Internal(crate::error::file_error(&index_path, e)));
        }
    };

    // Phase 4a: update + persist the index FIRST — under both locks. Writing
    // the ownership record before the symlink ensures the only way an
    // observer can see the new launcher set is alongside the corresponding
    // index entry. If the symlink write fails, the index snapshot taken
    // above is replayed so the on-disk state collapses back to pre-call.
    index.remove_owner(package.registry(), package.repository());
    if needs_entrypoints {
        let owner = IndexOwner {
            registry: package.registry().to_string(),
            repository: package.repository().to_string(),
            digest: info.identifier.digest().to_string(),
        };
        for name in new_names {
            index.entries.insert(name, owner.clone());
        }
    }
    index.write_atomic(&index_path).await?;

    // Phase 4b: commit the `current` symlink. Failure here triggers a full
    // rollback: prior symlink target (or removal) AND prior index contents
    // are restored before the error is surfaced.
    log::debug!("Creating current symlink at '{}'.", current_path.display());
    if let Err(e) = rm.link(&current_path, pkg_root) {
        rollback_symlink(&rm, &current_path, prior_current_target.as_deref());
        restore_index_snapshot(&index_path, prior_index_bytes.as_deref()).await;
        return Err(PackageErrorKind::Internal(e));
    }

    Ok(WireSelectionOutcome { current: current_path })
}

/// RAII guard combining the registry-scoped entry-points index lock and the
/// per-repo `.select.lock`, in the documented acquisition order
/// (index → select). Releases both on drop.
///
/// Used by `deselect` / `uninstall --deselect` to hold the same critical
/// section as [`wire_selection`] while clearing index entries and unlinking
/// the symlink pair.
pub struct SelectionLocks {
    /// Held first; outlives `_select` because Rust drops fields in
    /// declaration order, ensuring release order = select → index (reverse of
    /// acquisition) per the lock hierarchy.
    _index: FileLock,
    _select: FileLock,
}

/// Acquires both selection locks in the documented order.
///
/// Lock order: `index_lock` → `select_lock`. The reverse risks deadlock under
/// concurrent installs targeting different repos in the same registry.
#[allow(clippy::result_large_err)]
pub async fn acquire_selection_locks(
    fs: &file_structure::FileStructure,
    package: &oci::Identifier,
) -> Result<SelectionLocks, PackageErrorKind> {
    let index = acquire_index_lock(fs, package.registry()).await?;
    let select = acquire_select_lock(fs, package).await?;
    Ok(SelectionLocks {
        _index: index,
        _select: select,
    })
}

/// Removes every index entry owned by `package` and writes the index back
/// atomically when changes occur. Caller MUST hold the registry index lock.
///
/// No-op when the index file does not exist or no entries match.
#[allow(clippy::result_large_err)]
pub async fn clear_index_owner(
    fs: &file_structure::FileStructure,
    package: &oci::Identifier,
) -> Result<(), PackageErrorKind> {
    let index_path = fs.symlinks.entrypoints_index(package.registry());
    if !tokio::fs::try_exists(&index_path).await.unwrap_or(false) {
        return Ok(());
    }
    let mut index = EntrypointsIndex::read(&index_path).await?;
    let before = index.entries.len();
    index.remove_owner(package.registry(), package.repository());
    if index.entries.len() != before {
        index.write_atomic(&index_path).await?;
    }
    Ok(())
}

/// Restores the entry-points index file to its pre-mutation state.
///
/// `prior_bytes = None` indicates the index did not exist before the critical
/// section opened — restore by removing the file. `Some(bytes)` indicates a
/// pre-existing index whose contents must be re-materialised verbatim. Either
/// way, the on-disk index is collapsed back to what was observed under the
/// registry-index lock at the start of the operation, regardless of whatever
/// in-memory swap the caller had already attempted.
///
/// Rollback failures are logged but never propagated: the caller already has
/// a real error to surface, and a secondary failure in cleanup would only
/// obscure the root cause.
pub(super) async fn restore_index_snapshot(index_path: &Path, prior_bytes: Option<&[u8]>) {
    match prior_bytes {
        Some(bytes) => {
            if let Err(rollback_err) = tokio::fs::write(index_path, bytes).await {
                log::warn!(
                    "Failed to roll back entry-points index at '{}': {}",
                    index_path.display(),
                    rollback_err,
                );
            }
        }
        None => match tokio::fs::remove_file(index_path).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(rollback_err) => {
                log::warn!(
                    "Failed to roll back (remove) entry-points index at '{}': {}",
                    index_path.display(),
                    rollback_err,
                );
            }
        },
    }
}

/// Restores a symlink to its prior state after a partial-select failure.
///
/// On any rollback failure we log and continue: the caller already has a real
/// error to surface, and burying it under a rollback secondary failure would
/// obscure the root cause.
pub(super) fn rollback_symlink(rm: &ReferenceManager, forward_path: &Path, prior_target: Option<&Path>) {
    match prior_target {
        Some(target) => {
            if let Err(rollback_err) = rm.link(forward_path, target) {
                log::warn!(
                    "Failed to roll back symlink at '{}' to prior target '{}': {}",
                    forward_path.display(),
                    target.display(),
                    rollback_err,
                );
            }
        }
        None => {
            if let Err(rollback_err) = rm.unlink(forward_path) {
                log::warn!(
                    "Failed to roll back (unlink) symlink at '{}': {}",
                    forward_path.display(),
                    rollback_err,
                );
            }
        }
    }
}

/// Exports env vars from a package's metadata into an entry list.
pub fn export_env(
    content: &Path,
    metadata: &metadata::Metadata,
    dep_contexts: std::collections::HashMap<
        crate::package::metadata::dependency::DependencyName,
        metadata::env::accumulator::DependencyContext,
    >,
    entries: &mut Vec<metadata::env::exporter::Entry>,
) -> crate::Result<()> {
    let mut exp = metadata::env::exporter::Exporter::new(content, dep_contexts);
    if let Some(env) = metadata.env() {
        for v in env {
            exp.add(v)?;
        }
    }
    entries.extend(exp.take());
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::file_structure::{FileStructure, PackageStore};
    use crate::oci;
    use crate::package::resolved_package::ResolvedPackage;
    use crate::prelude::SerdeExt;

    /// Writes valid + resolve.json under a fake content path, then writes a
    /// metadata.json that references an undeclared dep — `load_object_data`
    /// must reject it instead of returning a half-formed `InstallInfo`.
    #[tokio::test]
    async fn load_object_data_rejects_invalid_metadata_at_consumption() {
        let tempdir = tempfile::tempdir().unwrap();
        let store_root = tempdir.path().join("packages");
        std::fs::create_dir_all(&store_root).unwrap();
        let store = PackageStore::new(&store_root);

        let digest_hex: String = "ab".repeat(32);
        let id =
            oci::Identifier::new_registry("foo/bar", "example.com").clone_with_digest(oci::Digest::Sha256(digest_hex));
        let pinned = oci::PinnedIdentifier::try_from(id).unwrap();

        let pkg_dir = store.path(&pinned);
        std::fs::create_dir_all(&pkg_dir).unwrap();
        let content_dir = pkg_dir.join("content");
        std::fs::create_dir_all(&content_dir).unwrap();

        // Bad metadata: env var references `${deps.missing.installPath}` with no
        // matching declared dep — must trigger ValidMetadata's UnknownDependencyRef.
        let bad = r#"{"type":"bundle","version":1,"dependencies":[],"env":[{"key":"FOO","value":"${deps.missing.installPath}/x"}]}"#;
        std::fs::write(pkg_dir.join("metadata.json"), bad).unwrap();
        ResolvedPackage::new()
            .write_json(pkg_dir.join("resolve.json"))
            .await
            .unwrap();

        let result = super::load_object_data(&store, &content_dir).await;
        assert!(result.is_err(), "expected validation failure, got Ok");
        let err = result.unwrap_err();
        let chain = format!("{err:#}");
        assert!(
            chain.contains("missing"),
            "error chain must mention undeclared dep name 'missing': {chain}"
        );
    }

    /// `acquire_select_lock` materializes the per-repo lock file and returns
    /// a guard. Serializes Cluster 3's transactional select state.
    #[tokio::test]
    async fn acquire_select_lock_creates_lock_file_at_expected_path() {
        let tempdir = tempfile::tempdir().unwrap();
        let fs = FileStructure::with_root(tempdir.path().to_path_buf());
        let id = oci::Identifier::new_registry("cmake", "example.com");

        let _guard = super::acquire_select_lock(&fs, &id).await.expect("acquire lock");

        let lock_path = fs.symlinks.select_lock(&id);
        assert!(
            lock_path.exists(),
            "lock file must be created at {}",
            lock_path.display()
        );
        assert_eq!(
            lock_path.file_name().unwrap().to_str().unwrap(),
            ".select.lock",
            "lock file must use the documented name"
        );
    }

    /// A second `acquire_select_lock` for the same package must block until
    /// the first guard is dropped — proves `current` symlink updates
    /// serialize across concurrent installer/deselect callers.
    #[tokio::test]
    async fn acquire_select_lock_serializes_concurrent_callers() {
        use futures::FutureExt;

        let tempdir = tempfile::tempdir().unwrap();
        let fs = FileStructure::with_root(tempdir.path().to_path_buf());
        let id = oci::Identifier::new_registry("cmake", "example.com");

        let first = super::acquire_select_lock(&fs, &id).await.expect("first acquire");

        // Second acquire must not be ready while `first` is held.
        let second_fut = super::acquire_select_lock(&fs, &id);
        tokio::pin!(second_fut);
        assert!(
            second_fut.as_mut().now_or_never().is_none(),
            "second acquire must block while the first guard is held"
        );

        drop(first);
        // After releasing, the second acquire becomes ready.
        let second = tokio::time::timeout(std::time::Duration::from_secs(2), second_fut)
            .await
            .expect("second acquire timed out after release")
            .expect("second acquire failed");
        drop(second);
    }

    /// Distinct packages must not contend on the same lock — each repo gets
    /// its own `.select.lock` file under `{base}/`.
    #[tokio::test]
    async fn acquire_select_lock_is_per_repo() {
        let tempdir = tempfile::tempdir().unwrap();
        let fs = FileStructure::with_root(tempdir.path().to_path_buf());
        let id_a = oci::Identifier::new_registry("cmake", "example.com");
        let id_b = oci::Identifier::new_registry("ninja", "example.com");

        let _guard_a = super::acquire_select_lock(&fs, &id_a).await.expect("acquire a");
        // Distinct repo: must succeed immediately, no contention.
        let _guard_b = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            super::acquire_select_lock(&fs, &id_b),
        )
        .await
        .expect("distinct-repo acquire timed out — locks are not per-repo")
        .expect("distinct-repo acquire failed");
    }

    /// `EntrypointsIndex::default()` must yield `schema_version = 1` so the
    /// missing-file path in [`super::wire_selection`] persists the documented
    /// schema on first write — the prior `#[derive(Default)]` emitted `0`,
    /// silently shipping pre-spec metadata to disk.
    #[test]
    fn entrypoints_index_default_pins_schema_version_to_one() {
        let idx = super::EntrypointsIndex::default();
        assert_eq!(
            idx.schema_version, 1,
            "Default must seed schema_version with the documented contract value"
        );
        assert!(idx.entries.is_empty(), "Default must start with no ownership entries");
    }

    /// Round-trips a freshly-created index file through the same I/O path
    /// `wire_selection` uses: `read` (returns `Default` when absent) →
    /// `write_atomic` → re-parse from disk. The persisted JSON must declare
    /// `schema_version: 1`. Locks down the contract Codex Warn 1 flagged.
    #[tokio::test]
    async fn freshly_created_index_file_persists_schema_version_one() {
        let tempdir = tempfile::tempdir().unwrap();
        let fs = FileStructure::with_root(tempdir.path().to_path_buf());
        let registry = "example.com";
        let index_path = fs.symlinks.entrypoints_index(registry);
        // Sanity: registry has no index yet — exercises the missing-file branch.
        assert!(
            !tokio::fs::try_exists(&index_path).await.unwrap(),
            "test precondition: no pre-existing index"
        );

        let idx = super::EntrypointsIndex::read(&index_path)
            .await
            .expect("read missing index");
        idx.write_atomic(&index_path).await.expect("write fresh index");

        let bytes = tokio::fs::read(&index_path).await.expect("read written index");
        let json: serde_json::Value = serde_json::from_slice(&bytes).expect("parse index json");
        assert_eq!(
            json.get("schema_version").and_then(|v| v.as_u64()),
            Some(1),
            "freshly-written index file must declare schema_version=1, got {bytes:?}",
            bytes = String::from_utf8_lossy(&bytes),
        );
    }

    /// `wire_selection` must roll the entry-points index back to its
    /// pre-call contents when symlink commit fails. We force a symlink write
    /// failure by handing `wire_selection` a content path whose
    /// `<parent>/refs/symlinks/` directory does not exist and cannot be
    /// created (parent points at a *file* instead of a directory) —
    /// `ReferenceManager::link` then fails when it tries to create the
    /// back-reference, after the index swap has already landed on disk.
    /// Post-error, the index file must byte-match the snapshot taken before
    /// the call, including the absence of the new launcher entry.
    #[tokio::test]
    async fn wire_selection_rolls_back_index_on_symlink_failure() {
        use crate::package::install_info::InstallInfo;
        use crate::package::metadata::Metadata;
        use crate::package::resolved_package::ResolvedPackage;

        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let fs = FileStructure::with_root(root.clone());

        // Seed the index with an unrelated entry so we can later prove the
        // file was not just deleted but truly restored to its prior bytes.
        let registry = "example.com";
        let index_path = fs.symlinks.entrypoints_index(registry);
        let seeded = r#"{"schema_version":1,"entries":{"unrelated":{"registry":"example.com","repository":"other/pkg","digest":"sha256:00"}}}"#;
        if let Some(parent) = index_path.parent() {
            tokio::fs::create_dir_all(parent).await.unwrap();
        }
        tokio::fs::write(&index_path, seeded).await.unwrap();
        let snapshot_before = tokio::fs::read(&index_path).await.unwrap();

        // Build a content path whose parent (the would-be package dir) is a
        // FILE, not a directory. `ReferenceManager::link` calls
        // `create_dir_all` on `<parent>/refs/symlinks/` and that fails with
        // `NotADirectory`, surfacing as the wire_selection symlink commit
        // error after the index write has already happened.
        let pkg_parent = root.join("packages").join("reg").join("sha256");
        std::fs::create_dir_all(&pkg_parent).unwrap();
        let pkg_dir_as_file = pkg_parent.join("ab1234567890abcdef1234567890abcd");
        std::fs::write(&pkg_dir_as_file, b"poison").unwrap();
        let content_path = pkg_dir_as_file.join("content");

        let id = oci::Identifier::new_registry("myorg/cmake", registry)
            .clone_with_digest(oci::Digest::Sha256("a".repeat(64)));
        let pinned = oci::PinnedIdentifier::try_from(id.clone()).unwrap();
        let json = serde_json::json!({
            "type": "bundle",
            "version": 1,
            "entrypoints": [{"name": "cmake", "target": "${installPath}/bin/cmake"}],
        })
        .to_string();
        let metadata: Metadata = serde_json::from_str(&json).unwrap();
        let info = InstallInfo {
            identifier: pinned.clone(),
            metadata,
            resolved: ResolvedPackage::new(),
            content: content_path,
        };
        let pkg_id: oci::Identifier = pinned.into();

        let result = super::wire_selection(&fs, &pkg_id, &info, false, true).await;
        assert!(
            result.is_err(),
            "test precondition: poisoned content path must produce a symlink commit failure"
        );

        // Index file must be byte-identical to the pre-call snapshot.
        let snapshot_after = tokio::fs::read(&index_path).await.expect("index restored on rollback");
        assert_eq!(
            snapshot_after, snapshot_before,
            "wire_selection must restore the entry-points index byte-for-byte after a symlink commit failure"
        );

        // And the new entry must NOT be visible — proves the rollback covers
        // the in-flight ownership change, not just the file's existence.
        let parsed: super::EntrypointsIndex = serde_json::from_slice(&snapshot_after).unwrap();
        assert!(
            !parsed.entries.contains_key("cmake"),
            "rolled-back index must not contain the launcher name from the failed call"
        );
        assert!(
            parsed.entries.contains_key("unrelated"),
            "rolled-back index must preserve unrelated entries: {entries:?}",
            entries = parsed.entries.keys().collect::<Vec<_>>(),
        );
    }
}
