// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::{HashMap, HashSet};

use tokio::task::JoinSet;
use tracing::{Instrument, info_span};

use crate::{
    log, oci,
    oci::index::SelectResult,
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
    ) -> Result<InstallInfo, PackageErrorKind> {
        log::debug!("Finding package: {}", package);

        let identifier = match self.index().select(package, platforms).await {
            Ok(SelectResult::Found(id)) => id,
            Ok(SelectResult::Ambiguous(v)) => return Err(PackageErrorKind::SelectionAmbiguous(v)),
            Ok(SelectResult::NotFound) => return Err(PackageErrorKind::NotFound),
            Err(e) => return Err(PackageErrorKind::Internal(e)),
        };

        log::debug!("Resolved package identifier: {}", &identifier);

        let content = self
            .file_structure()
            .objects
            .content(&identifier)
            .map_err(PackageErrorKind::Internal)?;
        if !content.exists() {
            log::debug!("Content directory not found locally for '{}'.", identifier);
            return Err(PackageErrorKind::NotFound);
        }

        let metadata_path = self
            .file_structure()
            .objects
            .metadata(&identifier)
            .map_err(PackageErrorKind::Internal)?;
        let metadata = metadata::Metadata::read_json_from_path(metadata_path).map_err(PackageErrorKind::Internal)?;

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
                .instrument(info_span!("Finding", package = %packages[0]))
                .await
                .map_err(|kind| {
                    package_manager::error::Error::FindFailed(vec![PackageError::new(packages[0].clone(), kind)])
                })?;
            return Ok(vec![info]);
        }

        let mut tasks = JoinSet::new();
        for package in &packages {
            let mgr = self.clone();
            let package = package.clone();
            let platforms = platforms.clone();
            let span = info_span!("Finding", package = %package);
            tasks.spawn(
                async move {
                    let result = mgr.find(&package, platforms).await;
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
            errors.push(PackageError::new(
                id,
                PackageErrorKind::Internal(crate::Error::UndefinedWithMessage("task panicked unexpectedly".into())),
            ));
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
