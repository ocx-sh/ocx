// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::{
    oci,
    package::install_info::InstallInfo,
    package_manager::{self, error::PackageError},
};

use super::super::PackageManager;
use super::common::WireSelectionOutcome;

impl PackageManager {
    /// Selects (sets the `current` symlink for) multiple packages, preserving
    /// input order.
    ///
    /// Resolution runs in parallel inside [`find_all`](PackageManager::find_all)
    /// (resolution failures surface as `FindFailed`); the selection wire-up is
    /// then a sequential loop that aggregates every per-package failure into a
    /// single [`SelectFailed`](package_manager::error::Error::SelectFailed)
    /// instead of aborting on the first, so the CLI can report all offenders
    /// with identifier context. Input order holds by construction.
    ///
    /// Returns each resolved [`InstallInfo`] paired with its
    /// [`WireSelectionOutcome`] so the caller can build its report.
    #[allow(clippy::result_large_err)]
    pub async fn select_all(
        &self,
        packages: Vec<oci::Identifier>,
        platform: oci::Platform,
    ) -> Result<Vec<(InstallInfo, WireSelectionOutcome)>, package_manager::error::Error> {
        let infos = self.find_all(packages.clone(), platform).await?;

        // ponytail: sequential wire-up matches the deselect_all / uninstall_all
        // precedent — the expensive resolve already ran in parallel inside
        // find_all; this loop is cheap fs mutation. Parallel upgrade path if it
        // ever matters: install.rs `install_all` Phase 2 (index-tagged JoinSet).
        let mut results: Vec<(InstallInfo, WireSelectionOutcome)> = Vec::with_capacity(infos.len());
        let mut errors: Vec<PackageError> = Vec::new();

        for (package, info) in packages.iter().zip(infos) {
            match super::common::wire_selection(self.file_structure(), package, &info, false, true).await {
                Ok(outcome) => results.push((info, outcome)),
                Err(kind) => errors.push(PackageError::new(package.clone(), kind)),
            }
        }

        if !errors.is_empty() {
            return Err(package_manager::error::Error::SelectFailed(errors));
        }

        Ok(results)
    }
}
