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
    /// Finds a package locally; if absent and the manager is online, installs it.
    async fn find_or_install(
        &self,
        package: &oci::Identifier,
        platforms: Vec<oci::Platform>,
    ) -> Result<InstallInfo, PackageErrorKind> {
        match self.find(package, platforms.clone()).await {
            Ok(info) => Ok(info),
            Err(PackageErrorKind::NotFound) if !self.is_offline() => {
                log::info!("Package '{}' not found locally, installing.", package);
                self.install(package, platforms, false, false).await
            }
            Err(PackageErrorKind::NotFound) => {
                log::error!("Package not found and offline mode is enabled: {}", package);
                Err(PackageErrorKind::NotFound)
            }
            Err(e) => Err(e),
        }
    }

    /// Finds each package locally and, when a package is absent and the manager
    /// is online, installs it automatically.
    pub async fn find_or_install_all(
        &self,
        packages: Vec<oci::Identifier>,
        platforms: Vec<oci::Platform>,
    ) -> Result<Vec<InstallInfo>, package_manager::error::Error> {
        if packages.is_empty() {
            return Ok(Vec::new());
        }
        if packages.len() == 1 {
            let info = self
                .find_or_install(&packages[0], platforms)
                .instrument(crate::cli::progress::spinner_span(
                    info_span!("Resolving", package = %packages[0]),
                    &packages[0],
                ))
                .await
                .map_err(|kind| {
                    package_manager::error::Error::FindFailed(vec![PackageError::new(packages[0].clone(), kind)])
                })?;
            return Ok(vec![info]);
        }

        let mut tasks: JoinSet<(oci::Identifier, Result<InstallInfo, PackageErrorKind>)> = JoinSet::new();

        for package in &packages {
            let mgr = self.clone();
            let pkg = package.clone();
            let plat = platforms.clone();

            let span = crate::cli::progress::spinner_span(info_span!("Resolving", package = %pkg), &pkg);
            tasks.spawn(
                async move {
                    let result = mgr.find_or_install(&pkg, plat).await;
                    (pkg, result)
                }
                .instrument(span),
            );
        }

        super::common::drain_package_tasks(&packages, tasks, package_manager::error::Error::FindFailed).await
    }
}
