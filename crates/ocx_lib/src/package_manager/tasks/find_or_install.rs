use std::collections::HashMap;

use tokio::task::JoinSet;

use crate::{
    log, oci,
    package::install_info::InstallInfo,
    package_manager::{self, error::PackageError, error::PackageErrorKind},
};

use super::super::PackageManager;

impl PackageManager {
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

        let mut tasks: JoinSet<(oci::Identifier, crate::Result<InstallInfo>)> = JoinSet::new();

        for package in &packages {
            let mgr = self.clone();
            let pkg = package.clone();
            let plat = platforms.clone();

            tasks.spawn(async move {
                let result = match mgr.find(&pkg, plat.clone()).await {
                    Ok(info) => Ok(info),
                    Err(crate::Error::PackageNotFound(_)) if !mgr.is_offline() => {
                        log::info!("Package '{}' not found locally, installing.", pkg);
                        mgr.install(&pkg, plat, false, false).await
                    }
                    Err(crate::Error::PackageNotFound(_)) => {
                        log::error!(
                            "Package not found and offline mode is enabled: {}",
                            pkg
                        );
                        Err(crate::Error::PackageNotFound(pkg.clone()))
                    }
                    Err(e) => Err(e),
                };
                (pkg, result)
            });
        }

        let mut results: HashMap<oci::Identifier, InstallInfo> =
            HashMap::with_capacity(packages.len());
        let mut errors: Vec<PackageError> = Vec::new();

        while let Some(join_result) = tasks.join_next().await {
            match join_result {
                Ok((id, Ok(info))) => {
                    results.insert(id, info);
                }
                Ok((id, Err(e))) => {
                    errors.push(PackageError::new(id, PackageErrorKind::Internal(e)));
                }
                Err(e) => log::error!("Task panicked: {}", e),
            }
        }

        let mut infos = Vec::with_capacity(packages.len());
        for package in &packages {
            if let Some(info) = results.remove(package) {
                infos.push(info);
            }
        }

        if !errors.is_empty() {
            return Err(package_manager::error::Error::FindFailed(errors));
        }

        Ok(infos)
    }
}
