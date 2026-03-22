// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::{HashMap, HashSet};

use tokio::task::JoinSet;
use tracing::{Instrument, info_span};

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

        let storage = self
            .file_structure()
            .objects
            .path(&identifier)
            .map_err(PackageErrorKind::Internal)?;
        let temp_path = self
            .file_structure()
            .temp
            .path(&identifier)
            .map_err(PackageErrorKind::Internal)?;

        let client = self.client().map_err(PackageErrorKind::Internal)?;
        let acquire = match self
            .file_structure()
            .temp
            .try_acquire(&temp_path)
            .map_err(PackageErrorKind::Internal)?
        {
            Some(r) => r,
            None => {
                log::debug!("Temp dir locked by another process, waiting: {}", temp_path.display());
                self.file_structure()
                    .temp
                    .acquire_with_timeout(&temp_path, client.lock_timeout())
                    .await
                    .map_err(PackageErrorKind::Internal)?
            }
        };
        if acquire.was_cleaned {
            log::debug!("Cleaned previous temp data at {}", temp_path.display());
        }

        let install_info = client
            .pull_package(identifier.clone(), &storage, acquire)
            .await
            .map_err(PackageErrorKind::Internal)?;

        log::debug!("Package install succeeded for '{}'.", &identifier);

        let rm = ReferenceManager::new(self.file_structure().clone());
        if candidate {
            let link_path = self.file_structure().installs.candidate(package);
            log::debug!("Creating candidate symlink at '{}'.", link_path.display());
            rm.link(&link_path, &install_info.content)
                .map_err(PackageErrorKind::Internal)?;
        }
        if select {
            let link_path = self.file_structure().installs.current(package);
            log::debug!("Creating current symlink at '{}'.", link_path.display());
            rm.link(&link_path, &install_info.content)
                .map_err(PackageErrorKind::Internal)?;
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
                .instrument(crate::cli::progress::spinner_span(
                    info_span!("Installing", package = %packages[0]),
                    &packages[0],
                ))
                .await
                .map_err(|kind| {
                    package_manager::error::Error::InstallFailed(vec![PackageError::new(packages[0].clone(), kind)])
                })?;
            return Ok(vec![info]);
        }

        let mut tasks = JoinSet::new();
        for package in &packages {
            let mgr = self.clone();
            let package = package.clone();
            let platforms = platforms.clone();
            let span = crate::cli::progress::spinner_span(info_span!("Installing", package = %package), &package);
            tasks.spawn(
                async move {
                    let result = mgr.install(&package, platforms, candidate, select).await;
                    (package, result)
                }
                .instrument(span),
            );
        }

        let mut pending: HashSet<oci::Identifier> = packages.iter().cloned().collect();
        let mut results: HashMap<oci::Identifier, InstallInfo> = HashMap::with_capacity(packages.len());
        let mut errors: Vec<PackageError> = Vec::new();

        while let Some(join_result) = tasks.join_next().await {
            match join_result {
                Ok((id, Ok(info))) => {
                    pending.remove(&id);
                    results.insert(id, info);
                }
                Ok((id, Err(kind))) => {
                    pending.remove(&id);
                    errors.push(PackageError::new(id, kind));
                }
                Err(e) => log::error!("Task panicked: {}", e),
            }
        }

        for id in pending {
            errors.push(PackageError::new(id, PackageErrorKind::TaskPanicked));
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
