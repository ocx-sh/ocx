// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

pub mod entrypoints;
pub mod error;
pub mod package_ref;

mod tasks;

#[cfg(test)]
mod resolve_env_package_root_tests {
    use std::path::Path;
    use tempfile::tempdir;

    use crate::{
        file_structure::FileStructure,
        file_structure::{BlobStore, TagStore},
        oci::index::{ChainMode, Index, LocalConfig, LocalIndex},
    };

    /// Helper: construct a minimal offline PackageManager for unit testing.
    fn make_test_manager(ocx_home: &Path) -> super::PackageManager {
        let fs = FileStructure::with_root(ocx_home.to_path_buf());
        let local_index = LocalIndex::new(LocalConfig {
            tag_store: TagStore::new(ocx_home.join("tags")),
            blob_store: BlobStore::new(ocx_home.join("blobs")),
        });
        let index = Index::from_chained(local_index, vec![], ChainMode::Offline);
        super::PackageManager::new(
            fs,
            index,
            None, // offline — no OCI client
            "localhost:5000",
        )
    }

    #[tokio::test]
    async fn resolve_env_from_missing_package_root_errors() {
        let tmp = tempdir().unwrap();
        let manager = make_test_manager(tmp.path());
        // Non-existent package root — PackageStore cannot read metadata.json.
        let result = manager
            .resolve_env_from_package_root(Path::new("/nonexistent/pkg"))
            .await;
        assert!(result.is_err(), "missing package root must return Err");
    }

    #[tokio::test]
    async fn resolve_env_from_package_root_with_missing_metadata_errors() {
        let tmp = tempdir().unwrap();
        let pkg_root = tmp.path().join("pkg");
        // Package root exists, but no metadata.json — must error.
        tokio::fs::create_dir_all(&pkg_root).await.unwrap();
        tokio::fs::create_dir_all(pkg_root.join("content")).await.unwrap();
        let manager = make_test_manager(tmp.path());
        let result = manager.resolve_env_from_package_root(&pkg_root).await;
        assert!(result.is_err(), "missing metadata.json must return Err");
    }

    #[tokio::test]
    async fn resolve_env_from_package_root_with_missing_resolve_json_errors() {
        let tmp = tempdir().unwrap();
        let pkg_root = tmp.path().join("pkg");
        tokio::fs::create_dir_all(pkg_root.join("content")).await.unwrap();
        // Write metadata.json but no resolve.json.
        let meta = serde_json::json!({
            "type": "bundle",
            "version": 1,
            "env": [{"key": "PATH", "type": "path", "required": true, "value": "${installPath}/bin"}]
        });
        tokio::fs::write(pkg_root.join("metadata.json"), meta.to_string().as_bytes())
            .await
            .unwrap();
        let manager = make_test_manager(tmp.path());
        let result = manager.resolve_env_from_package_root(&pkg_root).await;
        assert!(result.is_err(), "missing resolve.json must return Err");
    }
}

// Re-export types needed by other modules and CLI commands.
pub use error::DependencyError;
pub use package_ref::{PackageRef, PackageRefParseError};
pub use tasks::common::WireSelectionOutcome;
pub use tasks::profile_resolve::{ProfileEntryResolution, ResolvedProfileEntry};

use crate::{file_structure, oci, profile::ProfileManager};

/// Central facade for package operations (find, install, uninstall, etc.).
///
/// `PackageManager` holds all the context that tasks need — file structure,
/// index, OCI client — and is cheap to [`Clone`].
///
/// Environment variable resolution uses persisted `resolve.json` files
/// written at install time — see [`resolve_env`](Self::resolve_env).
///
/// Progress reporting is handled via `tracing` spans emitted by task code;
/// the CLI wires up `tracing-indicatif` (or similar) to visualize them.
#[derive(Clone)]
pub struct PackageManager {
    file_structure: file_structure::FileStructure,
    index: oci::index::Index,
    client: Option<oci::Client>,
    default_registry: String,
    profile: ProfileManager,
}

impl PackageManager {
    pub fn new(
        file_structure: file_structure::FileStructure,
        index: oci::index::Index,
        client: Option<oci::Client>,
        default_registry: impl Into<String>,
    ) -> Self {
        let default_registry = default_registry.into();
        let profile = ProfileManager::new(file_structure.clone());
        Self {
            file_structure,
            index,
            client,
            default_registry,
            profile,
        }
    }

    pub fn file_structure(&self) -> &file_structure::FileStructure {
        &self.file_structure
    }

    pub fn index(&self) -> &oci::index::Index {
        &self.index
    }

    /// Returns the OCI client, or `Err(OfflineMode)` if none is available.
    pub fn client(&self) -> crate::Result<&oci::Client> {
        self.client.as_ref().ok_or(crate::Error::OfflineMode)
    }

    pub fn default_registry(&self) -> &str {
        &self.default_registry
    }

    pub fn profile(&self) -> &ProfileManager {
        &self.profile
    }

    pub fn is_offline(&self) -> bool {
        self.client.is_none()
    }

    /// Wires the per-repo `current` selection symlink for `package`.
    ///
    /// Thin facade over [`tasks::common::wire_selection`]. Used by both
    /// `install --select` (via `tasks::install`) and the `select` CLI command
    /// so collision detection, lock acquisition, and the per-registry
    /// entry-points index update share a single implementation. The symlink
    /// targets the package root; consumers traverse `<current>/content/`,
    /// `<current>/entrypoints/`, or `<current>/metadata.json`.
    ///
    /// # Errors
    ///
    /// Returns [`crate::package_manager::error::PackageErrorKind::EntrypointNameCollision`]
    /// when `package`'s entrypoint names overlap with launcher names already
    /// owned by a different `(registry, repository)` in the per-registry
    /// `entrypoints-index.json`.
    #[allow(clippy::result_large_err)]
    pub async fn wire_selection(
        &self,
        package: &oci::Identifier,
        info: &crate::package::install_info::InstallInfo,
        candidate: bool,
        select: bool,
    ) -> Result<WireSelectionOutcome, error::PackageErrorKind> {
        tasks::common::wire_selection(self.file_structure(), package, info, candidate, select).await
    }

    /// Builds an [`InstallInfo`] for an already-installed package whose
    /// on-disk **package root** is known. Used by `ocx exec file://<pkg-root>`
    /// to bypass identifier resolution and read the installed metadata
    /// directly.
    ///
    /// Reads `metadata.json`, `resolve.json`, and the `digest` file from
    /// `pkg_root`. Sets `info.content` to `<pkg_root>/content` because
    /// downstream env resolution interpolates `${installPath}` against the
    /// content tree.
    ///
    /// # Errors
    ///
    /// Returns an error if `pkg_root` does not exist or any of the required
    /// files are missing or malformed.
    pub async fn install_info_from_package_root(
        &self,
        pkg_root: &std::path::Path,
    ) -> crate::Result<crate::package::install_info::InstallInfo> {
        use crate::file_structure::read_digest_file;
        use crate::package::install_info::InstallInfo;
        use crate::package::metadata::ValidMetadata;
        use crate::package::resolved_package::ResolvedPackage;
        use crate::prelude::SerdeExt;

        let objects = &self.file_structure.packages;

        // `*_for_content` accepts either a `content/` path or a package root
        // (see `package_dir_for_content`), so the same helpers serve both the
        // legacy install-symlink chase and the new file:// pkg-root flow.
        let metadata_path = objects.metadata_for_content(pkg_root)?;
        let resolve_path = objects.resolve_for_content(pkg_root)?;
        let (metadata_result, resolved_result) = tokio::join!(
            crate::package::metadata::Metadata::read_json(&metadata_path),
            ResolvedPackage::read_json(&resolve_path),
        );
        // Centralize publish-time validation at consumption: every on-disk
        // metadata blob must clear `ValidMetadata::try_from` before flowing
        // into env resolution (mirrors `tasks/common.rs::load_object_data`).
        // Catches stale or tampered metadata that predates current rules.
        let metadata: crate::package::metadata::Metadata = ValidMetadata::try_from(metadata_result?)?.into();
        let resolved = resolved_result?;

        // Reconstruct a PinnedIdentifier from the sibling `digest` file.
        // The identifier is used only for dedup tracking in resolve_env.
        let digest_path = objects.digest_file_for_content(pkg_root)?;
        let digest = read_digest_file(&digest_path).await?;
        let base_id =
            crate::oci::Identifier::new_registry("file-url-mode", &self.default_registry).clone_with_digest(digest);
        let pinned = crate::oci::PinnedIdentifier::try_from(base_id)?;

        Ok(InstallInfo {
            identifier: pinned,
            metadata,
            resolved,
            content: pkg_root.join("content"),
        })
    }

    /// Resolves environment entries for a package known only by its on-disk
    /// package-root path.
    ///
    /// Convenience wrapper around [`Self::install_info_from_package_root`] +
    /// [`Self::resolve_env`]. Returns the same `Vec<Entry>` shape as
    /// identifier mode so `ocx exec file://<pkg-root>` has parity with
    /// `ocx exec <identifier>`.
    pub async fn resolve_env_from_package_root(
        &self,
        pkg_root: &std::path::Path,
    ) -> crate::Result<Vec<crate::package::metadata::env::exporter::Entry>> {
        let info = self.install_info_from_package_root(pkg_root).await?;
        self.resolve_env(&[info]).await
    }
}
