use crate::{
    file_structure::SymlinkKind, log, oci,
    package::{install_info::InstallInfo, metadata},
    package_manager::{self, error::PackageError, error::PackageErrorKind},
    prelude::SerdeExt,
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
    pub async fn find_symlink(
        &self,
        package: &oci::Identifier,
        kind: SymlinkKind,
    ) -> crate::Result<InstallInfo> {
        if package.digest().is_some() {
            return Err(crate::Error::PackageSymlinkRequiresTag(package.clone()));
        }

        if kind == SymlinkKind::Current {
            if let Some(tag) = package.tag() {
                log::warn!("--current ignores the tag '{tag}' of '{package}'");
            }
        }

        let symlink_path = self.file_structure().installs.symlink(package, kind);

        if !symlink_path.exists() {
            return Err(crate::Error::PackageSymlinkNotFound(package.clone(), kind));
        }

        let metadata_path = self
            .file_structure()
            .objects
            .metadata_for_content(&symlink_path)?;
        let metadata = metadata::Metadata::read_json_from_path(&metadata_path)?;

        log::debug!(
            "Resolved '{}' via {:?} symlink at '{}'",
            package,
            kind,
            symlink_path.display()
        );

        Ok(InstallInfo {
            identifier: package.clone(),
            metadata,
            content: symlink_path,
        })
    }

    pub async fn find_symlink_all(
        &self,
        packages: Vec<oci::Identifier>,
        kind: SymlinkKind,
    ) -> Result<Vec<InstallInfo>, package_manager::error::Error> {
        let mut infos = Vec::with_capacity(packages.len());
        let mut errors: Vec<PackageError> = Vec::new();

        for package in &packages {
            match self.find_symlink(package, kind).await {
                Ok(info) => infos.push(info),
                Err(e) => errors.push(PackageError::new(
                    package.clone(),
                    PackageErrorKind::Internal(e),
                )),
            }
        }

        if !errors.is_empty() {
            return Err(package_manager::error::Error::FindFailed(errors));
        }

        Ok(infos)
    }
}
