// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use tokio::task::JoinSet;

use crate::{error::PackageError, error::PackageErrorKind};
use ocx_package::install_info::InstallInfo;

use super::super::PackageManager;

impl PackageManager {
    /// Finds a package in the object store without index resolution; `None` if absent.
    pub async fn find_plain(
        &self,
        identifier: &ocx_oci::PinnedPackageRef,
    ) -> Result<Option<InstallInfo>, PackageErrorKind> {
        super::common::find_in_store(&self.file_structure().packages, identifier).await
    }

    /// Locates a package; a pinned digest already in the store is answered with no network.
    pub async fn find(
        &self,
        package: &ocx_oci::PackageRef,
        platform: ocx_oci::Platform,
    ) -> Result<InstallInfo, PackageErrorKind> {
        log::debug!("Finding package: {}", package);

        if let Ok(pinned) = ocx_oci::PinnedPackageRef::try_from(package.clone())
            && let Some(info) = self.find_plain(&pinned).await?
        {
            log::debug!("Found package in store without resolving: {}", pinned);
            // `any()`: a store hit proves a flat platform leaf, which `resolve`'s flat arm also stamps `any()`.
            let info = info.with_platform(ocx_oci::Platform::any());
            // A guard refusal propagates; only a genuine absence is a miss.
            return Ok(
                match self
                    .index()
                    .route_local(pinned.as_identifier())
                    .await
                    .map_err(|error| PackageErrorKind::Internal(error.into()))?
                {
                    // Stamp route_local's registry, not the logical host, or this reports a fetch that never happened.
                    Some(routed) => info.with_transport_registry(routed.registry()),
                    None => info,
                },
            );
        }

        let resolved = self.resolve(package, platform).await?;
        let identifier = resolved.pinned.clone();

        log::debug!("Resolved package identifier: {}", &identifier);

        let info = self.find_plain(&identifier).await?.ok_or_else(|| {
            log::debug!("Package not found locally for '{}'.", identifier);
            PackageErrorKind::NotFound
        })?;
        // Stamp as `setup_owned` does, or a cached find reports no platform and no registry.
        let info = info.with_platform(resolved.platform.clone());
        let info = match &resolved.transport_pinned {
            Ok(transport) => info.with_transport_registry(transport.registry()),
            Err(_) => info,
        };

        // Idempotent upsert: heals legacy installs and alt-tag resolves through a different image index.
        super::common::reference_manager(self.file_structure())
            .link_blobs(&info.dir().content(), resolved.blobs())
            .await
            .map_err(|error| PackageErrorKind::Internal(error.into()))?;

        Ok(info)
    }

    pub async fn find_all(
        &self,
        packages: Vec<ocx_oci::PackageRef>,
        platform: ocx_oci::Platform,
    ) -> Result<Vec<InstallInfo>, crate::error::Error> {
        if packages.is_empty() {
            return Ok(Vec::new());
        }
        if packages.len() == 1 {
            let info = self
                .find(&packages[0], platform)
                .await
                .map_err(|kind| crate::error::Error::FindFailed(vec![PackageError::new(packages[0].clone(), kind)]))?;
            return Ok(vec![info]);
        }

        let mut tasks = JoinSet::new();
        for package in &packages {
            let mgr = self.clone();
            let package = package.clone();
            let platform = platform.clone();
            tasks.spawn(async move {
                let result = mgr.find(&package, platform).await;
                (package, result)
            });
        }

        super::common::drain_package_tasks(&packages, tasks, crate::error::Error::FindFailed).await
    }
}

#[cfg(test)]
mod tests {
    use crate::PackageManager;
    use ocx_index::{Index, IndexStore, LocalConfig, LocalIndex};
    use ocx_store::{file_structure, file_structure::FileStructure};

    const SHA256_HEX: &str = "aabbccddaabbccddaabbccddaabbccddaabbccddaabbccddaabbccddaabbccdd";
    const VALID_METADATA_JSON: &str = r#"{"type":"bundle","version":1}"#;

    fn valid_resolve_json() -> String {
        r#"{"dependencies":[]}"#.to_string()
    }

    fn test_pinned() -> ocx_oci::PinnedPackageRef {
        let id = ocx_oci::PackageRef::new_registry("test/pkg", "example.com")
            .clone_with_digest(ocx_oci::Digest::Sha256(SHA256_HEX.to_string()));
        ocx_oci::PinnedPackageRef::try_from(id).unwrap()
    }

    /// Creates a `PackageManager` backed by a temp directory.
    fn setup_manager() -> (tempfile::TempDir, PackageManager, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let fs = FileStructure::with_root(dir.path().to_path_buf());
        let index = Index::from_chained(
            LocalIndex::new(LocalConfig {
                index_store: IndexStore::new(dir.path().join("index")),
            }),
            Vec::new(),
            ocx_index::ChainMode::Offline,
        );
        let mgr = PackageManager::new(fs.clone(), index, None, "example.com");
        let obj_path = fs.packages.path(&test_pinned());
        (dir, mgr, obj_path)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn find_plain_present_returns_install_info() {
        let (_dir, mgr, obj_path) = setup_manager();
        let pkg = file_structure::PackageDir { dir: obj_path.clone() };
        std::fs::create_dir_all(pkg.content()).unwrap();
        std::fs::write(pkg.metadata(), VALID_METADATA_JSON).unwrap();
        std::fs::write(pkg.resolve(), valid_resolve_json()).unwrap();

        let result = mgr.find_plain(&test_pinned()).await.unwrap();
        assert!(result.is_some());
        let info = result.unwrap();
        assert_eq!(info.identifier(), &test_pinned());
        assert_eq!(info.dir().content(), pkg.content());
        assert!(info.resolved().dependencies.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn find_plain_absent_no_content_returns_none() {
        let (_dir, mgr, obj_path) = setup_manager();
        let pkg = file_structure::PackageDir { dir: obj_path.clone() };
        std::fs::create_dir_all(&obj_path).unwrap();
        std::fs::write(pkg.metadata(), VALID_METADATA_JSON).unwrap();
        std::fs::write(pkg.resolve(), valid_resolve_json()).unwrap();

        let result = mgr.find_plain(&test_pinned()).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn find_plain_absent_no_metadata_returns_none() {
        let (_dir, mgr, obj_path) = setup_manager();
        let pkg = file_structure::PackageDir { dir: obj_path };
        std::fs::create_dir_all(pkg.content()).unwrap();

        let result = mgr.find_plain(&test_pinned()).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn find_plain_absent_no_resolve_returns_none() {
        let (_dir, mgr, obj_path) = setup_manager();
        let pkg = file_structure::PackageDir { dir: obj_path };
        std::fs::create_dir_all(pkg.content()).unwrap();
        std::fs::write(pkg.metadata(), VALID_METADATA_JSON).unwrap();

        let result = mgr.find_plain(&test_pinned()).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn find_plain_absent_empty_returns_none() {
        let (_dir, mgr, _obj_path) = setup_manager();

        let result = mgr.find_plain(&test_pinned()).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
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
