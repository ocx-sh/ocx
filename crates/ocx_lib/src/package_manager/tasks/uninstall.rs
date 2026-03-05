use std::path::PathBuf;

use tracing::info_span;

use crate::{
    log, oci,
    package_manager::{self, error::PackageError, error::PackageErrorKind},
    reference_manager::ReferenceManager,
};

use super::super::PackageManager;

/// Result of a successful uninstall where the candidate symlink existed.
pub struct UninstallResult {
    /// The candidate symlink path that was removed.
    pub candidate: PathBuf,
    /// The object directory that was purged, if purging was requested and the
    /// object had no remaining references.  `None` when purge was not requested
    /// or the object still has references.
    pub purged: Option<PathBuf>,
}

impl PackageManager {
    /// Removes the candidate symlink for `package` and optionally the current
    /// symlink (`deselect`) and the backing object directory (`purge`).
    ///
    /// Returns `Some(UninstallResult)` when the candidate symlink existed and
    /// was removed, or `None` when no candidate was present (no-op).
    pub fn uninstall(
        &self,
        package: &oci::Identifier,
        deselect: bool,
        purge: bool,
    ) -> Result<Option<UninstallResult>, PackageErrorKind> {
        let _span = info_span!("Uninstalling", package = %package).entered();
        log::debug!("Uninstalling package '{}'.", package);

        if package.digest().is_some() {
            return Err(PackageErrorKind::SymlinkRequiresTag);
        }

        let rm = ReferenceManager::new(self.file_structure().clone());
        let candidate_path = self.file_structure().installs.candidate(package);

        let content_path = if crate::symlink::is_link(&candidate_path) {
            let path = std::fs::read_link(&candidate_path).ok();
            log::trace!("Candidate content path: {:?}", path);
            rm.unlink(&candidate_path).map_err(PackageErrorKind::Internal)?;
            path
        } else {
            log::warn!(
                "Package '{}' has no installed candidate at '{}' — nothing to uninstall.",
                package,
                candidate_path.display(),
            );
            return Ok(None);
        };

        if deselect {
            let current_path = self.file_structure().installs.current(package);
            if crate::symlink::is_link(&current_path) {
                rm.unlink(&current_path).map_err(PackageErrorKind::Internal)?;
            } else {
                log::debug!(
                    "Package '{}' has no current symlink at '{}' — skipping deselect.",
                    package,
                    current_path.display(),
                );
            }
        }

        let mut purged = None;
        if purge
            && let Some(ref content) = content_path
            && let Ok(refs_dir) = self.file_structure().objects.refs_dir_for_content(content)
        {
            let is_empty = !refs_dir.is_dir()
                || std::fs::read_dir(&refs_dir)
                    .map(|mut d| d.next().is_none())
                    .unwrap_or(true);
            if is_empty {
                if let Some(obj_dir) = content.parent() {
                    log::info!("Purging unreferenced object: {}", obj_dir.display());
                    std::fs::remove_dir_all(obj_dir).map_err(|e| {
                        PackageErrorKind::Internal(crate::Error::InternalFile(obj_dir.to_path_buf(), e))
                    })?;
                    purged = Some(obj_dir.to_path_buf());
                }
            } else {
                log::debug!(
                    "Object at '{}' still has references — skipping purge.",
                    content.display(),
                );
            }
        }

        Ok(Some(UninstallResult {
            candidate: candidate_path,
            purged,
        }))
    }

    pub fn uninstall_all(
        &self,
        packages: &[oci::Identifier],
        deselect: bool,
        purge: bool,
    ) -> Result<Vec<Option<UninstallResult>>, package_manager::error::Error> {
        let mut results: Vec<Option<UninstallResult>> = Vec::with_capacity(packages.len());
        let mut errors: Vec<PackageError> = Vec::new();

        for package in packages {
            match self.uninstall(package, deselect, purge) {
                Ok(result) => results.push(result),
                Err(kind) => errors.push(PackageError::new(package.clone(), kind)),
            }
        }

        if !errors.is_empty() {
            return Err(package_manager::error::Error::UninstallFailed(errors));
        }

        Ok(results)
    }
}
