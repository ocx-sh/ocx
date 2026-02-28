use std::collections::HashMap;

use ocx_lib::{
    Error, file_structure, log, oci, package::install_info, reference_manager::ReferenceManager,
};
use tokio::task::JoinSet;

use crate::app;

#[derive(Clone)]
pub struct Install {
    pub context: app::Context,
    pub file_structure: file_structure::FileStructure,
    pub platforms: Vec<oci::Platform>,
    pub candidate: bool,
    pub select: bool,
}

impl Install {
    pub async fn install(self, package: &oci::Identifier) -> ocx_lib::Result<install_info::InstallInfo> {
        log::debug!("Installing package: {}", &package);
        let identifier = self
            .context
            .default_index()
            .select(package, self.platforms)
            .await?
            .ok_or(Error::PackageNotFound(package.clone()))?;
        log::trace!("Resolved package identifier: {}", &identifier);
        let storage = self.file_structure.objects.path(&identifier)?;
        let install_info = self.context.remote_client()?.pull_package(identifier.clone(), &storage).await?;
        log::debug!("Package install succeeded for '{}'.", &identifier);

        let rm = ReferenceManager::new(self.file_structure.clone());
        if self.candidate {
            let link_path = self.file_structure.installs.candidate(package);
            rm.link(&link_path, &install_info.content)?;
        }
        if self.select {
            let link_path = self.file_structure.installs.current(package);
            rm.link(&link_path, &install_info.content)?;
        }

        Ok(install_info)
    }

    pub async fn install_all(
        self,
        packages: Vec<oci::Identifier>,
    ) -> ocx_lib::Result<Vec<install_info::InstallInfo>> {
        if packages.is_empty() {
            log::debug!("No packages to install.");
            return Ok(Vec::new());
        } else if packages.len() == 1 {
            return Ok(vec![self.install(packages.into_iter().nth(0).as_ref().unwrap()).await?]);
        }
        
        let mut tasks = JoinSet::new();
        for package in &packages {
            let task = self.clone();
            let package = package.clone();
            tasks.spawn(async move { 
                let install_info = task.install(&package).await;
                (package, install_info)
             });
        }

        let mut install_infos = HashMap::with_capacity(packages.len());
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok((identifier, Ok(install_info))) => {
                    install_infos.insert(identifier, install_info);
                },
                Ok((identifier, Err(e))) => log::error!("Error installing package '{}': {}", identifier, e),
                Err(e) => log::error!("Task panicked: {}", e),
            };
        }

        let mut sorted_install_infos = Vec::with_capacity(packages.len());
        let mut missing_packages = Vec::new();
        for package in packages {
            if let Some(install_info) = install_infos.remove(&package) {
                sorted_install_infos.push(install_info);
            } else {
                log::error!("Failed to install package '{}'", package);
                missing_packages.push(package);
            }
        }

        if !missing_packages.is_empty() {
            return Err(Error::PackageInstallFailed(missing_packages));
        }
        Ok(sorted_install_infos)
    }
}
