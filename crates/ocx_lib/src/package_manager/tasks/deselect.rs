use std::path::PathBuf;

use tracing::info_span;

use crate::{
    log, oci,
    package_manager::{self, error::PackageError, error::PackageErrorKind},
    reference_manager::ReferenceManager,
};

use super::super::PackageManager;

impl PackageManager {
    /// Removes the current-version symlink for `package`.
    ///
    /// Returns `Some(target_path)` when the current symlink existed and was
    /// removed, or `None` when no current symlink was present (no-op).
    pub fn deselect(&self, package: &oci::Identifier) -> Result<Option<PathBuf>, PackageErrorKind> {
        let _span = info_span!("Deselecting", package = %package).entered();
        log::debug!("Deselecting package '{}'.", package);

        if package.digest().is_some() {
            return Err(PackageErrorKind::SymlinkRequiresTag);
        }

        let rm = ReferenceManager::new(self.file_structure().clone());
        let current_path = self.file_structure().installs.current(package);

        let target = if crate::symlink::is_link(&current_path) {
            let path = std::fs::read_link(&current_path).ok();
            rm.unlink(&current_path).map_err(PackageErrorKind::Internal)?;
            path
        } else {
            log::warn!(
                "Package '{}' has no current symlink at '{}' — nothing to deselect.",
                package,
                current_path.display(),
            );
            None
        };

        Ok(target)
    }

    pub fn deselect_all(
        &self,
        packages: &[oci::Identifier],
    ) -> Result<Vec<Option<PathBuf>>, package_manager::error::Error> {
        let mut results: Vec<Option<PathBuf>> = Vec::with_capacity(packages.len());
        let mut errors: Vec<PackageError> = Vec::new();

        for package in packages {
            match self.deselect(package) {
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
