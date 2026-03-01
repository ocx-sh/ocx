use std::collections::HashMap;

use tokio::task::JoinSet;

use crate::{
    log, oci,
    package::{install_info::InstallInfo, metadata},
    package_manager::{self, error::PackageError, error::PackageErrorKind},
    prelude::SerdeExt,
};

use super::super::PackageManager;

impl PackageManager {
    pub async fn find(
        &self,
        package: &oci::Identifier,
        platforms: Vec<oci::Platform>,
    ) -> crate::Result<InstallInfo> {
        log::debug!("Finding package: {}", package);

        let identifier = self
            .index()
            .select(package, platforms)
            .await?
            .ok_or(crate::Error::PackageNotFound(package.clone()))?;

        log::debug!("Resolved package identifier: {}", &identifier);

        let content = self.file_structure().objects.content(&identifier)?;
        if !content.exists() {
            return Err(crate::Error::PackageNotFound(identifier.clone()));
        }

        let metadata_path = self.file_structure().objects.metadata(&identifier)?;
        let metadata = metadata::Metadata::read_json_from_path(metadata_path)?;

        Ok(InstallInfo {
            identifier,
            metadata,
            content,
        })
    }

    pub async fn find_all(
        &self,
        packages: Vec<oci::Identifier>,
        platforms: Vec<oci::Platform>,
    ) -> Result<Vec<InstallInfo>, package_manager::error::Error> {
        if packages.is_empty() {
            return Ok(Vec::new());
        }
        if packages.len() == 1 {
            let info = self
                .find(&packages[0], platforms)
                .await
                .map_err(|e| {
                    package_manager::error::Error::FindFailed(vec![PackageError::new(
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
                let result = mgr.find(&package, platforms).await;
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

        // Preserve input order.
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
