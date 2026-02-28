use std::path::PathBuf;

use crate::{Result, oci};

/// Manages the local OCI index on the filesystem.
///
/// The index stores tag→digest mappings and cached manifests, enabling
/// offline resolution and fast tag lookups without querying a remote registry.
///
/// Layout:
/// ```text
/// {root}/
///   {registry}/
///     tags/
///       {repository}.json      — tag→digest map for the repository
///     objects/
///       {algorithm}/
///         {shard_a}/{shard_b}/{shard_c}.json  — cached manifest JSON
/// ```
#[derive(Debug, Clone)]
pub struct IndexStore {
    root: PathBuf,
}

impl IndexStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The root directory of the index store.
    pub fn root(&self) -> &PathBuf {
        &self.root
    }

    /// Path to the tag-to-digest map JSON file for the given identifier.
    pub fn tags(&self, identifier: &oci::Identifier) -> PathBuf {
        self.root
            .join(identifier.registry())
            .join("tags")
            .join(identifier.repository())
            .with_added_extension("json")
    }

    /// Path to the cached manifest JSON file for the given identifier and digest.
    pub fn manifest(&self, identifier: &oci::Identifier, digest: &oci::Digest) -> PathBuf {
        self.blob_with_extension(identifier, digest, "json")
    }

    /// Path to a raw blob file (no extension) for the given identifier and digest.
    pub fn blob(&self, identifier: &oci::Identifier, digest: &oci::Digest) -> PathBuf {
        self.root
            .join(identifier.registry())
            .join("objects")
            .join(Self::digest_path(digest))
    }

    /// Path to a blob file with the given extension.
    pub fn blob_with_extension(
        &self,
        identifier: &oci::Identifier,
        digest: &oci::Digest,
        extension: impl AsRef<std::ffi::OsStr>,
    ) -> PathBuf {
        self.blob(identifier, digest).with_added_extension(extension)
    }

    /// Lists all repository names cached for the given registry.
    ///
    /// Scans `{root}/{registry}/tags/` for `.json` files and returns their
    /// stems as repository names, sorted alphabetically.
    ///
    /// Returns an empty vec if the directory does not exist.
    pub fn list_repositories(&self, registry: &str) -> Result<Vec<String>> {
        let tags_dir = self.root.join(registry).join("tags");
        if !tags_dir.exists() {
            return Ok(Vec::new());
        }
        let entries = std::fs::read_dir(&tags_dir)
            .map_err(|e| crate::error::file_error(&tags_dir, e))?;
        let mut repos = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| crate::error::file_error(&tags_dir, e))?;
            let path = entry.path();
            if path.extension().map(|e| e == "json").unwrap_or(false) {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    repos.push(stem.to_string());
                }
            }
        }
        repos.sort();
        Ok(repos)
    }

    /// Converts a digest to a sharded path component.
    ///
    /// Uses the same three-level sharding scheme as [`ObjectStore`]:
    /// `{algorithm}/{hex[0..8]}/{hex[8..16]}/{hex[16..32]}`
    fn digest_path(digest: &oci::Digest) -> PathBuf {
        match digest {
            oci::Digest::Sha256(h) => {
                PathBuf::from(format!("sha256/{}/{}/{}", &h[0..8], &h[8..16], &h[16..32]))
            }
            oci::Digest::Sha384(h) => {
                PathBuf::from(format!("sha384/{}/{}/{}", &h[0..8], &h[8..16], &h[16..32]))
            }
            oci::Digest::Sha512(h) => {
                PathBuf::from(format!("sha512/{}/{}/{}", &h[0..8], &h[8..16], &h[16..32]))
            }
        }
    }
}
