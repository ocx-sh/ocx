use std::collections::HashMap;

use ocx_lib::{
    Error, file_structure, log, oci,
    package::{install_info, metadata},
    prelude::SerdeExt,
};
use tokio::task::JoinSet;

use crate::app;

#[derive(Clone)]
pub struct Find {
    pub context: app::Context,
    pub file_structure: file_structure::FileStructure,
    pub platforms: Vec<oci::Platform>,
}

impl Find {
    pub async fn find(self, package: &oci::Identifier) -> ocx_lib::Result<install_info::InstallInfo> {
        log::debug!("Finding package: {}", &package);
        let identifier = self
            .context
            .default_index()
            .select(package, self.platforms)
            .await?
            .ok_or(Error::PackageNotFound(package.clone()))?;
        log::debug!("Resolved package identifier: {}", &identifier);
        let content = self.file_structure.objects.content(&identifier)?;
        if !content.exists() {
            return Err(Error::PackageNotFound(identifier.clone()));
        }
        let metadata = self.file_structure.objects.metadata(&identifier)?;
        let metadata = metadata::Metadata::read_json_from_path(metadata)?;
        Ok(install_info::InstallInfo {
            identifier,
            metadata,
            content,
        })
    }

    pub async fn find_all(self, packages: Vec<oci::Identifier>) -> ocx_lib::Result<Vec<install_info::InstallInfo>> {
        if packages.is_empty() {
            log::debug!("No packages to find.");
            return Ok(Vec::new());
        } else if packages.len() == 1 {
            return Ok(vec![self.find(packages.into_iter().nth(0).as_ref().unwrap()).await?]);
        }

        let mut tasks = JoinSet::new();

        for package in &packages {
            let task = self.clone();
            let package = package.clone();
            tasks.spawn(async move {
                let install_info = task.find(&package).await; 
                (package, install_info)
            });
        }

        let mut install_infos = HashMap::with_capacity(packages.len());
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok((identifier, Ok(install_info))) => {
                    install_infos.insert(identifier, install_info);
                }
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
