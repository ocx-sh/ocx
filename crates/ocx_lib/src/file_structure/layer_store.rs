// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::{Path, PathBuf};

use crate::{Result, log, oci};

/// Represents a single content-addressed layer directory within the layer store.
///
/// A layer directory has a fixed layout:
/// - `content/` -- the extracted layer files (directory tree)
/// - `digest`   -- full digest string for recovery
pub struct LayerDir {
    /// The root directory of this layer (parent of `content/`, `digest`).
    pub dir: PathBuf,
}

impl LayerDir {
    /// Path to the extracted layer content directory.
    pub fn content(&self) -> PathBuf {
        self.dir.join("content")
    }

    /// Path to the digest marker file.
    pub fn digest_file(&self) -> PathBuf {
        self.dir.join(super::cas_path::DIGEST_FILENAME)
    }
}

/// Manages the content-addressed layer store on the local filesystem.
///
/// All layers are stored under a single `root` directory, sharded by
/// registry and digest (via [`super::cas_path::cas_shard_path`]) to avoid
/// filesystem limits in any single directory.
///
/// Layout:
/// ```text
/// {root}/
///   {registry_slug}/
///     {algorithm}/        e.g. sha256
///       {2hex}/           first 2 hex chars of digest
///         {30hex}/        next 30 hex chars
///           content/
///           digest
/// ```
#[derive(Debug, Clone)]
pub struct LayerStore {
    root: PathBuf,
}

impl LayerStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The root directory of the layer store.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the layer directory path for the given registry and digest.
    pub fn path(&self, registry: &str, digest: &oci::Digest) -> PathBuf {
        self.root
            .join(super::slugify(registry))
            .join(super::cas_path::cas_shard_path(digest))
    }

    /// Returns the `content/` path for the given registry and digest.
    pub fn content(&self, registry: &str, digest: &oci::Digest) -> PathBuf {
        self.path(registry, digest).join("content")
    }

    /// Returns the `digest` file path for the given registry and digest.
    pub fn digest_file(&self, registry: &str, digest: &oci::Digest) -> PathBuf {
        self.path(registry, digest).join(super::cas_path::DIGEST_FILENAME)
    }

    /// Lists all layer directories currently present in the store.
    ///
    /// A layer directory is identified by the presence of a `content/` child
    /// directory. Recursion stops at that point so that layer-installed files
    /// are never traversed.
    ///
    /// Returns an empty vec if the store root does not exist yet.
    pub async fn list_all(&self) -> Result<Vec<LayerDir>> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        crate::utility::fs::DirWalker::new(self.root.clone(), classify_layer_dir)
            .max_depth(MAX_WALK_DEPTH)
            .walk()
            .await
    }
}

/// Registry directory + CAS shard depth (algorithm/prefix/suffix).
const MAX_WALK_DEPTH: usize = 1 + super::cas_path::CAS_SHARD_DEPTH;

/// Directory names that are part of the layer layout and must not be
/// recursed into during the store walk.
const LAYER_SKIP_NAMES: &[&str] = &["content"];

/// Classifies a directory for the generic walker.
///
/// - If a `content/` subdirectory exists and the path is valid CAS →
///   [`WalkDecision::leaf`] with a [`LayerDir`].
/// - If `content/` exists but the path is invalid → [`WalkDecision::skip`].
/// - Otherwise → [`WalkDecision::descend_skip`], skipping `content`.
fn classify_layer_dir(dir: &Path, _depth: usize) -> crate::utility::fs::WalkDecision<LayerDir> {
    if dir.join("content").is_dir() {
        if super::cas_path::is_valid_cas_path(dir) {
            return crate::utility::fs::WalkDecision::leaf(LayerDir { dir: dir.to_path_buf() });
        }
        log::warn!("Skipping content/ dir not matching CAS layout: {}", dir.display());
        return crate::utility::fs::WalkDecision::skip();
    }
    crate::utility::fs::WalkDecision::descend_skip(LAYER_SKIP_NAMES)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci;

    const SHA256_HEX: &str = "43567c07f1a6b07b5e8dc052108c9d4c4a32130e18bcbd8a78c53af3e90325d9";

    fn digest() -> oci::Digest {
        oci::Digest::Sha256(SHA256_HEX.to_string())
    }

    // ---- path construction ------------------------------------------------

    #[test]
    fn path_flat_registry() {
        let store = LayerStore::new("/layers");
        let p = store.path("example.com", &digest());
        let expected = Path::new("/layers")
            .join("example.com")
            .join("sha256")
            .join("43")
            .join("567c07f1a6b07b5e8dc052108c9d4c");
        assert_eq!(p, expected);
    }

    #[test]
    fn path_port_containing_registry_is_slugified() {
        let store = LayerStore::new("/layers");
        let p = store.path("localhost:5000", &digest());
        let expected = Path::new("/layers")
            .join("localhost_5000")
            .join("sha256")
            .join("43")
            .join("567c07f1a6b07b5e8dc052108c9d4c");
        assert_eq!(p, expected);
    }

    #[test]
    fn content_is_path_join_content() {
        let store = LayerStore::new("/layers");
        let p = store.content("example.com", &digest());
        assert_eq!(p.file_name().unwrap(), "content");
        assert_eq!(p.parent().unwrap(), store.path("example.com", &digest()));
    }

    #[test]
    fn digest_file_is_path_join_digest() {
        let store = LayerStore::new("/layers");
        let p = store.digest_file("example.com", &digest());
        assert_eq!(p.file_name().unwrap(), "digest");
        assert_eq!(p.parent().unwrap(), store.path("example.com", &digest()));
    }

    // ---- LayerDir accessors -----------------------------------------------

    #[test]
    fn layer_dir_accessors() {
        let layer = LayerDir {
            dir: PathBuf::from("/layers/reg/sha256/43/rest"),
        };
        assert_eq!(layer.content(), PathBuf::from("/layers/reg/sha256/43/rest/content"));
        assert_eq!(layer.digest_file(), PathBuf::from("/layers/reg/sha256/43/rest/digest"));
    }

    // ---- list_all ---------------------------------------------------------

    #[tokio::test]
    async fn list_all_returns_empty_when_root_absent() {
        let store = LayerStore::new("/nonexistent/path/that/does/not/exist");
        assert_eq!(store.list_all().await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn list_all_finds_single_layer() {
        let dir = tempfile::tempdir().unwrap();
        let content = dir
            .path()
            .join("example.com/sha256/43/567c07f1a6b07b5e8dc052108c9d4c/content");
        std::fs::create_dir_all(&content).unwrap();

        let store = LayerStore::new(dir.path());
        let layers = store.list_all().await.unwrap();
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0].content(), content);
    }

    #[tokio::test]
    async fn list_all_skips_invalid_cas_path() {
        let dir = tempfile::tempdir().unwrap();

        // Valid layer
        let valid = dir
            .path()
            .join("example.com/sha256/43/567c07f1a6b07b5e8dc052108c9d4c/content");
        std::fs::create_dir_all(&valid).unwrap();

        // Invalid: wrong algorithm
        let invalid = dir
            .path()
            .join("example.com/md5/43/567c07f1a6b07b5e8dc052108c9d4c/content");
        std::fs::create_dir_all(&invalid).unwrap();

        let store = LayerStore::new(dir.path());
        let layers = store.list_all().await.unwrap();
        assert_eq!(layers.len(), 1);
    }

    #[tokio::test]
    async fn list_all_does_not_recurse_into_content_subdirs() {
        let dir = tempfile::tempdir().unwrap();
        let content = dir
            .path()
            .join("example.com/sha256/43/567c07f1a6b07b5e8dc052108c9d4c/content");
        // Nested content/ inside the layer should not produce a second result
        std::fs::create_dir_all(content.join("subdir/content")).unwrap();

        let store = LayerStore::new(dir.path());
        let layers = store.list_all().await.unwrap();
        assert_eq!(layers.len(), 1);
    }

    #[tokio::test]
    async fn list_all_skips_directory_without_content() {
        let dir = tempfile::tempdir().unwrap();
        // Directory with correct CAS structure but no `content/` child
        let no_content = dir.path().join("example.com/sha256/43/567c07f1a6b07b5e8dc052108c9d4c");
        std::fs::create_dir_all(&no_content).unwrap();
        std::fs::write(no_content.join("digest"), b"sha256:43567c...").unwrap();

        let store = LayerStore::new(dir.path());
        let layers = store.list_all().await.unwrap();
        assert_eq!(layers.len(), 0);
    }
}
