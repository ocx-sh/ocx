// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

use tracing::info_span;

use crate::{
    log, oci,
    package_manager::{self, error::PackageError, error::PackageErrorKind},
};

use super::garbage_collection::GarbageCollector;

use super::super::PackageManager;

/// Result of a successful uninstall where the candidate symlink existed.
pub struct UninstallResult {
    /// The candidate symlink path that was removed.
    pub candidate: PathBuf,
    /// The object directory that was purged, if purging was requested and the
    /// object had no remaining references.  `None` when purge was not requested
    /// or the object still has references.
    pub purged: Option<PathBuf>,
}

/// Intermediate result from symlink removal (before purge).
struct SymlinkRemoval {
    candidate: Option<PathBuf>,
    /// Read-link target of the candidate symlink — the package root after the
    /// post-flatten layout change.
    pkg_root: Option<PathBuf>,
}

impl PackageManager {
    /// Removes the candidate symlink for `package` and optionally the current
    /// symlink (`deselect`) and the backing object directory (`purge`).
    ///
    /// Returns `Some(UninstallResult)` when the candidate symlink existed and
    /// was removed, or `None` when no candidate was present (no-op).
    pub async fn uninstall(
        &self,
        package: &oci::Identifier,
        deselect: bool,
        purge: bool,
    ) -> Result<Option<UninstallResult>, PackageErrorKind> {
        let result = uninstall_symlinks(self.file_structure(), package, deselect).await?;
        let Some((candidate, pkg_root)) = result else {
            return Ok(None);
        };

        let mut purged = None;
        // `pkg_root` is the candidate symlink's read-link target, which the
        // post-flatten layout points at the package root directly (no further
        // `.parent()` needed).
        if purge && let Some(ref obj_dir) = pkg_root {
            let gc = GarbageCollector::build(self.file_structure(), &[])
                .await
                .map_err(PackageErrorKind::Internal)?;
            let removed = gc
                .purge(std::slice::from_ref(obj_dir))
                .await
                .map_err(PackageErrorKind::Internal)?;
            if removed.iter().any(|p| p == obj_dir) {
                purged = Some(obj_dir.clone());
            }
        }

        Ok(Some(UninstallResult { candidate, purged }))
    }

    /// Uninstalls multiple packages, optionally purging unreferenced objects.
    ///
    /// Unlike a simple loop over [`PackageManager::uninstall`], this method
    /// builds the [`GarbageCollector`] reachability graph **once** for all
    /// packages. The graph walks the entire object store (`O(all objects)`),
    /// so batching avoids redundant filesystem scans when purging multiple
    /// packages.
    pub async fn uninstall_all(
        &self,
        packages: &[oci::Identifier],
        deselect: bool,
        purge: bool,
    ) -> Result<Vec<Option<UninstallResult>>, package_manager::error::Error> {
        // Phase 1: Remove symlinks for all packages, collecting content paths.
        let mut removals: Vec<SymlinkRemoval> = Vec::with_capacity(packages.len());
        let mut errors: Vec<PackageError> = Vec::new();

        for package in packages {
            match uninstall_symlinks(self.file_structure(), package, deselect).await {
                Ok(Some((candidate, pkg_root))) => removals.push(SymlinkRemoval {
                    candidate: Some(candidate),
                    pkg_root,
                }),
                Ok(None) => removals.push(SymlinkRemoval {
                    candidate: None,
                    pkg_root: None,
                }),
                Err(kind) => errors.push(PackageError::new(package.clone(), kind)),
            }
        }

        if !errors.is_empty() {
            return Err(package_manager::error::Error::UninstallFailed(errors));
        }

        // Phase 2: Batch purge — collect all object dirs, purge once.
        let purge_seeds: Vec<PathBuf> = if purge {
            removals.iter().filter_map(|r| r.pkg_root.clone()).collect()
        } else {
            Vec::new()
        };

        let purged_set: std::collections::HashSet<PathBuf> = if !purge_seeds.is_empty() {
            let gc = GarbageCollector::build(self.file_structure(), &[]).await.map_err(|e| {
                package_manager::error::Error::UninstallFailed(vec![PackageError::new(
                    packages[0].clone(),
                    PackageErrorKind::Internal(e),
                )])
            })?;
            gc.purge(&purge_seeds)
                .await
                .map_err(|e| {
                    package_manager::error::Error::UninstallFailed(vec![PackageError::new(
                        packages[0].clone(),
                        PackageErrorKind::Internal(e),
                    )])
                })?
                .into_iter()
                .collect()
        } else {
            std::collections::HashSet::new()
        };

        // Phase 3: Build results.
        let results = removals
            .into_iter()
            .map(|r| {
                let candidate = r.candidate?;
                let purged = r
                    .pkg_root
                    .as_ref()
                    .filter(|obj_dir| purged_set.contains(obj_dir.as_path()))
                    .cloned();
                Some(UninstallResult { candidate, purged })
            })
            .collect();

        Ok(results)
    }
}

/// Removes install symlinks (candidate + optionally current) without purging.
/// Returns `(candidate_path, content_path)` or `None` if no candidate existed.
async fn uninstall_symlinks(
    fs: &crate::file_structure::FileStructure,
    package: &oci::Identifier,
    deselect: bool,
) -> Result<Option<(PathBuf, Option<PathBuf>)>, PackageErrorKind> {
    let _span = crate::cli::progress::spinner_span(info_span!("Uninstalling", package = %package), package).entered();
    log::debug!("Uninstalling package '{}'.", package);

    if package.digest().is_some() {
        return Err(PackageErrorKind::SymlinkRequiresTag);
    }

    let rm = super::common::reference_manager(fs);
    let candidate_path = fs.symlinks.candidate(package);

    let content_path = if crate::symlink::is_link(&candidate_path) {
        let path = tokio::fs::read_link(&candidate_path).await.ok();
        log::trace!("Candidate content path: {:?}", path);
        rm.unlink(&candidate_path).map_err(PackageErrorKind::Internal)?;
        path
    } else {
        log::warn!(
            "Package '{}' has no installed candidate at '{}' — nothing to uninstall.",
            package,
            candidate_path.display(),
        );
        return Ok(None);
    };

    if deselect {
        // Hold the per-repo .select.lock for the entire teardown.
        // Symmetric with tasks/deselect.rs.
        let _locks = super::common::acquire_selection_locks(fs, package).await?;

        let current_path = fs.symlinks.current(package);

        if crate::symlink::is_link(&current_path) {
            rm.unlink(&current_path).map_err(PackageErrorKind::Internal)?;
        } else {
            log::debug!(
                "Package '{}' has no current symlink at '{}' — skipping deselect.",
                package,
                current_path.display(),
            );
        }
    }

    Ok(Some((candidate_path, content_path)))
}
