// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::{Path, PathBuf};

use crate::{Error, Result, oci};

/// Represents a single content-addressed object directory within the object store.
///
/// An object directory has a fixed layout:
/// - `content/`      — the installed package files (directory tree)
/// - `metadata.json` — package metadata
/// - `refs/`         — back-reference symlinks managed by [`crate::reference_manager::ReferenceManager`]
pub struct ObjectDir {
    /// The root directory of this object (parent of `content/`, `metadata.json`, `refs/`).
    pub dir: PathBuf,
}

impl ObjectDir {
    /// Path to the package content directory.
    pub fn content(&self) -> PathBuf {
        self.dir.join("content")
    }

    /// Path to the package metadata file.
    pub fn metadata(&self) -> PathBuf {
        self.dir.join("metadata.json")
    }

    /// Path to the back-reference symlink directory.
    pub fn refs_dir(&self) -> PathBuf {
        self.dir.join("refs")
    }

    /// Reconstructs a partial digest string from the sharded object store path.
    ///
    /// Returns `"{algorithm}:{shard1}{shard2}{shard3}"` (e.g.
    /// `"sha256:43567c07f1a6b07b5e8dc052108c9d4c"`). This is a prefix of the
    /// full digest since the sharding scheme only encodes the first 32 hex chars.
    ///
    /// Returns `None` if the path structure is unexpected.
    pub fn digest_string(&self) -> Option<String> {
        let mut components = self.dir.components().rev();
        let shard3 = components.next()?.as_os_str().to_str()?;
        let shard2 = components.next()?.as_os_str().to_str()?;
        let shard1 = components.next()?.as_os_str().to_str()?;
        let algorithm = components.next()?.as_os_str().to_str()?;
        Some(format!("{algorithm}:{shard1}{shard2}{shard3}"))
    }
}

/// Manages the content-addressed object store on the local filesystem.
///
/// All objects are stored under a single `root` directory, sharded by
/// registry, repository, and digest to avoid filesystem limits in any
/// single directory.
///
/// Layout:
/// ```text
/// {root}/
///   {registry}/
///     {repository}/
///       {algorithm}/          e.g. sha256
///         {shard_a}/          first 8 hex chars of digest
///           {shard_b}/        next 8 hex chars
///             {shard_c}/      next 16 hex chars
///               content/
///               metadata.json
///               refs/
/// ```
#[derive(Debug, Clone)]
pub struct ObjectStore {
    root: PathBuf,
}

impl ObjectStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The root directory of the object store.
    pub fn root(&self) -> &PathBuf {
        &self.root
    }

    /// Returns the object directory path for the given identifier.
    ///
    /// Requires the identifier to carry a digest; returns an error otherwise.
    pub fn path(&self, identifier: &oci::Identifier) -> Result<PathBuf> {
        let digest = identifier
            .digest()
            .ok_or_else(|| super::error::Error::MissingDigest(identifier.to_string()))?;
        Ok(self
            .root
            .join(super::slugify(identifier.registry()))
            .join(super::repository_path(identifier.repository()))
            .join(Self::digest_path(&digest)))
    }

    /// Returns the `content/` path for the given identifier.
    pub fn content(&self, identifier: &oci::Identifier) -> Result<PathBuf> {
        Ok(self.path(identifier)?.join("content"))
    }

    /// Returns the `metadata.json` path for the given identifier.
    pub fn metadata(&self, identifier: &oci::Identifier) -> Result<PathBuf> {
        Ok(self.path(identifier)?.join("metadata.json"))
    }

    /// Returns the `metadata.json` path for the object that owns `content_path`.
    ///
    /// `content_path` may be a real path or a symlink; symlinks are resolved
    /// before navigating to the sibling file.
    pub fn metadata_for_content(&self, content_path: &Path) -> Result<PathBuf> {
        Ok(Self::object_dir_for_content(content_path)?.join("metadata.json"))
    }

    /// Returns the `refs/` directory for the object that owns `content_path`.
    ///
    /// `content_path` may be a real path or a symlink; symlinks are resolved
    /// before navigating to the sibling directory.
    pub fn refs_dir_for_content(&self, content_path: &Path) -> Result<PathBuf> {
        Ok(Self::object_dir_for_content(content_path)?.join("refs"))
    }

    /// Lists all object directories currently present in the store.
    ///
    /// An object directory is identified by the presence of a `content/` child
    /// directory.  Recursion stops at that point so that package-installed files
    /// (which may themselves contain arbitrary subdirectories) are never traversed.
    ///
    /// Returns an empty vec if the store root does not exist yet.
    pub fn list_all(&self) -> Result<Vec<ObjectDir>> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        let mut result = Vec::new();
        Self::walk_object_dirs(&self.root, &mut result)?;
        Ok(result)
    }

    /// Resolves `content_path` (following symlinks) and returns the object root
    /// directory that contains it.  Used internally to derive sibling paths such
    /// as `metadata.json` and `refs/`.
    fn object_dir_for_content(content_path: &Path) -> Result<PathBuf> {
        let canonical =
            dunce::canonicalize(content_path).map_err(|e| Error::InternalFile(content_path.to_path_buf(), e))?;
        canonical
            .parent()
            .map(|p| p.to_path_buf())
            .ok_or(Error::InternalPathInvalid(canonical))
    }

    /// Recursively walks `dir` and collects all object directories.
    ///
    /// A directory is treated as an object directory when it directly contains
    /// a `content/` subdirectory.  Recursion stops at that point so that
    /// package-installed files are never traversed.
    fn walk_object_dirs(dir: &Path, result: &mut Vec<ObjectDir>) -> Result<()> {
        if dir.join("content").is_dir() {
            result.push(ObjectDir { dir: dir.to_path_buf() });
            return Ok(());
        }
        let entries = std::fs::read_dir(dir).map_err(|e| Error::InternalFile(dir.to_path_buf(), e))?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                Self::walk_object_dirs(&path, result)?;
            }
        }
        Ok(())
    }

    /// Converts a digest to a sharded path component.
    ///
    /// The sharding scheme splits the hex string into three levels to avoid
    /// filesystem directory entry limits:
    /// `{algorithm}/{hex[0..8]}/{hex[8..16]}/{hex[16..32]}`
    fn digest_path(digest: &oci::Digest) -> PathBuf {
        let (algorithm, h) = match digest {
            oci::Digest::Sha256(h) => ("sha256", h.as_str()),
            oci::Digest::Sha384(h) => ("sha384", h.as_str()),
            oci::Digest::Sha512(h) => ("sha512", h.as_str()),
        };
        Path::new(algorithm).join(&h[0..8]).join(&h[8..16]).join(&h[16..32])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci;

    // hex = 43567c07 | f1a6b07b | 5e8dc052108c9d4c | 4a32130e18bcbd8a78c53af3e90325d9
    const SHA256_HEX: &str = "43567c07f1a6b07b5e8dc052108c9d4c4a32130e18bcbd8a78c53af3e90325d9";

    fn digest() -> oci::Digest {
        oci::Digest::Sha256(SHA256_HEX.to_string())
    }

    fn id_with_digest() -> oci::Identifier {
        oci::Identifier::new_registry("cmake", "example.com").clone_with_digest(digest())
    }

    fn id_nested_repo_with_digest() -> oci::Identifier {
        oci::Identifier::new_registry("org/sub/pkg", "example.com").clone_with_digest(digest())
    }

    fn id_tag_only() -> oci::Identifier {
        oci::Identifier::new_registry("cmake", "example.com").clone_with_tag("3.28")
    }

    // ── digest_path ───────────────────────────────────────────────────────────

    #[test]
    fn path_sharding_sha256() {
        let store = ObjectStore::new("/store");
        let p = store.path(&id_with_digest()).unwrap();
        let expected = Path::new("/store")
            .join("example.com")
            .join("cmake")
            .join("sha256")
            .join("43567c07")
            .join("f1a6b07b")
            .join("5e8dc052108c9d4c");
        assert_eq!(p, expected);
    }

    #[test]
    fn path_sharding_nested_repo() {
        let store = ObjectStore::new("/store");
        let p = store.path(&id_nested_repo_with_digest()).unwrap();
        let expected = Path::new("/store")
            .join("example.com")
            .join("org")
            .join("sub")
            .join("pkg")
            .join("sha256")
            .join("43567c07")
            .join("f1a6b07b")
            .join("5e8dc052108c9d4c");
        assert_eq!(p, expected);
    }

    #[test]
    fn path_requires_digest() {
        let store = ObjectStore::new("/store");
        assert!(store.path(&id_tag_only()).is_err());
    }

    // ── content / metadata ────────────────────────────────────────────────────

    #[test]
    fn content_is_path_join_content() {
        let store = ObjectStore::new("/store");
        let p = store.content(&id_with_digest()).unwrap();
        assert_eq!(p.file_name().unwrap(), "content");
        assert_eq!(p.parent().unwrap(), store.path(&id_with_digest()).unwrap());
    }

    #[test]
    fn metadata_is_path_join_metadata_json() {
        let store = ObjectStore::new("/store");
        let p = store.metadata(&id_with_digest()).unwrap();
        assert_eq!(p.file_name().unwrap(), "metadata.json");
        assert_eq!(p.parent().unwrap(), store.path(&id_with_digest()).unwrap());
    }

    // ── metadata_for_content / refs_dir_for_content ───────────────────────────

    #[test]
    fn metadata_for_content_returns_sibling_metadata_json() {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let obj = root.join("obj");
        let content = obj.join("content");
        std::fs::create_dir_all(&content).unwrap();

        let store = ObjectStore::new(&root);
        let result = store.metadata_for_content(&content).unwrap();
        assert_eq!(result, obj.join("metadata.json"));
    }

    #[test]
    fn refs_dir_for_content_returns_sibling_refs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let obj = root.join("obj");
        let content = obj.join("content");
        std::fs::create_dir_all(&content).unwrap();

        let store = ObjectStore::new(&root);
        let result = store.refs_dir_for_content(&content).unwrap();
        assert_eq!(result, obj.join("refs"));
    }

    #[test]
    fn metadata_for_content_follows_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let obj = root.join("obj");
        let content = obj.join("content");
        std::fs::create_dir_all(&content).unwrap();

        let link = root.join("link");
        crate::symlink::create(&content, &link).unwrap();

        let store = ObjectStore::new(&root);
        let result = store.metadata_for_content(&link).unwrap();
        assert_eq!(result, obj.join("metadata.json"));
    }

    // ── list_all ──────────────────────────────────────────────────────────────

    #[test]
    fn list_all_returns_empty_when_root_absent() {
        let store = ObjectStore::new("/nonexistent/path/that/does/not/exist");
        assert_eq!(store.list_all().unwrap().len(), 0);
    }

    #[test]
    fn list_all_finds_single_object() {
        let dir = tempfile::tempdir().unwrap();
        let content = dir.path().join("reg/repo/sha256/aa/bb/cc/content");
        std::fs::create_dir_all(&content).unwrap();

        let store = ObjectStore::new(dir.path());
        let objects = store.list_all().unwrap();
        assert_eq!(objects.len(), 1);
        assert_eq!(objects[0].content(), content);
    }

    #[test]
    fn list_all_finds_multiple_objects_at_different_paths() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("reg/cmake/sha256/aa/bb/cc/content")).unwrap();
        std::fs::create_dir_all(dir.path().join("reg/cmake/sha256/dd/ee/ff/content")).unwrap();
        std::fs::create_dir_all(dir.path().join("reg/clang/sha256/11/22/33/content")).unwrap();

        let store = ObjectStore::new(dir.path());
        let objects = store.list_all().unwrap();
        assert_eq!(objects.len(), 3);
    }

    #[test]
    fn list_all_does_not_recurse_into_content_subdirs() {
        // A nested `content/` inside the package files must not be treated as
        // another object directory.
        let dir = tempfile::tempdir().unwrap();
        let content = dir.path().join("reg/repo/sha256/aa/bb/cc/content");
        std::fs::create_dir_all(content.join("subdir/content")).unwrap();

        let store = ObjectStore::new(dir.path());
        let objects = store.list_all().unwrap();
        assert_eq!(objects.len(), 1);
    }

    // ── ObjectDir accessors ───────────────────────────────────────────────────

    #[test]
    fn object_dir_accessors() {
        let obj_dir = ObjectDir {
            dir: PathBuf::from("/store/reg/cmake/sha256/a/b/c"),
        };
        assert_eq!(
            obj_dir.content(),
            PathBuf::from("/store/reg/cmake/sha256/a/b/c/content")
        );
        assert_eq!(
            obj_dir.metadata(),
            PathBuf::from("/store/reg/cmake/sha256/a/b/c/metadata.json")
        );
        assert_eq!(obj_dir.refs_dir(), PathBuf::from("/store/reg/cmake/sha256/a/b/c/refs"));
    }
}
