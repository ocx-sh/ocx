use std::collections::HashMap;

use ocx_lib::{Error, file_structure, log, oci, package::install_info};
use tokio::task::JoinSet;

use crate::app;

use super::{find, install};

/// Finds each package locally and, when a package is absent and the context is online,
/// installs it automatically.
///
/// One task is spawned per package. Each task performs the full find→install chain so that
/// installs for quickly-missing packages start concurrently with finds still in-flight.
///
/// Reuses [`find::Find`] and [`install::Install`] directly; no logic is duplicated.
#[derive(Clone)]
pub struct FindOrInstall {
    pub context: app::Context,
    pub file_structure: file_structure::FileStructure,
    pub platforms: Vec<oci::Platform>,
}

impl FindOrInstall {
    pub async fn find_or_install_all(
        self,
        packages: Vec<oci::Identifier>,
    ) -> ocx_lib::Result<Vec<install_info::InstallInfo>> {
        if packages.is_empty() {
            log::debug!("No packages to find or install.");
            return Ok(Vec::new());
        }

        let mut tasks: JoinSet<(oci::Identifier, ocx_lib::Result<install_info::InstallInfo>)> =
            JoinSet::new();

        for package in &packages {
            let find = find::Find {
                context: self.context.clone(),
                file_structure: self.file_structure.clone(),
                platforms: self.platforms.clone(),
            };
            let install = install::Install {
                context: self.context.clone(),
                file_structure: self.file_structure.clone(),
                platforms: self.platforms.clone(),
                candidate: false,
                select: false,
            };
            let offline = self.context.is_offline();
            let pkg = package.clone();

            tasks.spawn(async move {
                let result = match find.find(&pkg).await {
                    Ok(info) => Ok(info),
                    Err(Error::PackageNotFound(_)) if !offline => {
                        log::info!("Package '{}' not found locally, installing.", pkg);
                        install.install(&pkg).await
                    }
                    Err(Error::PackageNotFound(_)) => {
                        log::error!("Package not found and offline mode is enabled: {}", pkg);
                        Err(Error::PackageNotFound(pkg.clone()))
                    }
                    Err(e) => Err(e),
                };
                (pkg, result)
            });
        }

        let mut results: HashMap<oci::Identifier, install_info::InstallInfo> =
            HashMap::with_capacity(packages.len());
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok((identifier, Ok(info))) => {
                    results.insert(identifier, info);
                }
                Ok((identifier, Err(e))) => {
                    log::error!("Failed to find or install package '{}': {}", identifier, e);
                }
                Err(e) => log::error!("Task panicked: {}", e),
            }
        }

        let mut sorted_infos = Vec::with_capacity(packages.len());
        let mut missing_packages = Vec::new();
        for package in packages {
            if let Some(info) = results.remove(&package) {
                sorted_infos.push(info);
            } else {
                missing_packages.push(package);
            }
        }

        if !missing_packages.is_empty() {
            return Err(Error::PackageInstallFailed(missing_packages));
        }
        Ok(sorted_infos)
    }
}
