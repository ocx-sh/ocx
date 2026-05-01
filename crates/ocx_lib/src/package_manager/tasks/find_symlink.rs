// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use tracing::info_span;

use crate::{
    file_structure::{PackageDir, SymlinkKind},
    log, oci,
    package::install_info::InstallInfo,
    package_manager::{self, error::PackageError, error::PackageErrorKind},
    utility,
};

use super::super::PackageManager;

impl PackageManager {
    /// Resolves a package's content path via an install symlink rather than the
    /// content-addressed object store.
    ///
    /// The `content` path in the returned [`InstallInfo`] is the *symlink path
    /// itself* rather than the resolved object-store path.  This is intentional:
    /// downstream consumers embed this path in their output so it must be stable
    /// across package updates.
    ///
    /// Unlike the resolver-backed tasks (`find`, `install`, etc.), this path does
    /// not walk the OCI resolution chain and therefore does not upsert entries
    /// into `refs/blobs/`. No resolution happens — the install symlink points
    /// directly at an already-installed package whose `refs/blobs/` was populated
    /// at install time.
    pub async fn find_symlink(
        &self,
        package: &oci::Identifier,
        kind: SymlinkKind,
    ) -> Result<InstallInfo, PackageErrorKind> {
        log::debug!("Finding {:?} symlink for '{}'.", kind, package);

        if package.digest().is_some() {
            return Err(PackageErrorKind::SymlinkRequiresTag);
        }

        if kind == SymlinkKind::Current
            && let Some(tag) = package.tag()
        {
            log::warn!("--current ignores the tag '{tag}' of '{package}'");
        }

        let symlink_path = self.file_structure().symlinks.symlink(package, kind);

        if !utility::fs::path_exists_lossy(&symlink_path).await {
            return Err(PackageErrorKind::SymlinkNotFound(kind));
        }

        let packages = &self.file_structure().packages;
        let (metadata, resolved) = super::common::load_object_data(packages, &symlink_path)
            .await
            .map_err(PackageErrorKind::Internal)?;
        let identifier = super::common::identifier_for_symlink(packages, &symlink_path, package, kind)
            .await
            .map_err(PackageErrorKind::Internal)?;

        log::debug!(
            "Resolved '{}' via {:?} symlink at '{}'",
            package,
            kind,
            symlink_path.display()
        );

        // Install symlinks target the package root (post-flatten layout). The
        // env-resolution layer derives `${installPath}` from
        // `info.dir().content()` so traversal stays stable through the symlink
        // while landing in the right subdir.
        let dir = PackageDir {
            dir: symlink_path.to_path_buf(),
        };

        Ok(InstallInfo::new(identifier, metadata, resolved, dir))
    }

    pub async fn find_symlink_all(
        &self,
        packages: Vec<oci::Identifier>,
        kind: SymlinkKind,
    ) -> Result<Vec<InstallInfo>, package_manager::error::Error> {
        let mut infos = Vec::with_capacity(packages.len());
        let mut errors: Vec<PackageError> = Vec::new();

        for package in &packages {
            let _span =
                crate::cli::progress::spinner_span(info_span!("Resolving", package = %package), package).entered();
            match self.find_symlink(package, kind).await {
                Ok(info) => infos.push(info),
                Err(kind) => errors.push(PackageError::new(package.clone(), kind)),
            }
        }

        if !errors.is_empty() {
            return Err(package_manager::error::Error::FindFailed(errors));
        }

        Ok(infos)
    }
}
