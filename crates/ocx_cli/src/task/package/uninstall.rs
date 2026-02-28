use ocx_lib::{Error, Result, file_structure, log, oci, reference_manager::ReferenceManager};

pub struct Uninstall {
    pub file_structure: file_structure::FileStructure,
    /// Also remove the `current` symlink.
    pub deselect: bool,
    /// Delete the object directory when no references remain.
    pub purge: bool,
}

impl Uninstall {
    /// Removes the candidate symlink for `package` and optionally the current
    /// symlink (`deselect`) and the backing object directory (`purge`).
    ///
    /// If the candidate symlink is absent the call is a no-op (the package is
    /// already uninstalled), but a warning is emitted so the caller knows no
    /// change was made.
    pub fn uninstall(&self, package: &oci::Identifier) -> Result<()> {
        log::debug!("Uninstalling package '{}'.", package);

        if package.digest().is_some() {
            return Err(Error::PackageSymlinkRequiresTag(package.clone()));
        }

        let rm = ReferenceManager::new(self.file_structure.clone());
        let candidate_path = self.file_structure.installs.candidate(package);

        // Capture the content path and sever the candidate symlink, or warn
        // and skip if the candidate was never installed.
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

        if self.deselect {
            let current_path = self.file_structure.installs.current(package);
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

        if self.purge {
            if let Some(content) = content_path {
                if let Ok(refs_dir) = self.file_structure.objects.refs_dir_for_content(&content) {
                    let is_empty = !refs_dir.is_dir()
                        || std::fs::read_dir(&refs_dir)
                            .map(|mut d| d.next().is_none())
                            .unwrap_or(true);
                    if is_empty {
                        if let Some(obj_dir) = content.parent() {
                            log::info!("Purging unreferenced object: {}", obj_dir.display());
                            std::fs::remove_dir_all(obj_dir)
                                .map_err(|e| Error::InternalFile(obj_dir.to_path_buf(), e))?;
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

    /// Calls [`uninstall`] for each package in order, failing on the first error.
    pub fn uninstall_all(&self, packages: &[oci::Identifier]) -> Result<()> {
        for package in packages {
            self.uninstall(package)?;
        }
        Ok(())
    }
}
