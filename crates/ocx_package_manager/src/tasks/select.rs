// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::error::PackageError;
use ocx_package::install_info::InstallInfo;

use super::super::PackageManager;
use super::common::WireSelectionOutcome;

impl PackageManager {
    /// Selects (sets the `current` symlink for) multiple packages, preserving input order.
    ///
    /// Resolution failures surface as `FindFailed`; wire-up failures are aggregated into one
    /// [`SelectFailed`](crate::error::Error::SelectFailed) instead of aborting.
    #[allow(clippy::result_large_err)]
    pub async fn select_all(
        &self,
        packages: Vec<ocx_oci::PackageRef>,
        platform: ocx_oci::Platform,
    ) -> Result<Vec<(InstallInfo, WireSelectionOutcome)>, crate::error::Error> {
        let infos = self.find_all(packages.clone(), platform).await?;

        // ponytail: sequential cheap fs wire-up after the parallel find_all; parallelise
        // like install.rs `install_all` Phase 2 if it ever matters.
        let mut results: Vec<(InstallInfo, WireSelectionOutcome)> = Vec::with_capacity(infos.len());
        let mut errors: Vec<PackageError> = Vec::new();

        for (package, info) in packages.iter().zip(infos) {
            match super::common::wire_selection(self.file_structure(), package, &info, false, true).await {
                Ok(outcome) => results.push((info, outcome)),
                Err(kind) => errors.push(PackageError::new(package.clone(), kind)),
            }
        }

        if !errors.is_empty() {
            return Err(crate::error::Error::SelectFailed(errors));
        }

        Ok(results)
    }
}
