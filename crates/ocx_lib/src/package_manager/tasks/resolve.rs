// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::{HashMap, HashSet};

use tokio::task::JoinSet;
use tracing::{Instrument, info_span};

use crate::{
    oci,
    oci::index::SelectResult,
    package::install_info::InstallInfo,
    package_manager::{self, DependencyError, error::PackageError, error::PackageErrorKind},
};

use super::super::PackageManager;

impl PackageManager {
    /// Resolves an identifier through the index (tag → digest, platform matching).
    pub async fn resolve(
        &self,
        package: &oci::Identifier,
        platforms: Vec<oci::Platform>,
    ) -> Result<oci::PinnedIdentifier, PackageErrorKind> {
        match self.index().select(package, platforms).await {
            Ok(SelectResult::Found(id)) => {
                oci::PinnedIdentifier::try_from(id).map_err(|_| PackageErrorKind::DigestMissing)
            }
            Ok(SelectResult::Ambiguous(v)) => Err(PackageErrorKind::SelectionAmbiguous(v)),
            Ok(SelectResult::NotFound) => Err(PackageErrorKind::NotFound),
            Err(e) => Err(PackageErrorKind::Internal(e)),
        }
    }

    /// Resolves multiple identifiers in parallel, preserving input order.
    pub async fn resolve_all(
        &self,
        packages: &[oci::Identifier],
        platforms: Vec<oci::Platform>,
    ) -> Result<Vec<oci::PinnedIdentifier>, package_manager::error::Error> {
        if packages.is_empty() {
            return Ok(Vec::new());
        }
        if packages.len() == 1 {
            let pinned = self
                .resolve(&packages[0], platforms)
                .instrument(crate::cli::progress::spinner_span(
                    info_span!("Resolving", package = %packages[0]),
                    &packages[0],
                ))
                .await
                .map_err(|kind| {
                    package_manager::error::Error::FindFailed(vec![PackageError::new(packages[0].clone(), kind)])
                })?;
            return Ok(vec![pinned]);
        }

        let mut tasks = JoinSet::new();
        for package in packages {
            let mgr = self.clone();
            let package = package.clone();
            let platforms = platforms.clone();
            let span = crate::cli::progress::spinner_span(info_span!("Resolving", package = %package), &package);
            tasks.spawn(
                async move {
                    let result = mgr.resolve(&package, platforms).await;
                    (package, result)
                }
                .instrument(span),
            );
        }

        super::common::drain_package_tasks(packages, tasks, package_manager::error::Error::FindFailed).await
    }

    /// Resolves environment entries for packages, including transitive deps.
    ///
    /// Uses pre-computed export flags from resolve.json — no recursive metadata
    /// walk needed. Dependencies are already in topological order (deps before
    /// dependents) from `with_dependencies`.
    ///
    /// Detects conflicts: if the same `registry/repo` appears with different
    /// digests across the requested packages, an error is returned.
    pub async fn resolve_env(
        &self,
        packages: &[InstallInfo],
    ) -> crate::Result<Vec<crate::package::metadata::env::exporter::Entry>> {
        let objects = &self.file_structure().objects;
        let mut seen_digests = HashSet::new();
        let mut seen_repos: HashMap<(String, String), oci::Digest> = HashMap::new();
        let mut entries = Vec::new();

        for pkg in packages {
            // Exported transitive deps first (topological order preserved).
            for dep in &pkg.resolved.dependencies {
                if !dep.exported {
                    continue;
                }
                if !check_exported(&dep.identifier, &mut seen_digests, &mut seen_repos)? {
                    continue;
                }
                let content = objects.content(&dep.identifier);
                let (dep_metadata, _) = super::common::load_object_data(objects, &content).await?;
                super::common::export_env(&content, &dep_metadata, &mut entries)?;
            }
            // Then the root package itself.
            if check_exported(&pkg.resolved.identifier, &mut seen_digests, &mut seen_repos)? {
                super::common::export_env(&pkg.content, &pkg.metadata, &mut entries)?;
            }
        }
        Ok(entries)
    }

    /// Returns the set of digests that are transitively exported by the given
    /// packages. Roots are always included.
    ///
    /// Uses pre-computed export flags — no metadata loading needed, just
    /// identifier filtering. Detects conflicts: if the same `registry/repo`
    /// appears with different digests among exported deps, an error is returned.
    pub async fn resolve_exported_set(&self, packages: &[InstallInfo]) -> crate::Result<HashSet<oci::Digest>> {
        let mut seen_digests = HashSet::new();
        let mut seen_repos: HashMap<(String, String), oci::Digest> = HashMap::new();

        for pkg in packages {
            for dep in &pkg.resolved.dependencies {
                if dep.exported {
                    check_exported(&dep.identifier, &mut seen_digests, &mut seen_repos)?;
                }
            }
            check_exported(&pkg.resolved.identifier, &mut seen_digests, &mut seen_repos)?;
        }
        Ok(seen_digests)
    }
}

/// Checks for conflicts and deduplicates an exported dependency.
/// Returns `true` if newly inserted, `false` if already seen.
fn check_exported(
    id: &oci::PinnedIdentifier,
    seen_digests: &mut HashSet<oci::Digest>,
    seen_repos: &mut HashMap<(String, String), oci::Digest>,
) -> crate::Result<bool> {
    let digest = id.digest();
    let repo_key = (id.registry().to_owned(), id.repository().to_owned());
    if let Some(existing) = seen_repos.get(&repo_key)
        && *existing != digest
    {
        return Err(crate::Error::Dependency(DependencyError::Conflict {
            repository: format!("{}/{}", id.registry(), id.repository()),
            digests: vec![existing.clone(), digest.clone()],
        }));
    }
    seen_repos.insert(repo_key, digest.clone());
    Ok(seen_digests.insert(digest))
}
