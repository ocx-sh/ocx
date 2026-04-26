// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Shared utilities for task modules.
//!
//! Free functions only — no `impl PackageManager`. Since `tasks` is a private
//! module, these helpers are invisible to external consumers.
//!
//! ## Selection-state lock order
//!
//! Selection-state mutations (the per-repo `current` symlink) are guarded by
//! the per-repo `.select.lock`.
//!
//! - **`{symlinks/{registry}/{repo}}/.select.lock`** (per repo) — held for the
//!   actual symlink writes/rollback inside [`wire_selection`].
//!
//! `deselect` and `uninstall --deselect` acquire the same per-repo
//! `.select.lock` before clearing symlinks.

use std::collections::{HashMap, HashSet};
use std::path::Path;

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

/// Outcome of [`wire_selection`] for the caller's reporting.
#[derive(Debug, Clone)]
pub struct WireSelectionOutcome {
    /// Path to the `current` symlink that was written (or refreshed).
    pub current: std::path::PathBuf,
}

/// Wires the per-repo `current` selection symlink for `package` and optionally
/// writes the candidate symlink first. Both symlinks target the package root,
/// so consumers traverse `<symlink>/content/`, `<symlink>/entrypoints/`, or
/// `<symlink>/metadata.json` from a single anchor.
///
/// Shared by [`super::install::create_install_symlinks`] and the CLI `select`
/// command so both paths run identical lock acquisition and symlink logic.
/// Entrypoint name collision detection has moved to `collect_entrypoints` in
/// `visible.rs`, called from `pull.rs` (Stage 1) and `apply_visible_packages`
/// (Stage 2).
///
/// # Lock order
///
/// Acquires the per-repo `.select.lock` before the symlink write. See module
/// docs for the updated lock hierarchy.
///
/// # Errors
///
/// - [`PackageErrorKind::Internal`] for I/O or symlink failures.
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

    // Acquire the per-repo .select.lock for the symlink write.
    let _select_guard = acquire_select_lock(fs, package).await?;

    // Snapshot the prior `current` symlink target so rollback can restore it
    // on symlink write failure.
    let prior_current_target = tokio::fs::read_link(&current_path).await.ok();

    // Commit the `current` symlink. Failure here triggers rollback of the
    // prior symlink target before the error is surfaced.
    log::debug!("Creating current symlink at '{}'.", current_path.display());
    if let Err(e) = rm.link(&current_path, pkg_root) {
        rollback_symlink(&rm, &current_path, prior_current_target.as_deref());
        return Err(PackageErrorKind::Internal(e));
    }

    Ok(WireSelectionOutcome { current: current_path })
}

/// RAII guard for the per-repo `.select.lock`. Releases on drop.
///
/// Used by `deselect` / `uninstall --deselect` to hold the same critical
/// section as [`wire_selection`] while unlinking the symlink pair.
pub struct SelectionLocks {
    _select: FileLock,
}

/// Acquires the per-repo `.select.lock`.
///
/// Serializes mutations of the per-repo `current` symlink across
/// `install --select`, `deselect`, and `uninstall --deselect`.
#[allow(clippy::result_large_err)]
pub async fn acquire_selection_locks(
    fs: &file_structure::FileStructure,
    package: &oci::Identifier,
) -> Result<SelectionLocks, PackageErrorKind> {
    let select = acquire_select_lock(fs, package).await?;
    Ok(SelectionLocks { _select: select })
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
    dep_contexts: &std::collections::HashMap<
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
    use crate::prelude::SerdeExt as _;

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
}
