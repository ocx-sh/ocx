// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use tokio::task::JoinSet;

use crate::{concurrency::Concurrency, error::PackageError, error::PackageErrorKind};
use ocx_package::install_info::InstallInfo;

use super::super::PackageManager;

/// How a package reached the store for this invocation; `Pulled` surfaces as `resolution.autoInstalled`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arrival {
    /// Already materialized in the package store when this invocation started.
    Cached,
    /// Pulled and materialized during this invocation.
    Pulled,
}

/// One package resolved by [`PackageManager::find_or_install_all`], with how it got into the store.
#[derive(Debug)]
pub struct FoundPackage {
    pub info: InstallInfo,
    pub arrival: Arrival,
}

impl PackageManager {
    /// Finds a package locally; if absent, falls through to [`pull`], which offline re-assembles from the local CAS.
    async fn find_or_install(
        &self,
        package: &ocx_oci::PackageRef,
        platform: ocx_oci::Platform,
    ) -> Result<FoundPackage, PackageErrorKind> {
        match self.find(package, platform.clone()).await {
            Ok(info) => Ok(FoundPackage {
                info,
                arrival: Arrival::Cached,
            }),
            Err(PackageErrorKind::NotFound) => {
                if self.is_offline() {
                    log::info!(
                        "Package '{}' not found in package store; attempting offline re-assembly from cache.",
                        package
                    );
                } else {
                    log::info!("Package '{}' not found locally, pulling.", package);
                }
                self.pull(package, platform).await.map(|info| FoundPackage {
                    info,
                    // An offline re-assembly is still a materialization; `resolution.offline` records the network.
                    arrival: Arrival::Pulled,
                })
            }
            Err(e) => Err(e),
        }
    }

    /// Finds each package locally, installing absent ones; results are in input order.
    pub async fn find_or_install_all(
        &self,
        packages: &[ocx_oci::PackageRef],
        platform: ocx_oci::Platform,
        concurrency: Concurrency,
    ) -> Result<Vec<FoundPackage>, crate::error::Error> {
        if packages.is_empty() {
            return Ok(Vec::new());
        }
        if packages.len() == 1 {
            let found = self
                .find_or_install(&packages[0], platform)
                .await
                .map_err(|kind| crate::error::Error::FindFailed(vec![PackageError::new(packages[0].clone(), kind)]))?;
            return Ok(vec![found]);
        }

        let semaphore = concurrency.semaphore();
        let mut tasks: JoinSet<(ocx_oci::PackageRef, Result<FoundPackage, PackageErrorKind>)> = JoinSet::new();

        for package in packages {
            let mgr = self.clone();
            let pkg = package.clone();
            let plat = platform.clone();
            let sem = semaphore.clone();

            tasks.spawn(async move {
                let _permit = super::super::concurrency::acquire_permit(&sem).await;
                let result = mgr.find_or_install(&pkg, plat).await;
                (pkg, result)
            });
        }

        super::common::drain_package_tasks(packages, tasks, crate::error::Error::FindFailed).await
    }
}
