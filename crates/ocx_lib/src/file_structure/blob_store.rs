// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::{Path, PathBuf};

use crate::{Result, log, oci};

/// Represents a single content-addressed blob directory within the blob store.
///
/// A blob directory has a fixed layout:
/// - `data`   -- the raw blob content (single file)
/// - `digest` -- full digest string for recovery
pub struct BlobDir {
    /// The root directory of this blob (parent of `data`, `digest`).
    pub dir: PathBuf,
}

impl BlobDir {
    /// Path to the raw blob data file.
    pub fn data(&self) -> PathBuf {
        self.dir.join("data")
    }

    /// Path to the digest marker file.
    pub fn digest_file(&self) -> PathBuf {
        self.dir.join(super::cas_path::DIGEST_FILENAME)
    }
}

/// Manages the content-addressed blob store on the local filesystem.
///
/// All blobs are stored under a single `root` directory, sharded by
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
///           data
///           digest
/// ```
#[derive(Debug, Clone)]
pub struct BlobStore {
    root: PathBuf,
}

impl BlobStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The root directory of the blob store.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the blob directory path for the given registry and digest.
    pub fn path(&self, registry: &str, digest: &oci::Digest) -> PathBuf {
        self.root
            .join(super::slugify(registry))
            .join(super::cas_path::cas_shard_path(digest))
    }

    /// Returns the `data` file path for the given registry and digest.
    pub fn data(&self, registry: &str, digest: &oci::Digest) -> PathBuf {
        self.path(registry, digest).join("data")
    }

    /// Returns the `digest` file path for the given registry and digest.
    pub fn digest_file(&self, registry: &str, digest: &oci::Digest) -> PathBuf {
        self.path(registry, digest).join(super::cas_path::DIGEST_FILENAME)
    }

    /// Lists all blob directories currently present in the store.
    ///
    /// A blob directory is identified by the presence of a `data` child file.
    /// Returns an empty vec if the store root does not exist yet.
    pub async fn list_all(&self) -> Result<Vec<BlobDir>> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        crate::utility::fs::DirWalker::new(self.root.clone(), classify_blob_dir)
            .max_depth(MAX_WALK_DEPTH)
            .walk()
            .await
    }
}

/// Registry directory + CAS shard depth (algorithm/prefix/suffix).
const MAX_WALK_DEPTH: usize = 1 + super::cas_path::CAS_SHARD_DEPTH;

/// Directory names that are part of the blob layout and must not be
/// recursed into during the store walk.
const BLOB_SKIP_NAMES: &[&str] = &[];

/// Classifies a directory for the generic walker.
///
/// - If a `data` file exists and the path is valid CAS → [`WalkDecision::leaf`]
///   with a [`BlobDir`].
/// - If `data` exists but the path is invalid → [`WalkDecision::skip`].
/// - Otherwise → [`WalkDecision::descend`].
fn classify_blob_dir(dir: &Path, _depth: usize) -> crate::utility::fs::WalkDecision<BlobDir> {
    if dir.join("data").is_file() {
        if super::cas_path::is_valid_cas_path(dir) {
            return crate::utility::fs::WalkDecision::leaf(BlobDir { dir: dir.to_path_buf() });
        }
        log::warn!("Skipping data file in dir not matching CAS layout: {}", dir.display());
        return crate::utility::fs::WalkDecision::skip();
    }
    crate::utility::fs::WalkDecision::descend_skip(BLOB_SKIP_NAMES)
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
        let store = BlobStore::new("/blobs");
        let p = store.path("example.com", &digest());
        let expected = Path::new("/blobs")
            .join("example.com")
            .join("sha256")
            .join("43")
            .join("567c07f1a6b07b5e8dc052108c9d4c");
        assert_eq!(p, expected);
    }

    #[test]
    fn path_port_containing_registry_is_slugified() {
        let store = BlobStore::new("/blobs");
        let p = store.path("localhost:5000", &digest());
        let expected = Path::new("/blobs")
            .join("localhost_5000")
            .join("sha256")
            .join("43")
            .join("567c07f1a6b07b5e8dc052108c9d4c");
        assert_eq!(p, expected);
    }

    #[test]
    fn data_is_path_join_data() {
        let store = BlobStore::new("/blobs");
        let p = store.data("example.com", &digest());
        assert_eq!(p.file_name().unwrap(), "data");
        assert_eq!(p.parent().unwrap(), store.path("example.com", &digest()));
    }

    #[test]
    fn digest_file_is_path_join_digest() {
        let store = BlobStore::new("/blobs");
        let p = store.digest_file("example.com", &digest());
        assert_eq!(p.file_name().unwrap(), "digest");
        assert_eq!(p.parent().unwrap(), store.path("example.com", &digest()));
    }

    // ---- BlobDir accessors ------------------------------------------------

    #[test]
    fn blob_dir_accessors() {
        let blob = BlobDir {
            dir: PathBuf::from("/blobs/reg/sha256/43/rest"),
        };
        assert_eq!(blob.data(), PathBuf::from("/blobs/reg/sha256/43/rest/data"));
        assert_eq!(blob.digest_file(), PathBuf::from("/blobs/reg/sha256/43/rest/digest"));
    }

    // ---- list_all ---------------------------------------------------------

    #[tokio::test]
    async fn list_all_returns_empty_when_root_absent() {
        let store = BlobStore::new("/nonexistent/path/that/does/not/exist");
        assert_eq!(store.list_all().await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn list_all_finds_single_blob() {
        let dir = tempfile::tempdir().unwrap();
        let blob_dir = dir.path().join("example.com/sha256/43/567c07f1a6b07b5e8dc052108c9d4c");
        std::fs::create_dir_all(&blob_dir).unwrap();
        std::fs::write(blob_dir.join("data"), b"blob content").unwrap();

        let store = BlobStore::new(dir.path());
        let blobs = store.list_all().await.unwrap();
        assert_eq!(blobs.len(), 1);
        assert_eq!(blobs[0].data(), blob_dir.join("data"));
    }

    #[tokio::test]
    async fn list_all_skips_invalid_cas_path() {
        let dir = tempfile::tempdir().unwrap();

        // Valid blob
        let valid = dir.path().join("example.com/sha256/43/567c07f1a6b07b5e8dc052108c9d4c");
        std::fs::create_dir_all(&valid).unwrap();
        std::fs::write(valid.join("data"), b"valid").unwrap();

        // Invalid: wrong algorithm
        let invalid = dir.path().join("example.com/md5/43/567c07f1a6b07b5e8dc052108c9d4c");
        std::fs::create_dir_all(&invalid).unwrap();
        std::fs::write(invalid.join("data"), b"invalid").unwrap();

        let store = BlobStore::new(dir.path());
        let blobs = store.list_all().await.unwrap();
        assert_eq!(blobs.len(), 1);
    }

    #[tokio::test]
    async fn list_all_skips_directory_without_data_file() {
        let dir = tempfile::tempdir().unwrap();
        // Directory with correct structure but no `data` file
        let no_data = dir.path().join("example.com/sha256/43/567c07f1a6b07b5e8dc052108c9d4c");
        std::fs::create_dir_all(&no_data).unwrap();
        std::fs::write(no_data.join("digest"), b"sha256:43567c...").unwrap();

        let store = BlobStore::new(dir.path());
        let blobs = store.list_all().await.unwrap();
        assert_eq!(blobs.len(), 0);
    }
}
