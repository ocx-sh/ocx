// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use tokio::task::JoinSet;
use tracing::{Instrument, info_span};

use crate::{
    log, oci,
    package::install_info::InstallInfo,
    package_manager::{self, error::PackageError, error::PackageErrorKind},
};

use super::super::PackageManager;

impl PackageManager {
    /// Downloads a package and creates install symlinks.
    ///
    /// Delegates to [`PackageManager::pull`] for the actual download and
    /// transitive dependency resolution (see that method for concurrency
    /// safety), then optionally creates:
    ///
    /// - A **candidate** symlink at `installs/{repo}/candidates/{tag}` when
    ///   `candidate` is `true` — pins this version as an installed candidate.
    /// - A **current** symlink at `installs/{repo}/current` when `select` is
    ///   `true` — makes this version the active selection.
    ///
    /// Symlinks are managed via [`ReferenceManager::link`] which also creates
    /// back-references in the object store for GC tracking.
    pub async fn install(
        &self,
        package: &oci::Identifier,
        platforms: Vec<oci::Platform>,
        candidate: bool,
        select: bool,
    ) -> Result<InstallInfo, PackageErrorKind> {
        let install_info = self.pull(package, platforms).await?;

        let rm = super::common::reference_manager(self.file_structure());
        if candidate {
            let link_path = self.file_structure().installs.candidate(package);
            log::debug!("Creating candidate symlink at '{}'.", link_path.display());
            rm.link(&link_path, &install_info.content)
                .map_err(PackageErrorKind::Internal)?;
        }
        if select {
            let link_path = self.file_structure().installs.current(package);
            log::debug!("Creating current symlink at '{}'.", link_path.display());
            rm.link(&link_path, &install_info.content)
                .map_err(PackageErrorKind::Internal)?;
        }

        Ok(install_info)
    }

    /// Installs multiple packages in parallel.
    ///
    /// Each task creates its own [`PullTracker`] internally via
    /// [`PackageManager::pull`]. Cross-package diamond dependencies are
    /// deduplicated by the object store check (defense layer 2) and
    /// file locking (defense layer 3).
    ///
    /// Results are returned in input order via
    /// [`drain_package_tasks`](super::common::drain_package_tasks).
    ///
    /// See [`PackageManager::install`] for per-package behavior and
    /// [`PackageManager::pull`] for concurrency safety.
    pub async fn install_all(
        &self,
        packages: Vec<oci::Identifier>,
        platforms: Vec<oci::Platform>,
        candidate: bool,
        select: bool,
    ) -> Result<Vec<InstallInfo>, package_manager::error::Error> {
        if packages.is_empty() {
            return Ok(Vec::new());
        }
        if packages.len() == 1 {
            let info = self
                .install(&packages[0], platforms, candidate, select)
                .instrument(crate::cli::progress::spinner_span(
                    info_span!("Installing", package = %packages[0]),
                    &packages[0],
                ))
                .await
                .map_err(|kind| {
                    package_manager::error::Error::InstallFailed(vec![PackageError::new(packages[0].clone(), kind)])
                })?;
            return Ok(vec![info]);
        }

        let mut tasks = JoinSet::new();
        for package in &packages {
            let mgr = self.clone();
            let package = package.clone();
            let platforms = platforms.clone();
            let span = crate::cli::progress::spinner_span(info_span!("Installing", package = %package), &package);
            tasks.spawn(
                async move {
                    let result = mgr.install(&package, platforms, candidate, select).await;
                    (package, result)
                }
                .instrument(span),
            );
        }

        super::common::drain_package_tasks(&packages, tasks, package_manager::error::Error::InstallFailed).await
    }
}
