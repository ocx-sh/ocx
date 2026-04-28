// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::{
    log, oci,
    package::install_info::InstallInfo,
    package_manager::{self, concurrency::Concurrency, error::PackageError, error::PackageErrorKind},
};

use super::super::PackageManager;

impl PackageManager {
    /// Downloads a package and creates install symlinks.
    ///
    /// Delegates to [`PackageManager::pull`] for the actual download and
    /// transitive dependency resolution (see that method for concurrency
    /// safety), then optionally creates:
    ///
    /// - A **candidate** symlink at `symlinks/{repo}/candidates/{tag}` when
    ///   `candidate` is `true` — pins this version as an installed candidate.
    /// - A **current** symlink at `symlinks/{repo}/current` when `select` is
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

        create_install_symlinks(self, package, &install_info, candidate, select)?;

        Ok(install_info)
    }

    /// Installs multiple packages in parallel using a shared singleflight
    /// group for cross-package diamond dependency deduplication.
    ///
    /// Phase 1: [`pull_all`](PackageManager::pull_all) downloads all packages
    /// and their transitive deps with a shared singleflight group.
    /// Phase 2: Install symlinks are created sequentially (cheap I/O, no
    /// contention benefit from parallelism).
    pub async fn install_all(
        &self,
        packages: Vec<oci::Identifier>,
        platforms: Vec<oci::Platform>,
        candidate: bool,
        select: bool,
        concurrency: Concurrency,
    ) -> Result<Vec<InstallInfo>, package_manager::error::Error> {
        // Phase 1: Pull all packages with shared singleflight group.
        let infos = self.pull_all(&packages, platforms, concurrency).await?;

        // Phase 2: Create symlinks sequentially.
        if candidate || select {
            for (pkg, info) in packages.iter().zip(infos.iter()) {
                create_install_symlinks(self, pkg, info, candidate, select).map_err(|kind| {
                    package_manager::error::Error::InstallFailed(vec![PackageError::new(pkg.clone(), kind)])
                })?;
            }
        }

        Ok(infos)
    }
}

/// Creates candidate and/or current symlinks for a single package.
fn create_install_symlinks(
    mgr: &PackageManager,
    package: &oci::Identifier,
    info: &InstallInfo,
    candidate: bool,
    select: bool,
) -> Result<(), PackageErrorKind> {
    let rm = super::common::reference_manager(mgr.file_structure());
    if candidate {
        let link_path = mgr.file_structure().symlinks.candidate(package);
        log::debug!("Creating candidate symlink at '{}'.", link_path.display());
        rm.link(&link_path, &info.content).map_err(PackageErrorKind::Internal)?;
    }
    if select {
        let link_path = mgr.file_structure().symlinks.current(package);
        log::debug!("Creating current symlink at '{}'.", link_path.display());
        rm.link(&link_path, &info.content).map_err(PackageErrorKind::Internal)?;
    }
    Ok(())
}
