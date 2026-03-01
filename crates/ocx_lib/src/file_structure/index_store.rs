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
            .join(super::slugify(identifier.registry()))
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
            .join(super::slugify(identifier.registry()))
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
        let tags_dir = self.root.join(super::slugify(registry)).join("tags");
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci;

    const SHA256_HEX: &str = "43567c07f1a6b07b5e8dc052108c9d4c4a32130e18bcbd8a78c53af3e90325d9";

    fn digest() -> oci::Digest {
        oci::Digest::Sha256(SHA256_HEX.to_string())
    }

    fn id() -> oci::Identifier {
        oci::Identifier::new_registry("cmake", "example.com").clone_with_tag("3.28")
    }

    // ── path methods ─────────────────────────────────────────────────────────

    #[test]
    fn tags_path_structure() {
        let store = IndexStore::new("/index");
        let p = store.tags(&id());
        assert_eq!(p, PathBuf::from("/index/example.com/tags/cmake.json"));
    }

    #[test]
    fn blob_path_structure() {
        let store = IndexStore::new("/index");
        let p = store.blob(&id(), &digest());
        assert_eq!(
            p,
            PathBuf::from("/index/example.com/objects/sha256/43567c07/f1a6b07b/5e8dc052108c9d4c")
        );
    }

    #[test]
    fn manifest_path_is_blob_with_json_extension() {
        let store = IndexStore::new("/index");
        let p = store.manifest(&id(), &digest());
        assert_eq!(
            p,
            PathBuf::from(
                "/index/example.com/objects/sha256/43567c07/f1a6b07b/5e8dc052108c9d4c.json"
            )
        );
    }

    #[test]
    fn blob_with_extension_appends_extension() {
        let store = IndexStore::new("/index");
        let p = store.blob_with_extension(&id(), &digest(), "toml");
        assert!(p.to_str().unwrap().ends_with(".toml"));
    }

    // ── list_repositories ────────────────────────────────────────────────────

    #[test]
    fn list_repositories_returns_empty_when_registry_dir_absent() {
        let store = IndexStore::new("/nonexistent/path/that/does/not/exist");
        assert_eq!(store.list_repositories("example.com").unwrap(), Vec::<String>::new());
    }

    #[test]
    fn list_repositories_returns_sorted_repository_names() {
        let dir = tempfile::tempdir().unwrap();
        let tags_dir = dir.path().join("example.com/tags");
        std::fs::create_dir_all(&tags_dir).unwrap();
        std::fs::write(tags_dir.join("zlib.json"), b"{}").unwrap();
        std::fs::write(tags_dir.join("cmake.json"), b"{}").unwrap();
        std::fs::write(tags_dir.join("clang.json"), b"{}").unwrap();

        let store = IndexStore::new(dir.path());
        let repos = store.list_repositories("example.com").unwrap();
        assert_eq!(repos, vec!["clang", "cmake", "zlib"]);
    }

    #[test]
    fn list_repositories_ignores_non_json_files() {
        let dir = tempfile::tempdir().unwrap();
        let tags_dir = dir.path().join("example.com/tags");
        std::fs::create_dir_all(&tags_dir).unwrap();
        std::fs::write(tags_dir.join("cmake.json"), b"{}").unwrap();
        std::fs::write(tags_dir.join("README.txt"), b"ignore me").unwrap();
        std::fs::write(tags_dir.join("notes"), b"no extension").unwrap();

        let store = IndexStore::new(dir.path());
        let repos = store.list_repositories("example.com").unwrap();
        assert_eq!(repos, vec!["cmake"]);
    }
}
