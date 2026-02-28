use ocx_lib::{Error, Result, file_structure, log, oci, package::{install_info, metadata}, prelude::SerdeExt};

pub use ocx_lib::file_structure::SymlinkKind;

use crate::app;

/// Resolves a package's content path via an install symlink rather than the
/// content-addressed object store.
///
/// Both modes require that the relevant symlink was previously established by
/// installing (for `Candidate`) or selecting (for `Current`) the package.
/// Neither mode performs auto-install; if the symlink is absent the call fails
/// with a [`Error::PackageSymlinkNotFound`] error.
///
/// The `content` path in the returned [`install_info::InstallInfo`] is the
/// *symlink path itself* rather than the resolved object-store path.  This is
/// intentional: downstream consumers (env exporters, shell profile builders)
/// embed this path in their output, so it must be stable across package updates.
#[derive(Clone)]
pub struct FindSymlink {
    pub context: app::Context,
    pub file_structure: file_structure::FileStructure,
    pub kind: SymlinkKind,
}

impl FindSymlink {
    pub async fn find(&self, package: &oci::Identifier) -> Result<install_info::InstallInfo> {
        if package.digest().is_some() {
            return Err(Error::PackageSymlinkRequiresTag(package.clone()));
        }

        let symlink_path = self.file_structure.installs.symlink(package, self.kind);

        if !symlink_path.exists() {
            return Err(Error::PackageSymlinkNotFound(package.clone(), self.kind));
        }

        let metadata_path = self.file_structure.objects.metadata_for_content(&symlink_path)?;
        let metadata = metadata::Metadata::read_json_from_path(&metadata_path)?;
        log::debug!("Resolved '{}' via {:?} symlink at '{}'", package, self.kind, symlink_path.display());

        Ok(install_info::InstallInfo {
            identifier: package.clone(),
            metadata,
            content: symlink_path,
        })
    }

    pub async fn find_all(&self, packages: Vec<oci::Identifier>) -> Result<Vec<install_info::InstallInfo>> {
        let mut infos = Vec::with_capacity(packages.len());
        for package in &packages {
            infos.push(self.find(package).await?);
        }
        Ok(infos)
    }
}
