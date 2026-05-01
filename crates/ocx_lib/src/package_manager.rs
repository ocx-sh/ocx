// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

pub mod composer;
pub mod error;
pub mod launcher;

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
            .resolve_env_from_package_root(Path::new("/nonexistent/pkg"), false)
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
        let result = manager.resolve_env_from_package_root(&pkg_root, false).await;
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
        let result = manager.resolve_env_from_package_root(&pkg_root, false).await;
        assert!(result.is_err(), "missing resolve.json must return Err");
    }
}

#[cfg(test)]
mod install_info_identifier_tests {
    use std::path::Path;
    use tempfile::tempdir;

    use crate::{
        file_structure::{BlobStore, FileStructure, TagStore},
        oci::index::{ChainMode, Index, LocalConfig, LocalIndex},
    };

    fn make_test_manager(ocx_home: &Path) -> super::PackageManager {
        let fs = FileStructure::with_root(ocx_home.to_path_buf());
        let local_index = LocalIndex::new(LocalConfig {
            tag_store: TagStore::new(ocx_home.join("tags")),
            blob_store: BlobStore::new(ocx_home.join("blobs")),
        });
        let index = Index::from_chained(local_index, vec![], ChainMode::Offline);
        super::PackageManager::new(fs, index, None, "localhost:5000")
    }

    /// Write the minimal set of files that make `install_info_from_package_root`
    /// succeed for a package rooted at `pkg_root` with the given SHA-256 hex.
    async fn write_minimal_package_root(pkg_root: &std::path::Path, hex: &str) {
        tokio::fs::create_dir_all(pkg_root.join("content")).await.unwrap();
        // metadata.json — bare Bundle with no env vars (passes ValidMetadata).
        let meta = serde_json::json!({"type": "bundle", "version": 1, "env": []});
        tokio::fs::write(pkg_root.join("metadata.json"), meta.to_string().as_bytes())
            .await
            .unwrap();
        // resolve.json — leaf package with no dependencies.
        let resolve = serde_json::json!({"dependencies": []});
        tokio::fs::write(pkg_root.join("resolve.json"), resolve.to_string().as_bytes())
            .await
            .unwrap();
        // digest file written in the same format as `write_digest_file`.
        tokio::fs::write(pkg_root.join("digest"), format!("sha256:{hex}").as_bytes())
            .await
            .unwrap();
    }

    /// Two distinct package roots (as passed to `install_info_from_package_root`)
    /// must produce distinct repository components so they do not collapse onto
    /// the same key in the `seen_repos` dedup map inside `resolve_env`.
    #[tokio::test]
    async fn distinct_package_roots_yield_distinct_identifiers() {
        let tmp = tempdir().unwrap();
        let manager = make_test_manager(tmp.path());

        let root_a = tmp.path().join("pkg_a");
        let root_b = tmp.path().join("pkg_b");
        // Two distinct 64-char SHA-256 hex digests.
        let hex_a = "a".repeat(64);
        let hex_b = "b".repeat(64);

        write_minimal_package_root(&root_a, &hex_a).await;
        write_minimal_package_root(&root_b, &hex_b).await;

        let info_a = manager
            .install_info_from_package_root(&root_a)
            .await
            .expect("package root A must succeed");
        let info_b = manager
            .install_info_from_package_root(&root_b)
            .await
            .expect("package root B must succeed");

        assert_ne!(
            info_a.identifier().repository(),
            info_b.identifier().repository(),
            "distinct package roots must have distinct synthetic repository components"
        );
        assert!(
            info_a.identifier().repository().starts_with("file-url-mode/"),
            "synthetic repository must start with 'file-url-mode/'"
        );
        assert!(
            info_b.identifier().repository().starts_with("file-url-mode/"),
            "synthetic repository must start with 'file-url-mode/'"
        );
    }

    /// Two distinct content digests that share the first 16 hex characters
    /// must still yield DISTINCT synthetic repository components.
    ///
    /// Regression guard: an earlier shape truncated to `digest.hex()[..16]`,
    /// which collapsed any pair of package roots whose digests collided in
    /// their first 64 bits onto the same `(registry, repository)` dedup key
    /// inside `resolve_env`. One side then surfaced as a "conflicting digest"
    /// warning and was silently dropped. Using the full digest hex makes such
    /// a collision probabilistically impossible (full SHA-256 keyspace).
    #[tokio::test]
    async fn pkg_roots_with_same_16char_digest_prefix_do_not_collapse() {
        let tmp = tempdir().unwrap();
        let manager = make_test_manager(tmp.path());

        let root_a = tmp.path().join("pkg_a");
        let root_b = tmp.path().join("pkg_b");
        // Two 64-char SHA-256 hex digests sharing the first 16 hex chars
        // ("aaaaaaaaaaaaaaaa") but diverging from char 17 onwards.
        let prefix = "a".repeat(16);
        let hex_a = format!("{prefix}{}", "0".repeat(48));
        let hex_b = format!("{prefix}{}", "f".repeat(48));
        assert_eq!(&hex_a[..16], &hex_b[..16], "test fixture must share 16-char prefix");
        assert_ne!(hex_a, hex_b, "test fixture must diverge after the prefix");

        write_minimal_package_root(&root_a, &hex_a).await;
        write_minimal_package_root(&root_b, &hex_b).await;

        let info_a = manager
            .install_info_from_package_root(&root_a)
            .await
            .expect("package root A must succeed");
        let info_b = manager
            .install_info_from_package_root(&root_b)
            .await
            .expect("package root B must succeed");

        assert_ne!(
            info_a.identifier().repository(),
            info_b.identifier().repository(),
            "digests sharing the first 16 hex chars must still yield distinct synthetic repositories"
        );
        // Sanity: synthetic repo string must include the full 64-char digest hex,
        // not the truncated 16-char prefix that caused the original collision.
        assert!(
            info_a.identifier().repository().ends_with(&hex_a),
            "synthetic repo for A must embed the full 64-char digest hex"
        );
        assert!(
            info_b.identifier().repository().ends_with(&hex_b),
            "synthetic repo for B must embed the full 64-char digest hex"
        );
    }

    /// Calling `install_info_from_package_root` twice on the same root must
    /// yield the same repository component (idempotency).
    #[tokio::test]
    async fn same_package_root_yields_stable_identifier() {
        let tmp = tempdir().unwrap();
        let manager = make_test_manager(tmp.path());

        let root = tmp.path().join("pkg");
        write_minimal_package_root(&root, &"c".repeat(64)).await;

        let info_first = manager
            .install_info_from_package_root(&root)
            .await
            .expect("first call must succeed");
        let info_second = manager
            .install_info_from_package_root(&root)
            .await
            .expect("second call must succeed");

        assert_eq!(
            info_first.identifier().repository(),
            info_second.identifier().repository(),
            "repeated calls on the same root must produce a stable identifier"
        );
    }
}

// Re-export types needed by other modules and CLI commands.
pub use error::DependencyError;
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
    /// Returns I/O errors from the symlink wire-up. Closure-scoped entrypoint
    /// name collisions are detected at install Stage 1 via
    /// [`composer::check_entrypoints`](crate::package_manager::composer::check_entrypoints),
    /// not here.
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
    /// on-disk **package root** is known. Used by `ocx launcher exec <pkg-root>`
    /// to bypass identifier resolution and read the installed metadata
    /// directly.
    ///
    /// Reads `metadata.json`, `resolve.json`, and the `digest` file from
    /// `pkg_root`. The returned [`InstallInfo`] holds `pkg_root` itself; env
    /// resolution interpolates `${installPath}` against `info.dir().content()`.
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
        // install-symlink chase and the `launcher exec` pkg-root flow.
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
        // Uses the full content digest hex; collision-resistant. The synthetic
        // repository path is internal-only — never persisted in OCI manifests
        // and never compared with real registry repositories — so the longer
        // string is acceptable in exchange for full SHA-256 (~2^256) keyspace,
        // which makes a `(registry, repository)` collision between two distinct
        // pkg-roots probabilistically impossible.
        let digest_path = objects.digest_file_for_content(pkg_root)?;
        let digest = read_digest_file(&digest_path).await?;
        let repo_name = format!("file-url-mode/{}", digest.hex());
        let base_id = crate::oci::Identifier::new_registry(repo_name, &self.default_registry).clone_with_digest(digest);
        let pinned = crate::oci::PinnedIdentifier::try_from(base_id)?;

        Ok(InstallInfo::new(
            pinned,
            metadata,
            resolved,
            crate::file_structure::PackageDir {
                dir: pkg_root.to_path_buf(),
            },
        ))
    }

    /// Resolves environment entries for a package known only by its on-disk
    /// package-root path.
    ///
    /// Convenience wrapper around [`Self::install_info_from_package_root`] +
    /// [`Self::resolve_env`]. Returns the same `Vec<Entry>` shape as
    /// identifier mode so `ocx launcher exec <pkg-root>` has parity with
    /// `ocx exec <identifier>`.
    pub async fn resolve_env_from_package_root(
        &self,
        pkg_root: &std::path::Path,
        self_view: bool,
    ) -> crate::Result<Vec<crate::package::metadata::env::entry::Entry>> {
        let info = self.install_info_from_package_root(pkg_root).await?;
        self.resolve_env(&[std::sync::Arc::new(info)], self_view).await
    }
}
