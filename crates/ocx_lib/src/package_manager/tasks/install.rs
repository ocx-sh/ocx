use std::collections::HashMap;

use tokio::task::JoinSet;

use crate::{
    log, oci,
    package::install_info::InstallInfo,
    package_manager::{self, error::PackageError, error::PackageErrorKind},
    reference_manager::ReferenceManager,
};

use super::super::PackageManager;

impl PackageManager {
    pub async fn install(
        &self,
        package: &oci::Identifier,
        platforms: Vec<oci::Platform>,
        candidate: bool,
        select: bool,
    ) -> crate::Result<InstallInfo> {
        log::debug!("Installing package: {}", package);

        let identifier = self
            .index()
            .select(package, platforms)
            .await?
            .ok_or(crate::Error::PackageNotFound(package.clone()))?;

        log::trace!("Resolved package identifier: {}", &identifier);

        let storage = self.file_structure().objects.path(&identifier)?;
        let install_info = self.client()?.pull_package(identifier.clone(), &storage).await?;

        log::debug!("Package install succeeded for '{}'.", &identifier);

        let rm = ReferenceManager::new(self.file_structure().clone());
        if candidate {
            let link_path = self.file_structure().installs.candidate(package);
            rm.link(&link_path, &install_info.content)?;
        }
        if select {
            let link_path = self.file_structure().installs.current(package);
            rm.link(&link_path, &install_info.content)?;
        }

        Ok(install_info)
    }

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
                .await
                .map_err(|e| {
                    package_manager::error::Error::InstallFailed(vec![PackageError::new(
                        packages[0].clone(),
                        PackageErrorKind::Internal(e),
                    )])
                })?;
            return Ok(vec![info]);
        }

        let mut tasks = JoinSet::new();
        for package in &packages {
            let mgr = self.clone();
            let package = package.clone();
            let platforms = platforms.clone();
            tasks.spawn(async move {
                let result = mgr.install(&package, platforms, candidate, select).await;
                (package, result)
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
            return Err(package_manager::error::Error::InstallFailed(errors));
        }

        Ok(infos)
    }
}
