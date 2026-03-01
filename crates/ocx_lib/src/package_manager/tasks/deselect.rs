use tracing::info_span;

use crate::{
    log, oci,
    package_manager::{self, error::PackageError, error::PackageErrorKind},
    reference_manager::ReferenceManager,
};

use super::super::PackageManager;

impl PackageManager {
    pub fn deselect(
        &self,
        package: &oci::Identifier,
    ) -> Result<(), PackageErrorKind> {
        let _span = info_span!("Deselecting", package = %package).entered();
        log::debug!("Deselecting package '{}'.", package);

        if package.digest().is_some() {
            return Err(PackageErrorKind::SymlinkRequiresTag);
        }

        let rm = ReferenceManager::new(self.file_structure().clone());
        let current_path = self.file_structure().installs.current(package);

        if crate::symlink::is_link(&current_path) {
            rm.unlink(&current_path).map_err(PackageErrorKind::Internal)?;
        } else {
            log::warn!(
                "Package '{}' has no current symlink at '{}' — nothing to deselect.",
                package,
                current_path.display(),
            );
        }

        Ok(())
    }

    pub fn deselect_all(
        &self,
        packages: &[oci::Identifier],
    ) -> Result<(), package_manager::error::Error> {
        let mut errors: Vec<PackageError> = Vec::new();

        for package in packages {
            if let Err(kind) = self.deselect(package) {
                errors.push(PackageError::new(package.clone(), kind));
            }
        }

        if !errors.is_empty() {
            return Err(package_manager::error::Error::DeselectFailed(errors));
        }

        Ok(())
    }
}
