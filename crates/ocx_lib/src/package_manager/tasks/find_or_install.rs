// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use tokio::task::JoinSet;

use crate::{
    log, oci,
    package::install_info::InstallInfo,
    package_manager::{self, concurrency::Concurrency, error::PackageError, error::PackageErrorKind},
};

use super::super::PackageManager;

impl PackageManager {
    /// Finds a package locally; if absent, falls through to [`pull`].
    ///
    /// In offline mode, `pull` no longer requires network when the manifest,
    /// metadata config blob, and every layer are already in the local CAS —
    /// see the offline-safe paths in `setup_owned` and `extract_layer_atomic`.
    /// This lets `--offline exec` re-assemble a package whose `packages/`
    /// tree was deleted but whose `blobs/` and `layers/` are still present.
    /// When any cached input is missing, `pull` surfaces the underlying
    /// `OfflineMode` error and the caller sees a clear failure.
    async fn find_or_install(
        &self,
        package: &oci::Identifier,
        platform: oci::Platform,
    ) -> Result<InstallInfo, PackageErrorKind> {
        match self.find(package, platform.clone()).await {
            Ok(info) => Ok(info),
            Err(PackageErrorKind::NotFound) => {
                if self.is_offline() {
                    log::info!(
                        "Package '{}' not found in package store; attempting offline re-assembly from cache.",
                        package
                    );
                } else {
                    log::info!("Package '{}' not found locally, pulling.", package);
                }
                self.pull(package, platform).await
            }
            Err(e) => Err(e),
        }
    }

    /// Finds each package locally and, when a package is absent and the manager
    /// is online, installs it automatically.
    ///
    /// `concurrency` caps the outer dispatch in the multi-package case (matches
    /// [`pull_all`](PackageManager::pull_all) semantics). Single-package fast
    /// path is naturally serial and ignores the cap.
    pub async fn find_or_install_all(
        &self,
        packages: Vec<oci::Identifier>,
        platform: oci::Platform,
        concurrency: Concurrency,
    ) -> Result<Vec<InstallInfo>, package_manager::error::Error> {
        if packages.is_empty() {
            return Ok(Vec::new());
        }
        if packages.len() == 1 {
            let spin = self.progress().spinner(format!("Resolving '{}'", packages[0]));
            let info = spin
                .scope(self.find_or_install(&packages[0], platform))
                .await
                .map_err(|kind| {
                    package_manager::error::Error::FindFailed(vec![PackageError::new(packages[0].clone(), kind)])
                })?;
            return Ok(vec![info]);
        }

        let semaphore = concurrency.semaphore();
        let mut tasks: JoinSet<(oci::Identifier, Result<InstallInfo, PackageErrorKind>)> = JoinSet::new();

        for package in &packages {
            let mgr = self.clone();
            let pkg = package.clone();
            let plat = platform.clone();
            let sem = semaphore.clone();

            tasks.spawn(async move {
                let _permit = super::super::concurrency::acquire_permit(&sem).await;
                let spin = mgr.progress().spinner(format!("Resolving '{pkg}'"));
                let result = spin.scope(mgr.find_or_install(&pkg, plat)).await;
                (pkg, result)
            });
        }

        super::common::drain_package_tasks(&packages, tasks, package_manager::error::Error::FindFailed).await
    }
}
