// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashSet;
use std::path::PathBuf;

use crate::{error::PackageError, error::PackageErrorKind};

use super::purge::{UnrootedPaths, purge_unrooted_paths};

use super::super::PackageManager;

/// Result of a successful uninstall where the candidate symlink existed.
pub struct UninstallResult {
    /// The candidate symlink path that was removed.
    pub candidate: PathBuf,
    /// The purged object directory; `None` when purge was not requested or the object is still referenced.
    pub purged: Option<PathBuf>,
}

/// Intermediate result from symlink removal (before purge).
struct SymlinkRemoval {
    candidate: Option<PathBuf>,
    /// Read-link target of the candidate symlink (the package root).
    pkg_root: Option<PathBuf>,
}

impl PackageManager {
    /// Uninstalls multiple packages, optionally purging what nothing else holds.
    ///
    /// The purge keeps a package any root holds, a project lock pin included, by `clean`'s
    /// reachability roots; an unreadable lock keeps every package.
    pub async fn uninstall_all(
        &self,
        packages: &[ocx_oci::PackageRef],
        deselect: bool,
        purge: bool,
    ) -> Result<Vec<Option<UninstallResult>>, crate::error::Error> {
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
            return Err(crate::error::Error::UninstallFailed(errors));
        }

        let purge_seeds: Vec<PathBuf> = if purge {
            removals.iter().filter_map(|r| r.pkg_root.clone()).collect()
        } else {
            Vec::new()
        };

        let mut purged_set: HashSet<PathBuf> = HashSet::new();
        if !purge_seeds.is_empty() {
            let outcome = purge_unrooted_paths(self, &purge_seeds).await.map_err(|e| {
                crate::error::Error::UninstallFailed(vec![PackageError::new(
                    packages[0].clone(),
                    PackageErrorKind::Internal(e),
                )])
            })?;
            match outcome {
                Some(UnrootedPaths { held, .. }) => purged_set.extend(
                    purge_seeds
                        .into_iter()
                        .zip(held)
                        .filter_map(|(seed, held)| (!held).then_some(seed)),
                ),
                None => log::warn!("kept every package: a project lock could not be read, so nothing was purged"),
            }
        }

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
    fs: &ocx_store::file_structure::FileStructure,
    package: &ocx_oci::PackageRef,
    deselect: bool,
) -> Result<Option<(PathBuf, Option<PathBuf>)>, PackageErrorKind> {
    log::debug!("Uninstalling package '{}'.", package);

    if package.digest().is_some() {
        return Err(PackageErrorKind::SymlinkRequiresTag);
    }

    let rm = super::common::reference_manager(fs);
    let candidate_path = fs.symlinks.candidate(package);

    let content_path = if ocx_util::fs::symlink::is_link(&candidate_path) {
        let path = tokio::fs::read_link(&candidate_path).await.ok();
        log::trace!("Candidate content path: {:?}", path);
        rm.unlink(&candidate_path)
            .map_err(|error| PackageErrorKind::Internal(error.into()))?;
        path
    } else {
        // Debug, not warn: uninstalling an already-uninstalled tag is the normal idempotent case.
        log::debug!(
            "Package '{}' has no installed candidate at '{}' — nothing to uninstall.",
            package,
            candidate_path.display(),
        );
        return Ok(None);
    };

    if deselect {
        // Held for the whole teardown, as in tasks/deselect.rs, or a concurrent select races the unlink.
        let _locks = super::common::acquire_selection_locks(fs, package).await?;

        let current_path = fs.symlinks.current(package);

        if ocx_util::fs::symlink::is_link(&current_path) {
            rm.unlink(&current_path)
                .map_err(|error| PackageErrorKind::Internal(error.into()))?;
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
