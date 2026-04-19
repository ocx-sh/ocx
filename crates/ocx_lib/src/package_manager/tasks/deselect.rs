// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

use tracing::info_span;

use crate::{
    log, oci,
    package_manager::{self, error::PackageError, error::PackageErrorKind},
};

use super::super::PackageManager;

impl PackageManager {
    /// Removes the current-version symlink for `package`.
    ///
    /// Returns `Some(current_path)` when the current symlink existed and was
    /// removed, or `None` when no current symlink was present (no-op).
    pub async fn deselect(&self, package: &oci::Identifier) -> Result<Option<PathBuf>, PackageErrorKind> {
        let _span =
            crate::cli::progress::spinner_span(info_span!("Deselecting", package = %package), package).entered();
        log::debug!("Deselecting package '{}'.", package);

        if package.digest().is_some() {
            return Err(PackageErrorKind::SymlinkRequiresTag);
        }

        let rm = super::common::reference_manager(self.file_structure());
        let current_path = self.file_structure().symlinks.current(package);

        // Hold both selection locks (index → select, in that order) for the
        // entire teardown. See tasks/common.rs module docs.
        let _locks = super::common::acquire_selection_locks(self.file_structure(), package).await?;

        // Snapshot the pre-mutation index file. Used to roll back the index
        // ownership clear if the `current` unlink fails downstream — without
        // the snapshot, a failed unlink could leave a launcher name freed in
        // the index while the old `current` symlink still resolved on disk.
        let index_path = self.file_structure().symlinks.entrypoints_index(package.registry());
        let prior_index_bytes: Option<Vec<u8>> = match tokio::fs::read(&index_path).await {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                return Err(PackageErrorKind::Internal(crate::error::file_error(&index_path, e)));
            }
        };

        // Unlink `current` FIRST. Doing this before the index clear means a
        // failed unlink leaves the index still owning the launcher names,
        // matching the still-resolvable on-disk symlink and preserving cross-
        // repo collision detection. The unlink is idempotent when absent.
        let teardown_result: Result<Option<PathBuf>, crate::Error> = (|| {
            if crate::symlink::is_link(&current_path) {
                rm.unlink(&current_path)?;
                Ok(Some(current_path.clone()))
            } else {
                Ok(None)
            }
        })();

        let removed_current = match teardown_result {
            Ok(value) => value,
            Err(e) => {
                super::common::restore_index_snapshot(&index_path, prior_index_bytes.as_deref()).await;
                return Err(PackageErrorKind::Internal(e));
            }
        };

        // Symlink teardown succeeded — now clear ownership in the index. If
        // the index write fails, restore the snapshot before surfacing the
        // error so deselect remains all-or-nothing on the index file.
        if let Err(e) = super::common::clear_index_owner(self.file_structure(), package).await {
            super::common::restore_index_snapshot(&index_path, prior_index_bytes.as_deref()).await;
            return Err(e);
        }

        if removed_current.is_none() {
            log::warn!(
                "Package '{}' has no current symlink at '{}' — nothing to deselect.",
                package,
                current_path.display(),
            );
        }

        Ok(removed_current)
    }

    pub async fn deselect_all(
        &self,
        packages: &[oci::Identifier],
    ) -> Result<Vec<Option<PathBuf>>, package_manager::error::Error> {
        let mut results: Vec<Option<PathBuf>> = Vec::with_capacity(packages.len());
        let mut errors: Vec<PackageError> = Vec::new();

        for package in packages {
            match self.deselect(package).await {
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
