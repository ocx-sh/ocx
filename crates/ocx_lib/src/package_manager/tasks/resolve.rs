// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::{HashMap, HashSet};

use tokio::task::JoinSet;
use tracing::{Instrument, info_span};

use crate::{
    oci,
    oci::index::SelectResult,
    package::install_info::InstallInfo,
    package_manager::{self, error::PackageError, error::PackageErrorKind},
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
    /// Uses pre-computed visibility from resolve.json — no recursive metadata
    /// walk needed. Dependencies are already in topological order (deps before
    /// dependents) from `with_dependencies`.
    ///
    /// Root packages are direct exec/env targets, so both self-visible and
    /// consumer-visible deps contribute (everything except Sealed). The
    /// propagation algebra already ensures transitive deps behind Private
    /// edges get the correct resolved visibility.
    ///
    /// Detects conflicts: if the same `registry/repo` appears with different
    /// digests across the requested packages, an error is returned.
    pub async fn resolve_env(
        &self,
        packages: &[InstallInfo],
    ) -> crate::Result<Vec<crate::package::metadata::env::exporter::Entry>> {
        let objects = &self.file_structure().packages;
        let mut seen_digests = HashSet::new();
        let mut seen_repos: HashMap<oci::Repository, oci::Digest> = HashMap::new();
        let mut entries = Vec::new();

        for pkg in packages {
            // Visible transitive deps first (topological order preserved).
            // Root packages are direct exec targets — include self-visible
            // (Private, Public) and consumer-visible (Public, Interface) deps.
            for dep in &pkg.resolved.dependencies {
                if !dep.visibility.is_visible() {
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
            if check_exported(&pkg.identifier, &mut seen_digests, &mut seen_repos)? {
                super::common::export_env(&pkg.content, &pkg.metadata, &mut entries)?;
            }
        }
        Ok(entries)
    }

    /// Returns the set of digests that are visible from the given packages.
    /// Roots are always included.
    ///
    /// Uses pre-computed visibility — no metadata loading needed, just
    /// identifier filtering. Detects conflicts: if the same `registry/repo`
    /// appears with different digests among visible deps, an error is returned.
    pub async fn resolve_visible_set(&self, packages: &[InstallInfo]) -> crate::Result<HashSet<oci::Digest>> {
        let mut seen_digests = HashSet::new();
        let mut seen_repos: HashMap<oci::Repository, oci::Digest> = HashMap::new();

        for pkg in packages {
            for dep in &pkg.resolved.dependencies {
                if dep.visibility.is_visible() {
                    check_exported(&dep.identifier, &mut seen_digests, &mut seen_repos)?;
                }
            }
            check_exported(&pkg.identifier, &mut seen_digests, &mut seen_repos)?;
        }
        Ok(seen_digests)
    }
}

/// Deduplicates a visible dependency by digest, warning on conflicts.
///
/// Returns `true` if newly inserted, `false` if already seen (or conflict
/// where first-seen wins). When the same `registry/repo` appears with
/// different digests, a warning is emitted and the first-seen digest is
/// kept — matching the last-writer-wins semantics of scalar env vars.
fn check_exported(
    id: &oci::PinnedIdentifier,
    seen_digests: &mut HashSet<oci::Digest>,
    seen_repos: &mut HashMap<oci::Repository, oci::Digest>,
) -> crate::Result<bool> {
    let digest = id.digest();
    let repo_key = oci::Repository::from(&**id);
    if let Some(existing) = seen_repos.get(&repo_key)
        && *existing != digest
    {
        tracing::warn!(
            "Conflicting digests for {}: keeping {}, ignoring {}.",
            repo_key,
            existing,
            digest,
        );
        return Ok(false);
    }
    seen_repos.insert(repo_key, digest.clone());
    Ok(seen_digests.insert(digest))
}
