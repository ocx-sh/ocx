// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::{Path, PathBuf};

use crate::{Error, Result, log, oci};

/// Algorithm names and shard hex lengths — single source of truth for
/// [`ObjectStore::digest_path`] and the store walker validation.
const SHARD_DIGEST_ALGORITHMS: &[&str] = &["sha256", "sha384", "sha512"];
/// Hex lengths for each of the 3 sharding levels, totaling 32 chars (128 bits).
const SHARD_DIGEST_LENGTHS: &[usize] = &[8, 8, 16];

/// Represents a single content-addressed object directory within the object store.
///
/// An object directory has a fixed layout:
/// - `content/`      — the installed package files (directory tree)
/// - `metadata.json` — package metadata
/// - `refs/`         — back-reference symlinks managed by [`crate::reference_manager::ReferenceManager`]
/// - `deps/`         — forward-dependency symlinks managed by [`crate::reference_manager::ReferenceManager`]
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

    /// Path to the forward-dependency symlink directory.
    pub fn deps_dir(&self) -> PathBuf {
        self.dir.join("deps")
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
///               deps/
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

    /// Returns the repository-level directory for the given identifier.
    ///
    /// This is the common ancestor directory for all digest-sharded objects
    /// belonging to the same registry + repository combination. Does not
    /// require the identifier to carry a digest.
    pub fn repository_dir(&self, identifier: &oci::Identifier) -> PathBuf {
        self.root
            .join(super::slugify(identifier.registry()))
            .join(super::repository_path(identifier.repository()))
    }

    /// Returns the object directory path for the given identifier.
    pub fn path(&self, identifier: &oci::PinnedIdentifier) -> PathBuf {
        self.root
            .join(super::slugify(identifier.registry()))
            .join(super::repository_path(identifier.repository()))
            .join(Self::digest_path(&identifier.digest()))
    }

    /// Returns the `content/` path for the given identifier.
    pub fn content(&self, identifier: &oci::PinnedIdentifier) -> PathBuf {
        self.path(identifier).join("content")
    }

    /// Returns the `metadata.json` path for the given identifier.
    pub fn metadata(&self, identifier: &oci::PinnedIdentifier) -> PathBuf {
        self.path(identifier).join("metadata.json")
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

    /// Returns the `deps/` directory for the object that owns `content_path`.
    ///
    /// `content_path` may be a real path or a symlink; symlinks are resolved
    /// before navigating to the sibling directory.
    pub fn deps_dir_for_content(&self, content_path: &Path) -> Result<PathBuf> {
        Ok(Self::object_dir_for_content(content_path)?.join("deps"))
    }

    /// Returns the `resolve.json` path for the given identifier.
    pub fn resolve(&self, identifier: &oci::PinnedIdentifier) -> PathBuf {
        self.path(identifier).join("resolve.json")
    }

    /// Returns the `install.json` path for the given identifier.
    pub fn install_status(&self, identifier: &oci::PinnedIdentifier) -> PathBuf {
        self.path(identifier).join("install.json")
    }

    /// Returns the `resolve.json` path for the object that owns `content_path`.
    ///
    /// `content_path` may be a real path or a symlink; symlinks are resolved
    /// before navigating to the sibling file.
    pub fn resolve_for_content(&self, content_path: &Path) -> Result<PathBuf> {
        Ok(Self::object_dir_for_content(content_path)?.join("resolve.json"))
    }

    /// Lists all object directories currently present in the store.
    ///
    /// An object directory is identified by the presence of a `content/` child
    /// directory.  Recursion stops at that point so that package-installed files
    /// (which may themselves contain arbitrary subdirectories) are never traversed.
    ///
    /// Returns an empty vec if the store root does not exist yet.
    pub async fn list_all(&self) -> Result<Vec<ObjectDir>> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        crate::utility::fs::DirWalker::new(self.root.clone(), classify_object_dir)
            .max_depth(MAX_WALK_DEPTH)
            .walk()
            .await
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

    /// Converts a digest to a sharded path component using [`SHARD_DIGEST_LENGTHS`].
    ///
    /// Produces: `{algorithm}/{hex[0..8]}/{hex[8..16]}/{hex[16..32]}`
    fn digest_path(digest: &oci::Digest) -> PathBuf {
        // Explicit match ensures a compile error when a new Digest variant is
        // added, forcing the author to update SHARD_DIGEST_ALGORITHMS too
        // (used by is_valid_object_path for store-walk validation).
        let (algorithm, h) = match digest {
            oci::Digest::Sha256(h) => (SHARD_DIGEST_ALGORITHMS[0], h.as_str()),
            oci::Digest::Sha384(h) => (SHARD_DIGEST_ALGORITHMS[1], h.as_str()),
            oci::Digest::Sha512(h) => (SHARD_DIGEST_ALGORITHMS[2], h.as_str()),
        };
        let mut path = PathBuf::from(algorithm);
        let mut offset = 0;
        for &len in SHARD_DIGEST_LENGTHS {
            debug_assert!(offset + len <= h.len(), "Digest hex string is shorter than expected");
            path.push(&h[offset..offset + len]);
            offset += len;
        }
        path
    }
}

/// Object store layout: `{registry}/{repo_segments...}/{algorithm}/{shards...}`.
/// OCI repos are typically 1–3 segments; 5 is generous.
const MAX_WALK_DEPTH: usize = 1 + 5 + 1 + SHARD_DIGEST_LENGTHS.len();

/// Directory names that are part of the object layout and must not be
/// recursed into during the store walk.
const OBJECT_SKIP_NAMES: &[&str] = &["content", "refs", "deps"];

/// Classifies a directory for the generic walker.
///
/// - If a `content/` subdirectory exists and the path matches the sharded
///   digest layout → [`WalkAction::Leaf`] with an [`ObjectDir`].
/// - If `content/` exists but the path is invalid → [`WalkAction::Skip`].
/// - Otherwise → [`WalkAction::Descend`], skipping `content`, `refs`, `deps`.
fn classify_object_dir(dir: &Path, _depth: usize) -> crate::utility::fs::WalkAction<ObjectDir> {
    if dir.join("content").is_dir() {
        if is_valid_object_path(dir) {
            return crate::utility::fs::WalkAction::Leaf(ObjectDir { dir: dir.to_path_buf() });
        }
        log::warn!("Skipping content/ dir not matching store layout: {}", dir.display());
        return crate::utility::fs::WalkAction::Skip;
    }
    crate::utility::fs::WalkAction::Descend {
        skip_names: OBJECT_SKIP_NAMES,
    }
}

/// Validates path ends with `{algorithm}/{8hex}/{8hex}/{16hex}`.
fn is_valid_object_path(dir: &Path) -> bool {
    let tail: Vec<&str> = dir
        .components()
        .rev()
        .take(1 + SHARD_DIGEST_LENGTHS.len())
        .filter_map(|c| c.as_os_str().to_str())
        .collect();
    if tail.len() != 1 + SHARD_DIGEST_LENGTHS.len() {
        return false;
    }
    // tail is reversed: [shard3, shard2, shard1, algorithm]
    let algorithm = tail[SHARD_DIGEST_LENGTHS.len()];
    if !SHARD_DIGEST_ALGORITHMS.contains(&algorithm) {
        return false;
    }
    SHARD_DIGEST_LENGTHS
        .iter()
        .rev()
        .zip(&tail[..SHARD_DIGEST_LENGTHS.len()])
        .all(|(len, name)| name.len() == *len && name.bytes().all(|b| b.is_ascii_hexdigit()))
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

    fn pinned() -> oci::PinnedIdentifier {
        oci::PinnedIdentifier::try_from(id_with_digest()).unwrap()
    }

    fn pinned_nested() -> oci::PinnedIdentifier {
        let id = oci::Identifier::new_registry("org/sub/pkg", "example.com").clone_with_digest(digest());
        oci::PinnedIdentifier::try_from(id).unwrap()
    }

    // ── digest_path ───────────────────────────────────────────────────────────

    #[test]
    fn path_sharding_sha256() {
        let store = ObjectStore::new("/store");
        let p = store.path(&pinned());
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
        let p = store.path(&pinned_nested());
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

    // ── content / metadata ────────────────────────────────────────────────────

    #[test]
    fn content_is_path_join_content() {
        let store = ObjectStore::new("/store");
        let p = store.content(&pinned());
        assert_eq!(p.file_name().unwrap(), "content");
        assert_eq!(p.parent().unwrap(), store.path(&pinned()));
    }

    #[test]
    fn metadata_is_path_join_metadata_json() {
        let store = ObjectStore::new("/store");
        let p = store.metadata(&pinned());
        assert_eq!(p.file_name().unwrap(), "metadata.json");
        assert_eq!(p.parent().unwrap(), store.path(&pinned()));
    }

    // ── metadata_for_content / refs_dir_for_content ───────────────────────────

    #[test]
    fn metadata_for_content_returns_sibling_metadata_json() {
        let (_dir, root, store) = crate::test::fixtures::temp_object_store();
        let obj = root.join("obj");
        let content = obj.join("content");
        std::fs::create_dir_all(&content).unwrap();
        let result = store.metadata_for_content(&content).unwrap();
        assert_eq!(result, obj.join("metadata.json"));
    }

    #[test]
    fn refs_dir_for_content_returns_sibling_refs() {
        let (_dir, root, store) = crate::test::fixtures::temp_object_store();
        let obj = root.join("obj");
        let content = obj.join("content");
        std::fs::create_dir_all(&content).unwrap();
        let result = store.refs_dir_for_content(&content).unwrap();
        assert_eq!(result, obj.join("refs"));
    }

    #[test]
    fn resolve_for_content_returns_sibling_resolve_json() {
        let (_dir, root, store) = crate::test::fixtures::temp_object_store();
        let obj = root.join("obj");
        let content = obj.join("content");
        std::fs::create_dir_all(&content).unwrap();
        let result = store.resolve_for_content(&content).unwrap();
        assert_eq!(result, obj.join("resolve.json"));
    }

    #[test]
    fn metadata_for_content_follows_symlink() {
        let (_dir, root, store) = crate::test::fixtures::temp_object_store();
        let obj = root.join("obj");
        let content = obj.join("content");
        std::fs::create_dir_all(&content).unwrap();

        let link = root.join("link");
        crate::symlink::create(&content, &link).unwrap();
        let result = store.metadata_for_content(&link).unwrap();
        assert_eq!(result, obj.join("metadata.json"));
    }

    // ── list_all ──────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn list_all_returns_empty_when_root_absent() {
        let store = ObjectStore::new("/nonexistent/path/that/does/not/exist");
        assert_eq!(store.list_all().await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn list_all_finds_single_object() {
        let dir = tempfile::tempdir().unwrap();
        let content = dir
            .path()
            .join("reg/repo/sha256/aabbccdd/11223344/aabbccdd11223344/content");
        std::fs::create_dir_all(&content).unwrap();

        let store = ObjectStore::new(dir.path());
        let objects = store.list_all().await.unwrap();
        assert_eq!(objects.len(), 1);
        assert_eq!(objects[0].content(), content);
    }

    #[tokio::test]
    async fn list_all_finds_multiple_objects_at_different_paths() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(
            dir.path()
                .join("reg/cmake/sha256/aabbccdd/11223344/aabbccdd11223344/content"),
        )
        .unwrap();
        std::fs::create_dir_all(
            dir.path()
                .join("reg/cmake/sha256/ddeeff00/55667788/ddeeff0055667788/content"),
        )
        .unwrap();
        std::fs::create_dir_all(
            dir.path()
                .join("reg/clang/sha256/11223344/aabbccdd/11223344aabbccdd/content"),
        )
        .unwrap();

        let store = ObjectStore::new(dir.path());
        let objects = store.list_all().await.unwrap();
        assert_eq!(objects.len(), 3);
    }

    #[tokio::test]
    async fn list_all_does_not_recurse_into_content_subdirs() {
        // A nested `content/` inside the package files must not be treated as
        // another object directory.
        let dir = tempfile::tempdir().unwrap();
        let content = dir
            .path()
            .join("reg/repo/sha256/aabbccdd/11223344/aabbccdd11223344/content");
        std::fs::create_dir_all(content.join("subdir/content")).unwrap();

        let store = ObjectStore::new(dir.path());
        let objects = store.list_all().await.unwrap();
        assert_eq!(objects.len(), 1);
    }

    #[tokio::test]
    async fn list_all_skips_invalid_content_dir() {
        // A `content/` directory that doesn't match the store layout
        // (wrong shard structure) should be skipped.
        let dir = tempfile::tempdir().unwrap();
        // Valid object
        std::fs::create_dir_all(
            dir.path()
                .join("reg/repo/sha256/aabbccdd/11223344/aabbccdd11223344/content"),
        )
        .unwrap();
        // Invalid: short shard names (not matching expected hex lengths)
        std::fs::create_dir_all(dir.path().join("reg/repo/sha256/aa/bb/cc/content")).unwrap();
        // Invalid: no algorithm parent
        std::fs::create_dir_all(
            dir.path()
                .join("reg/repo/notanalgo/aabbccdd/11223344/aabbccdd11223344/content"),
        )
        .unwrap();

        let store = ObjectStore::new(dir.path());
        let objects = store.list_all().await.unwrap();
        assert_eq!(objects.len(), 1);
    }

    #[tokio::test]
    async fn list_all_respects_depth_limit() {
        // A deeply nested directory tree should not be explored beyond MAX_WALK_DEPTH.
        let dir = tempfile::tempdir().unwrap();
        let mut deep = dir.path().to_path_buf();
        for i in 0..MAX_WALK_DEPTH + 5 {
            deep = deep.join(format!("level{i}"));
        }
        deep = deep.join("sha256/aabbccdd/11223344/aabbccdd11223344/content");
        std::fs::create_dir_all(&deep).unwrap();

        let store = ObjectStore::new(dir.path());
        let objects = store.list_all().await.unwrap();
        assert_eq!(objects.len(), 0);
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
        assert_eq!(obj_dir.deps_dir(), PathBuf::from("/store/reg/cmake/sha256/a/b/c/deps"));
    }

    #[test]
    fn deps_dir_for_content_returns_sibling_deps() {
        let (_dir, root, store) = crate::test::fixtures::temp_object_store();
        let obj = root.join("obj");
        let content = obj.join("content");
        std::fs::create_dir_all(&content).unwrap();
        let result = store.deps_dir_for_content(&content).unwrap();
        assert_eq!(result, obj.join("deps"));
    }

    // ── repository_dir ──────────────────────────────────────────────────────

    #[test]
    fn repository_dir_returns_registry_repo_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let store = ObjectStore::new(dir.path());
        let id = oci::Identifier::new_registry("org/pkg", "example.com").clone_with_digest(digest());

        let result = store.repository_dir(&id);
        let expected = dir.path().join("example.com").join("org").join("pkg");
        assert_eq!(result, expected);
    }

    #[test]
    fn repository_dir_nested_repo() {
        let dir = tempfile::tempdir().unwrap();
        let store = ObjectStore::new(dir.path());
        let id = oci::Identifier::new_registry("a/b/c", "example.com").clone_with_digest(digest());

        let result = store.repository_dir(&id);
        let expected = dir.path().join("example.com").join("a").join("b").join("c");
        assert_eq!(result, expected);
    }
}
