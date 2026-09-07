// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use tokio::task::JoinSet;

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

    /// Locates a package, resolving through the index only when locating it
    /// actually requires resolution.
    ///
    /// A digest-addressed identifier that is already in the store is answered
    /// without resolving anything: the package directory is pure path
    /// arithmetic over the digest, so a manifest walk cannot change the answer,
    /// and the transport address the walk would derive routes a download this
    /// call has just established it is not going to make. That walk ended in a
    /// live `GET /p/<ns>/<pkg>.json` per package whenever the local index held
    /// no committed root — which is the permanent state of any machine that
    /// only ever runs `pull` and `exec` against a committed lock, because a
    /// digest-addressed resolve never grows one. Issue #424: a rendered
    /// trampoline paid it on every invocation, once per locked tool.
    ///
    /// The four cases, in full:
    ///
    /// - **pinned, in the store** — answered here, zero network.
    /// - **pinned, absent from the store** — falls through; the resolve and,
    ///   through [`find_or_install`](Self::find_or_install), the pull happen
    ///   exactly as before. First use still works.
    /// - **unpinned (tag)** — falls through unconditionally: a tag has to be
    ///   resolved before anything can be located.
    /// - **a digest that is not a platform leaf** (an image-index digest) —
    ///   misses the store, which is keyed on the leaf, and falls through.
    ///
    /// The two stamps the resolve path applies are preserved rather than
    /// dropped, so a root reached this way and a root reached through the
    /// resolve describe the same artefact identically:
    ///
    /// - the platform is [`oci::Platform::any()`], which is not a guess — a
    ///   store hit proves the digest is a platform leaf, a leaf manifest is a
    ///   flat image manifest, and [`resolve`](Self::resolve)'s flat arm stamps
    ///   `any()` unconditionally;
    /// - the transport registry comes from the locally committed index root
    ///   when there is one, and is left unset when there is not — the state
    ///   [`InstallInfo::transport_registry`] already documents for a path that
    ///   resolved nothing through the index. Naming the *logical* host instead
    ///   would report a registry nothing was ever fetched from.
    ///
    /// The `refs/blobs/` upsert is likewise skipped: it heals a legacy or
    /// alt-tag install's resolution chain, and this path has no chain to write.
    /// The `--no-pull` probe (`composer::local_root`) already answers from the
    /// store without it.
    pub async fn find(
        &self,
        package: &oci::Identifier,
        platform: oci::Platform,
    ) -> Result<InstallInfo, PackageErrorKind> {
        log::debug!("Finding package: {}", package);

        if let Ok(pinned) = oci::PinnedIdentifier::try_from(package.clone())
            && let Some(info) = self.find_plain(&pinned).await?
        {
            log::debug!("Found package in store without resolving: {}", pinned);
            let info = info.with_platform(oci::Platform::any());
            // A guard refusal propagates; only a genuine absence is a miss.
            return Ok(
                match self
                    .index()
                    .physical_reference_local(pinned.as_identifier())
                    .await
                    .map_err(PackageErrorKind::Internal)?
                {
                    Some(physical) => info.with_transport_registry(physical.registry()),
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
        // Stamp what the resolution learned — the selected platform and the
        // registry the content is fetched from — exactly as the pull path does
        // in `setup_owned`. Without it the same artefact describes itself
        // differently depending on whether this invocation happened to find it
        // in the store or had to fetch it: a cached find would report no
        // platform and no content registry at all.
        let info = info
            .with_platform(resolved.platform.clone())
            .with_transport_registry(resolved.transport_pinned.registry());

        // Upsert the resolution chain into the installed package's `refs/blobs/`
        // — idempotent, covers legacy installs and alt-tag resolves that walked
        // through a different image index than the one originally pulled.
        super::common::reference_manager(self.file_structure())
            .link_blobs(&info.dir().content(), resolved.blobs())
            .await
            .map_err(PackageErrorKind::Internal)?;

        Ok(info)
    }

    pub async fn find_all(
        &self,
        packages: Vec<oci::Identifier>,
        platform: oci::Platform,
    ) -> Result<Vec<InstallInfo>, package_manager::error::Error> {
        if packages.is_empty() {
            return Ok(Vec::new());
        }
        if packages.len() == 1 {
            let _spin = self.progress().spinner(format!("Finding '{}'", packages[0]));
            let info = self.find(&packages[0], platform).await.map_err(|kind| {
                package_manager::error::Error::FindFailed(vec![PackageError::new(packages[0].clone(), kind)])
            })?;
            return Ok(vec![info]);
        }

        let mut tasks = JoinSet::new();
        for package in &packages {
            let mgr = self.clone();
            let package = package.clone();
            let platform = platform.clone();
            tasks.spawn(async move {
                let _spin = mgr.progress().spinner(format!("Finding '{package}'"));
                let result = mgr.find(&package, platform).await;
                (package, result)
            });
        }

        super::common::drain_package_tasks(&packages, tasks, package_manager::error::Error::FindFailed).await
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        file_structure::{self, FileStructure, IndexStore},
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
        let index = Index::from_chained(
            LocalIndex::new(LocalConfig {
                index_store: IndexStore::new(dir.path().join("index")),
            }),
            Vec::new(),
            crate::oci::index::ChainMode::Offline,
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
