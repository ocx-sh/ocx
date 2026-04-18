// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Shared utilities for task modules.
//!
//! Free functions only — no `impl PackageManager`. Since `tasks` is a private
//! module, these helpers are invisible to external consumers.

use std::path::Path;

use std::collections::{HashMap, HashSet};

use tokio::task::JoinSet;

use crate::{
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
    Ok((metadata_result?, resolved_result?))
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

/// Exports env vars from a package's metadata into an entry list.
pub fn export_env(
    content: &Path,
    metadata: &metadata::Metadata,
    entries: &mut Vec<metadata::env::exporter::Entry>,
) -> crate::Result<()> {
    let mut exp = metadata::env::exporter::Exporter::new(content);
    if let Some(env) = metadata.env() {
        for v in env {
            exp.add(v)?;
        }
    }
    entries.extend(exp.take());
    Ok(())
}
