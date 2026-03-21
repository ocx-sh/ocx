// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

use tracing::info_span;

use crate::{
    log, oci,
    package_manager::{self, error::PackageError, error::PackageErrorKind},
    profile::ProfileSnapshot,
    reference_manager::ReferenceManager,
};

use super::super::PackageManager;

impl PackageManager {
    /// Removes the current-version symlink for `package`.
    ///
    /// Returns `Some(current_path)` when the current symlink existed and was
    /// removed, or `None` when no current symlink was present (no-op).
    pub fn deselect(
        &self,
        package: &oci::Identifier,
        profile: &ProfileSnapshot,
    ) -> Result<Option<PathBuf>, PackageErrorKind> {
        let _span =
            crate::cli::progress::spinner_span(info_span!("Deselecting", package = %package), package).entered();
        log::debug!("Deselecting package '{}'.", package);

        if package.digest().is_some() {
            return Err(PackageErrorKind::SymlinkRequiresTag);
        }

        let rm = ReferenceManager::new(self.file_structure().clone());
        let current_path = self.file_structure().installs.current(package);

        if crate::symlink::is_link(&current_path) {
            rm.unlink(&current_path).map_err(PackageErrorKind::Internal)?;
            profile.warn_if_current_referenced(package);
            Ok(Some(current_path))
        } else {
            log::warn!(
                "Package '{}' has no current symlink at '{}' — nothing to deselect.",
                package,
                current_path.display(),
            );
            Ok(None)
        }
    }

    pub fn deselect_all(
        &self,
        packages: &[oci::Identifier],
    ) -> Result<Vec<Option<PathBuf>>, package_manager::error::Error> {
        let profile = self.profile.snapshot();
        let mut results: Vec<Option<PathBuf>> = Vec::with_capacity(packages.len());
        let mut errors: Vec<PackageError> = Vec::new();

        for package in packages {
            match self.deselect(package, &profile) {
                Ok(target) => results.push(target),
                Err(kind) => errors.push(PackageError::new(package.clone(), kind)),
            }
        }

        if !errors.is_empty() {
            return Err(package_manager::error::Error::DeselectFailed(errors));
        }

        Ok(results)
    }
}
