// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use tokio::task::JoinSet;
use tracing::{Instrument, info_span};

use crate::{
    log, oci,
    package::install_info::InstallInfo,
    package_manager::{self, error::PackageError, error::PackageErrorKind},
};

use super::super::PackageManager;

impl PackageManager {
    /// Finds a package in the object store without index resolution.
    ///
    /// The identifier must carry a digest. Returns the installed package
    /// info if present, or `None` if the object is absent.
    ///
    /// Also serves as defense layer 2 in the concurrent pull safety model —
    /// see [`PackageManager::pull`] for details.
    pub async fn find_plain(
        &self,
        identifier: &oci::PinnedIdentifier,
    ) -> Result<Option<InstallInfo>, PackageErrorKind> {
        super::common::find_in_store(&self.file_structure().packages, identifier).await
    }

    pub async fn find(
        &self,
        package: &oci::Identifier,
        platforms: Vec<oci::Platform>,
    ) -> Result<InstallInfo, PackageErrorKind> {
        log::debug!("Finding package: {}", package);

        let identifier = self.resolve(package, platforms).await?;

        log::debug!("Resolved package identifier: {}", &identifier);

        self.find_plain(&identifier).await?.ok_or_else(|| {
            log::debug!("Package not found locally for '{}'.", identifier);
            PackageErrorKind::NotFound
        })
    }

    pub async fn find_all(
        &self,
        packages: Vec<oci::Identifier>,
        platforms: Vec<oci::Platform>,
    ) -> Result<Vec<InstallInfo>, package_manager::error::Error> {
        if packages.is_empty() {
            return Ok(Vec::new());
        }
        if packages.len() == 1 {
            let info = self
                .find(&packages[0], platforms)
                .instrument(crate::cli::progress::spinner_span(
                    info_span!("Finding", package = %packages[0]),
                    &packages[0],
                ))
                .await
                .map_err(|kind| {
                    package_manager::error::Error::FindFailed(vec![PackageError::new(packages[0].clone(), kind)])
                })?;
            return Ok(vec![info]);
        }

        let mut tasks = JoinSet::new();
        for package in &packages {
            let mgr = self.clone();
            let package = package.clone();
            let platforms = platforms.clone();
            let span = crate::cli::progress::spinner_span(info_span!("Finding", package = %package), &package);
            tasks.spawn(
                async move {
                    let result = mgr.find(&package, platforms).await;
                    (package, result)
                }
                .instrument(span),
            );
        }

        super::common::drain_package_tasks(&packages, tasks, package_manager::error::Error::FindFailed).await
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        file_structure::{self, BlobStore, FileStructure, TagStore},
        oci,
        oci::index::{Index, LocalConfig, LocalIndex},
        package_manager::PackageManager,
    };

    const SHA256_HEX: &str = "aabbccddaabbccddaabbccddaabbccddaabbccddaabbccddaabbccddaabbccdd";
    const VALID_METADATA_JSON: &str = r#"{"type":"bundle","version":1}"#;

    fn valid_resolve_json() -> String {
        r#"{"dependencies":[]}"#.to_string()
    }

    fn test_pinned() -> oci::PinnedIdentifier {
        let id = oci::Identifier::new_registry("test/pkg", "example.com")
            .clone_with_digest(oci::Digest::Sha256(SHA256_HEX.to_string()));
        oci::PinnedIdentifier::try_from(id).unwrap()
    }

    /// Creates a `PackageManager` backed by a temp directory.
    fn setup_manager() -> (tempfile::TempDir, PackageManager, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let fs = FileStructure::with_root(dir.path().to_path_buf());
        let index = Index::from_local(LocalIndex::new(LocalConfig {
            tag_store: TagStore::new(dir.path().join("tags")),
            blob_store: BlobStore::new(dir.path().join("blobs")),
        }));
        let mgr = PackageManager::new(fs.clone(), index, None, "example.com");
        let obj_path = fs.packages.path(&test_pinned());
        (dir, mgr, obj_path)
    }

    #[tokio::test]
    async fn find_plain_present_returns_install_info() {
        let (_dir, mgr, obj_path) = setup_manager();
        let pkg = file_structure::PackageDir { dir: obj_path.clone() };
        std::fs::create_dir_all(pkg.content()).unwrap();
        std::fs::write(pkg.metadata(), VALID_METADATA_JSON).unwrap();
        std::fs::write(pkg.resolve(), valid_resolve_json()).unwrap();

        let result = mgr.find_plain(&test_pinned()).await.unwrap();
        assert!(result.is_some());
        let info = result.unwrap();
        assert_eq!(info.identifier, test_pinned());
        assert_eq!(info.content, pkg.content());
        assert!(info.resolved.dependencies.is_empty());
    }

    #[tokio::test]
    async fn find_plain_absent_no_content_returns_none() {
        let (_dir, mgr, obj_path) = setup_manager();
        let pkg = file_structure::PackageDir { dir: obj_path.clone() };
        std::fs::create_dir_all(&obj_path).unwrap();
        std::fs::write(pkg.metadata(), VALID_METADATA_JSON).unwrap();
        std::fs::write(pkg.resolve(), valid_resolve_json()).unwrap();

        let result = mgr.find_plain(&test_pinned()).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn find_plain_absent_no_metadata_returns_none() {
        let (_dir, mgr, obj_path) = setup_manager();
        let pkg = file_structure::PackageDir { dir: obj_path };
        std::fs::create_dir_all(pkg.content()).unwrap();

        let result = mgr.find_plain(&test_pinned()).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn find_plain_absent_no_resolve_returns_none() {
        let (_dir, mgr, obj_path) = setup_manager();
        let pkg = file_structure::PackageDir { dir: obj_path };
        std::fs::create_dir_all(pkg.content()).unwrap();
        std::fs::write(pkg.metadata(), VALID_METADATA_JSON).unwrap();

        let result = mgr.find_plain(&test_pinned()).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn find_plain_absent_empty_returns_none() {
        let (_dir, mgr, _obj_path) = setup_manager();

        let result = mgr.find_plain(&test_pinned()).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn find_plain_invalid_metadata_returns_error() {
        let (_dir, mgr, obj_path) = setup_manager();
        let pkg = file_structure::PackageDir { dir: obj_path };
        std::fs::create_dir_all(pkg.content()).unwrap();
        std::fs::write(pkg.metadata(), "not valid json").unwrap();
        std::fs::write(pkg.resolve(), valid_resolve_json()).unwrap();

        let result = mgr.find_plain(&test_pinned()).await;
        assert!(result.is_err());
    }
}
