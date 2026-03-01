use std::collections::HashMap;

use tokio::task::JoinSet;
use tracing::{info_span, Instrument};

use crate::{
    log, oci,
    oci::index::SelectResult,
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
    ) -> Result<InstallInfo, PackageErrorKind> {
        log::debug!("Installing package: {}", package);

        let identifier = match self.index().select(package, platforms).await {
            Ok(SelectResult::Found(id)) => id,
            Ok(SelectResult::Ambiguous(v)) => return Err(PackageErrorKind::SelectionAmbiguous(v)),
            Ok(SelectResult::NotFound) => return Err(PackageErrorKind::NotFound),
            Err(e) => return Err(PackageErrorKind::Internal(e)),
        };

        log::debug!("Resolved package identifier: {}", &identifier);

        let storage = self.file_structure().objects.path(&identifier)
            .map_err(PackageErrorKind::Internal)?;
        let install_info = self.client()
            .map_err(PackageErrorKind::Internal)?
            .pull_package(identifier.clone(), &storage).await
            .map_err(PackageErrorKind::Internal)?;

        log::debug!("Package install succeeded for '{}'.", &identifier);

        let rm = ReferenceManager::new(self.file_structure().clone());
        if candidate {
            let link_path = self.file_structure().installs.candidate(package);
            log::debug!("Creating candidate symlink at '{}'.", link_path.display());
            rm.link(&link_path, &install_info.content).map_err(PackageErrorKind::Internal)?;
        }
        if select {
            let link_path = self.file_structure().installs.current(package);
            log::debug!("Creating current symlink at '{}'.", link_path.display());
            rm.link(&link_path, &install_info.content).map_err(PackageErrorKind::Internal)?;
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
                .instrument(info_span!("Installing", package = %packages[0]))
                .await
                .map_err(|kind| {
                    package_manager::error::Error::InstallFailed(vec![PackageError::new(
                        packages[0].clone(),
                        kind,
                    )])
                })?;
            return Ok(vec![info]);
        }

        let mut tasks = JoinSet::new();
        for package in &packages {
            let mgr = self.clone();
            let package = package.clone();
            let platforms = platforms.clone();
            let span = info_span!("Installing", package = %package);
            tasks.spawn(async move {
                let result = mgr.install(&package, platforms, candidate, select).await;
                (package, result)
            }.instrument(span));
        }

        let mut results: HashMap<oci::Identifier, InstallInfo> =
            HashMap::with_capacity(packages.len());
        let mut errors: Vec<PackageError> = Vec::new();

        while let Some(join_result) = tasks.join_next().await {
            match join_result {
                Ok((id, Ok(info))) => {
                    results.insert(id, info);
                }
                Ok((id, Err(kind))) => {
                    errors.push(PackageError::new(id, kind));
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
