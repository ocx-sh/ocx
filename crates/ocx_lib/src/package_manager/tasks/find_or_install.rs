// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use tokio::task::JoinSet;

use crate::{
    log, oci,
    package::install_info::InstallInfo,
    package_manager::{self, concurrency::Concurrency, error::PackageError, error::PackageErrorKind},
};

use super::super::PackageManager;

/// How a package reached the store for this invocation.
///
/// An enum rather than a bool because the two states are named domain facts, not
/// a flag: a caller reading `Pulled` learns *why* it matters, which is that this
/// invocation resolved a floating tag and materialized the package on the spot.
/// Together with a `sh.ocx.resolved-from: tag` annotation that is the drift
/// signal an execution record reports as `resolution.autoInstalled` — the one
/// state no pull-time record can capture.
///
/// `Arrival` rather than `Materialization`: this is the per-root **outcome**,
/// and [`composer::Materialization`](crate::package_manager::composer::Materialization)
/// is the **policy** the same call passes in. One name for both, in one call,
/// reads as one concept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arrival {
    /// Already materialized in the package store when this invocation started.
    Cached,
    /// Pulled and materialized during this invocation.
    Pulled,
}

/// One package resolved by [`PackageManager::find_or_install_all`], together
/// with how it got into the store.
#[derive(Debug)]
pub struct FoundPackage {
    /// The resolved package.
    pub info: InstallInfo,
    /// Whether this invocation had to pull it.
    pub arrival: Arrival,
}

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
                    // An offline re-assembly from the local CAS reaches this arm
                    // too. It is still a materialization this invocation
                    // performed, which is what the field states — the record's
                    // own `resolution.offline` says whether a network was
                    // involved.
                    arrival: Arrival::Pulled,
                })
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
    ///
    /// Results are in input order, so a caller needing to know *which*
    /// identifiers were pulled can zip this against the slice it passed in —
    /// which is why the input is borrowed rather than consumed: every such
    /// caller previously cloned the whole vector just to keep it alive.
    pub async fn find_or_install_all(
        &self,
        packages: &[oci::Identifier],
        platform: oci::Platform,
        concurrency: Concurrency,
    ) -> Result<Vec<FoundPackage>, package_manager::error::Error> {
        if packages.is_empty() {
            return Ok(Vec::new());
        }
        if packages.len() == 1 {
            let spin = self.progress().spinner(format!("Resolving '{}'", packages[0]));
            let found = spin
                .scope(self.find_or_install(&packages[0], platform))
                .await
                .map_err(|kind| {
                    package_manager::error::Error::FindFailed(vec![PackageError::new(packages[0].clone(), kind)])
                })?;
            return Ok(vec![found]);
        }

        let semaphore = concurrency.semaphore();
        let mut tasks: JoinSet<(oci::Identifier, Result<FoundPackage, PackageErrorKind>)> = JoinSet::new();

        for package in packages {
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

        super::common::drain_package_tasks(packages, tasks, package_manager::error::Error::FindFailed).await
    }
}
