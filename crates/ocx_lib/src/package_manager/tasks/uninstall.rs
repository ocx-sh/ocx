use crate::{
    log, oci,
    package_manager::{self, error::PackageError, error::PackageErrorKind},
    reference_manager::ReferenceManager,
};

use super::super::PackageManager;

impl PackageManager {
    /// Removes the candidate symlink for `package` and optionally the current
    /// symlink (`deselect`) and the backing object directory (`purge`).
    pub fn uninstall(
        &self,
        package: &oci::Identifier,
        deselect: bool,
        purge: bool,
    ) -> crate::Result<()> {
        log::debug!("Uninstalling package '{}'.", package);

        if package.digest().is_some() {
            return Err(crate::Error::PackageSymlinkRequiresTag(package.clone()));
        }

        let rm = ReferenceManager::new(self.file_structure().clone());
        let candidate_path = self.file_structure().installs.candidate(package);

        let content_path = if candidate_path.is_symlink() {
            let path = std::fs::read_link(&candidate_path).ok();
            log::trace!("Candidate content path: {:?}", path);
            rm.unlink(&candidate_path)?;
            path
        } else {
            log::warn!(
                "Package '{}' has no installed candidate at '{}' — nothing to uninstall.",
                package,
                candidate_path.display(),
            );
            None
        };

        if deselect {
            let current_path = self.file_structure().installs.current(package);
            if current_path.is_symlink() {
                rm.unlink(&current_path)?;
            } else {
                log::debug!(
                    "Package '{}' has no current symlink at '{}' — skipping deselect.",
                    package,
                    current_path.display(),
                );
            }
        }

        if purge {
            if let Some(content) = content_path {
                if let Ok(refs_dir) = self.file_structure().objects.refs_dir_for_content(&content) {
                    let is_empty = !refs_dir.is_dir()
                        || std::fs::read_dir(&refs_dir)
                            .map(|mut d| d.next().is_none())
                            .unwrap_or(true);
                    if is_empty {
                        if let Some(obj_dir) = content.parent() {
                            log::info!("Purging unreferenced object: {}", obj_dir.display());
                            std::fs::remove_dir_all(obj_dir).map_err(|e| {
                                crate::Error::InternalFile(obj_dir.to_path_buf(), e)
                            })?;
                        }
                    } else {
                        log::debug!(
                            "Object at '{}' still has references — skipping purge.",
                            content.display(),
                        );
                    }
                }
            } else {
                log::debug!("No content path captured — skipping purge (candidate was absent).");
            }
        }

        Ok(())
    }

    pub fn uninstall_all(
        &self,
        packages: &[oci::Identifier],
        deselect: bool,
        purge: bool,
    ) -> Result<(), package_manager::error::Error> {
        let mut errors: Vec<PackageError> = Vec::new();

        for package in packages {
            if let Err(e) = self.uninstall(package, deselect, purge) {
                errors.push(PackageError::new(
                    package.clone(),
                    PackageErrorKind::Internal(e),
                ));
            }
        }

        if !errors.is_empty() {
            return Err(package_manager::error::Error::UninstallFailed(errors));
        }

        Ok(())
    }
}
