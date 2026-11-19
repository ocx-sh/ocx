// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashSet;
use std::path::PathBuf;

use crate::{error::PackageError, error::PackageErrorKind};

use super::purge::{UnrootedPaths, purge_unrooted_paths};

use super::super::PackageManager;
use crate::activity::{Action, PackageActivity, Role};

/// Result of a successful uninstall where the candidate symlink existed.
pub struct UninstallResult {
    /// The candidate symlink path that was removed.
    pub candidate: PathBuf,
    /// The registry, repository and tag the caller named, at the digest the candidate pointed at;
    /// `None` when the package directory no longer held a readable digest.
    pub pinned: Option<ocx_oci::PinnedPackageRef>,
    /// The purged object directory; `None` when purge was not requested or the object is still referenced.
    pub purged: Option<PathBuf>,
}

/// Intermediate result from symlink removal (before purge).
struct SymlinkRemoval {
    candidate: Option<PathBuf>,
    /// Read-link target of the candidate symlink (the package root).
    pkg_root: Option<PathBuf>,
    /// The package named by the request, at the digest recorded in the package directory.
    pinned: Option<ocx_oci::PinnedPackageRef>,
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
                Ok(Some((candidate, pkg_root))) => {
                    let pinned = pinned_at_candidate_target(self.file_structure(), package, pkg_root.as_deref()).await;
                    removals.push(SymlinkRemoval {
                        candidate: Some(candidate),
                        pkg_root,
                        pinned,
                    });
                }
                Ok(None) => removals.push(SymlinkRemoval {
                    candidate: None,
                    pkg_root: None,
                    pinned: None,
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
                if let (Some(log), Some(pinned)) = (self.activity_log(), r.pinned.as_ref()) {
                    // The store never records the platform a package was set up for.
                    log.record(PackageActivity {
                        pinned: pinned.clone(),
                        platform: None,
                        action: Action::Removed,
                        role: Role::Root,
                        store_hit: false,
                        download_size: 0,
                        resolve: None,
                        download: None,
                        extract: None,
                    });
                }
                Some(UninstallResult {
                    candidate,
                    pinned: r.pinned,
                    purged,
                })
            })
            .collect();

        Ok(results)
    }
}

/// `package` pinned to the digest the removed candidate pointed at, read before any purge.
///
/// `None` when the link was dangling or the package directory holds no readable digest: the
/// removal still happened, but no identity can be named for it.
async fn pinned_at_candidate_target(
    fs: &ocx_store::file_structure::FileStructure,
    package: &ocx_oci::PackageRef,
    pkg_root: Option<&std::path::Path>,
) -> Option<ocx_oci::PinnedPackageRef> {
    let digest_path = fs.packages.digest_file_for_content(pkg_root?).ok()?;
    match ocx_store::file_structure::read_digest_file(&digest_path).await {
        Ok(digest) => Some(ocx_oci::PinnedPackageRef::pin(package, digest)),
        Err(error) => {
            log::debug!("No digest for the removed candidate of '{package}': {error}");
            None
        }
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

#[cfg(test)]
mod tests {
    use ocx_index::{ChainMode, Index, IndexStore, LocalConfig, LocalIndex};
    use ocx_store::file_structure::{FileStructure, write_digest_file};
    use ocx_store::reference_manager::ReferenceManager;

    use crate::activity::{Action, ActivityLog, Role};

    use super::*;

    const REGISTRY: &str = "example.com";

    fn manager(ocx_home: &std::path::Path) -> PackageManager {
        let file_structure = FileStructure::with_root(ocx_home.to_path_buf());
        let index = Index::from_chained(
            LocalIndex::new(LocalConfig {
                index_store: IndexStore::new(ocx_home.join("index")),
            }),
            vec![],
            ChainMode::Offline,
        );
        PackageManager::new(file_structure, index, None, REGISTRY)
    }

    /// Installs the shape `ocx package install` leaves: a package directory with its `digest` file
    /// and a candidate symlink to it. Returns the tag-only request.
    async fn install_candidate(
        manager: &PackageManager,
        repository: &str,
        tag: &str,
        fill: &str,
    ) -> ocx_oci::PackageRef {
        let fs = manager.file_structure();
        let request = ocx_oci::PackageRef::new_registry(repository, REGISTRY).clone_with_tag(tag);
        let digest = ocx_oci::Digest::Sha256(fill.repeat(64));
        let pinned = ocx_oci::PinnedPackageRef::pin(&request, digest.clone());
        let package_dir = fs.packages.path(&pinned);
        tokio::fs::create_dir_all(package_dir.join("content")).await.unwrap();
        write_digest_file(&package_dir.join(ocx_store::file_structure::DIGEST_FILENAME), &digest)
            .await
            .unwrap();
        let candidate = fs.symlinks.candidate(&request);
        std::fs::create_dir_all(candidate.parent().unwrap()).unwrap();
        ReferenceManager::new(fs.clone())
            .link(&candidate, &package_dir)
            .unwrap();
        request
    }

    /// The record names what the user typed plus the digest the candidate pointed at.
    #[tokio::test(flavor = "multi_thread")]
    async fn uninstall_records_the_pinned_identity() {
        let dir = tempfile::tempdir().unwrap();
        let log = ActivityLog::new();
        let manager = manager(dir.path()).with_activity_log(log.clone());
        let request = install_candidate(&manager, "acme/tool", "1.0", "a").await;

        let results = manager
            .uninstall_all(std::slice::from_ref(&request), false, false)
            .await
            .unwrap();

        let expected = ocx_oci::PinnedPackageRef::pin(&request, ocx_oci::Digest::Sha256("a".repeat(64)));
        let result = results[0].as_ref().expect("the candidate existed");
        assert_eq!(result.pinned.as_ref(), Some(&expected));
        let drained = log.drain();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].pinned, expected);
        assert_eq!(drained[0].action, Action::Removed);
        assert_eq!(drained[0].role, Role::Root);
        assert!(drained[0].platform.is_none(), "the store records no platform");
    }

    /// A manager without a log appends nothing; the same removal through one with a log does.
    #[tokio::test(flavor = "multi_thread")]
    async fn no_log_attached_records_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let bare = manager(dir.path());
        assert!(bare.activity_log().is_none());
        let request = install_candidate(&bare, "acme/tool", "1.0", "a").await;
        let results = bare.uninstall_all(&[request], false, false).await.unwrap();
        assert!(results[0].is_some(), "the removal itself still happens");

        let log = ActivityLog::new();
        let logged = manager(dir.path()).with_activity_log(log.clone());
        let request = install_candidate(&logged, "acme/other", "1.0", "b").await;
        logged.uninstall_all(&[request], false, false).await.unwrap();
        assert_eq!(log.drain().len(), 1, "an attached log does record");
    }

    /// An absent candidate is not a removal.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_absent_candidate_records_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let log = ActivityLog::new();
        let manager = manager(dir.path()).with_activity_log(log.clone());
        let request = ocx_oci::PackageRef::new_registry("acme/tool", REGISTRY).clone_with_tag("1.0");

        let results = manager.uninstall_all(&[request], false, false).await.unwrap();

        assert!(results[0].is_none());
        assert!(log.drain().is_empty());
    }
}
