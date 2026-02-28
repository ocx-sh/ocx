use ocx_lib::{file_structure, log, oci, package::{install_info, metadata}, prelude::SerdeExt};

pub use ocx_lib::file_structure::SymlinkKind;

use crate::app;

/// Resolves a package's content path via an install symlink rather than the
/// content-addressed object store.
///
/// Both modes require that the relevant symlink was previously established by
/// `ocx install` (for `Candidate`) or `ocx install --select` (for `Current`).
/// Neither mode performs auto-install; if the symlink is absent the call fails
/// with an actionable error message.
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
    pub async fn find(&self, package: &oci::Identifier) -> anyhow::Result<install_info::InstallInfo> {
        if package.digest().is_some() {
            anyhow::bail!(
                "Cannot use --candidate/--current with a digest identifier ('{package}').\n\
                 Digest identifiers address content directly; omit the flag to use the object store path,\n\
                 or provide a tag-only identifier to use the symlink path."
            );
        }

        let symlink_path = self.file_structure.install_symlink(package, self.kind);

        if !symlink_path.exists() {
            match self.kind {
                SymlinkKind::Candidate => anyhow::bail!(
                    "Package '{package}' has not been installed as a candidate.\n\
                     Run `ocx install {package}` first."
                ),
                SymlinkKind::Current => anyhow::bail!(
                    "Package '{package}' has no current selection.\n\
                     Run `ocx install --select {package}` to select a version."
                ),
            }
        }

        // The symlink points to the content directory inside the object store.
        // Follow it once to locate the sibling metadata.json, but keep the
        // symlink path itself as the content root so that resolved env paths
        // remain stable.
        let resolved_content = std::fs::canonicalize(&symlink_path)
            .map_err(|e| anyhow::anyhow!("Failed to resolve symlink '{}': {}", symlink_path.display(), e))?;

        let metadata_path = resolved_content
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Unexpected path structure for '{}'", resolved_content.display()))?
            .join("metadata.json");

        let metadata = metadata::Metadata::read_json_from_path(&metadata_path)
            .map_err(|e| anyhow::anyhow!("Failed to read metadata from '{}': {}", metadata_path.display(), e))?;

        log::debug!(
            "Resolved '{}' via {:?} symlink: {} → {}",
            package,
            self.kind,
            symlink_path.display(),
            resolved_content.display(),
        );

        Ok(install_info::InstallInfo {
            identifier: package.clone(),
            metadata,
            content: symlink_path,
        })
    }

    pub async fn find_all(&self, packages: Vec<oci::Identifier>) -> anyhow::Result<Vec<install_info::InstallInfo>> {
        let mut infos = Vec::with_capacity(packages.len());
        for package in &packages {
            infos.push(self.find(package).await?);
        }
        Ok(infos)
    }
}
