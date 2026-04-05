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
    file_structure::{self, ObjectStore},
    log, oci,
    package::{install_info::InstallInfo, metadata, resolved_package::ResolvedPackage},
    package_manager::error::{self, PackageError, PackageErrorKind},
    prelude::SerdeExt,
    reference_manager::ReferenceManager,
};

/// Finds a package in the object store without index resolution.
///
/// The identifier must carry a digest. Returns the installed package info if
/// present, or `None` if the object is absent. Also serves as defense layer 2
/// in the concurrent pull safety model.
pub async fn find_in_store(
    objects: &ObjectStore,
    identifier: &oci::PinnedIdentifier,
) -> Result<Option<InstallInfo>, PackageErrorKind> {
    let storage = objects.path(identifier);
    let content = storage.join("content");
    let metadata_path = storage.join("metadata.json");
    let resolve_path = storage.join("resolve.json");
    if tokio::fs::try_exists(&content).await.unwrap_or(false)
        && tokio::fs::try_exists(&metadata_path).await.unwrap_or(false)
        && tokio::fs::try_exists(&resolve_path).await.unwrap_or(false)
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

/// Loads metadata.json and resolve.json for an existing content path.
///
/// Uses `ObjectStore::metadata_for_content` / `resolve_for_content` which
/// follow symlinks, making this safe for both direct object paths and install
/// symlinks.
pub async fn load_object_data(
    objects: &ObjectStore,
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
