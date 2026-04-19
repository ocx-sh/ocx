// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::{Path, PathBuf};

use crate::{Error, Result, log, oci};

/// Represents a single content-addressed package directory within the package store.
///
/// A package directory has a fixed layout:
/// - `content/`       -- the installed package files (directory tree)
/// - `metadata.json`  -- package metadata
/// - `manifest.json`  -- OCI manifest
/// - `resolve.json`   -- resolved dependency graph
/// - `install.json`   -- install status
/// - `digest`         -- full digest string for recovery
/// - `refs/symlinks/` -- back-reference symlinks for install tracking
/// - `refs/deps/`     -- back-reference symlinks for dependency tracking
/// - `refs/layers/`   -- back-reference symlinks for layer tracking
/// - `refs/blobs/`    -- back-reference symlinks for blob tracking
pub struct PackageDir {
    /// The root directory of this package (parent of `content/`, `metadata.json`, etc.).
    pub dir: PathBuf,
}

impl PackageDir {
    /// Path to the package content directory.
    pub fn content(&self) -> PathBuf {
        self.dir.join("content")
    }

    /// Path to the package metadata file.
    pub fn metadata(&self) -> PathBuf {
        self.dir.join("metadata.json")
    }

    /// Path to the OCI manifest file.
    pub fn manifest(&self) -> PathBuf {
        self.dir.join("manifest.json")
    }

    /// Path to the resolved dependency graph file.
    pub fn resolve(&self) -> PathBuf {
        self.dir.join("resolve.json")
    }

    /// Path to the install status file.
    pub fn install_status(&self) -> PathBuf {
        self.dir.join("install.json")
    }

    /// Path to the digest marker file.
    pub fn digest_file(&self) -> PathBuf {
        self.dir.join(super::cas_path::DIGEST_FILENAME)
    }

    /// Path to the symlink back-reference directory.
    pub fn refs_symlinks_dir(&self) -> PathBuf {
        self.dir.join("refs").join("symlinks")
    }

    /// Path to the dependency back-reference directory.
    pub fn refs_deps_dir(&self) -> PathBuf {
        self.dir.join("refs").join("deps")
    }

    /// Path to the layer back-reference directory.
    pub fn refs_layers_dir(&self) -> PathBuf {
        self.dir.join("refs").join("layers")
    }

    /// Path to the blob back-reference directory.
    pub fn refs_blobs_dir(&self) -> PathBuf {
        self.dir.join("refs").join("blobs")
    }

    /// Path to the generated launchers directory.
    ///
    /// `entrypoints/` is a sibling of `content/` and `refs/` under the package root.
    /// Launcher files are regular files generated at install time, not content-addressed.
    pub fn entrypoints(&self) -> PathBuf {
        self.dir.join("entrypoints")
    }
}

/// Manages the content-addressed package store on the local filesystem.
///
/// All packages are stored under a single `root` directory, sharded by
/// registry and digest (via [`super::cas_path::cas_shard_path`]).
///
/// **Repository is NOT part of the path.** Only registry + digest determine
/// the filesystem location. This enables content deduplication across
/// repositories.
///
/// Layout:
/// ```text
/// {root}/
///   {registry_slug}/
///     {algorithm}/             e.g. sha256
///       {2hex}/                first 2 hex chars of digest
///         {30hex}/             next 30 hex chars
///           content/
///           metadata.json
///           manifest.json
///           resolve.json
///           install.json
///           digest
///           refs/
///             symlinks/
///             deps/
///             layers/
///             blobs/
/// ```
#[derive(Debug, Clone)]
pub struct PackageStore {
    root: PathBuf,
}

impl PackageStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The root directory of the package store.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the package directory path for the given identifier.
    ///
    /// **Only uses registry + digest from the identifier.** The repository
    /// is intentionally ignored for content deduplication.
    pub fn path(&self, identifier: &oci::PinnedIdentifier) -> PathBuf {
        self.root
            .join(super::slugify(identifier.registry()))
            .join(super::cas_path::cas_shard_path(&identifier.digest()))
    }

    /// Returns the `content/` path for the given identifier.
    pub fn content(&self, identifier: &oci::PinnedIdentifier) -> PathBuf {
        self.path(identifier).join("content")
    }

    /// Returns the `metadata.json` path for the given identifier.
    pub fn metadata(&self, identifier: &oci::PinnedIdentifier) -> PathBuf {
        self.path(identifier).join("metadata.json")
    }

    /// Returns the `manifest.json` path for the given identifier.
    pub fn manifest(&self, identifier: &oci::PinnedIdentifier) -> PathBuf {
        self.path(identifier).join("manifest.json")
    }

    /// Returns the `resolve.json` path for the given identifier.
    pub fn resolve(&self, identifier: &oci::PinnedIdentifier) -> PathBuf {
        self.path(identifier).join("resolve.json")
    }

    /// Returns the `install.json` path for the given identifier.
    pub fn install_status(&self, identifier: &oci::PinnedIdentifier) -> PathBuf {
        self.path(identifier).join("install.json")
    }

    /// Returns the `digest` file path for the given identifier.
    pub fn digest_file(&self, identifier: &oci::PinnedIdentifier) -> PathBuf {
        self.path(identifier).join(super::cas_path::DIGEST_FILENAME)
    }

    /// Returns the `metadata.json` path for the package that owns `content_path`.
    ///
    /// `content_path` may be a real path or a symlink; symlinks are resolved
    /// before navigating to the sibling file.
    pub fn metadata_for_content(&self, content_path: &Path) -> Result<PathBuf> {
        Ok(package_dir_for_content(content_path)?.join("metadata.json"))
    }

    /// Returns the `refs/symlinks/` directory for the package that owns `content_path`.
    ///
    /// `content_path` may be a real path or a symlink; symlinks are resolved
    /// before navigating to the sibling directory.
    pub fn refs_symlinks_dir_for_content(&self, content_path: &Path) -> Result<PathBuf> {
        Ok(package_dir_for_content(content_path)?.join("refs").join("symlinks"))
    }

    /// Returns the `refs/deps/` directory for the package that owns `content_path`.
    ///
    /// `content_path` may be a real path or a symlink; symlinks are resolved
    /// before navigating to the sibling directory.
    pub fn refs_deps_dir_for_content(&self, content_path: &Path) -> Result<PathBuf> {
        Ok(package_dir_for_content(content_path)?.join("refs").join("deps"))
    }

    /// Returns the `refs/layers/` directory for the package that owns `content_path`.
    ///
    /// `content_path` may be a real path or a symlink; symlinks are resolved
    /// before navigating to the sibling directory.
    pub fn refs_layers_dir_for_content(&self, content_path: &Path) -> Result<PathBuf> {
        Ok(package_dir_for_content(content_path)?.join("refs").join("layers"))
    }

    /// Returns the `refs/blobs/` directory for the package that owns `content_path`.
    ///
    /// `content_path` may be a real path or a symlink; symlinks are resolved
    /// before navigating to the sibling directory.
    pub fn refs_blobs_dir_for_content(&self, content_path: &Path) -> Result<PathBuf> {
        Ok(package_dir_for_content(content_path)?.join("refs").join("blobs"))
    }

    /// Returns the `resolve.json` path for the package that owns `content_path`.
    ///
    /// `content_path` may be a real path or a symlink; symlinks are resolved
    /// before navigating to the sibling file.
    pub fn resolve_for_content(&self, content_path: &Path) -> Result<PathBuf> {
        Ok(package_dir_for_content(content_path)?.join("resolve.json"))
    }

    /// Returns the `entrypoints/` path for the given identifier.
    ///
    /// `entrypoints/` is a sibling of `content/` and `refs/` inside the package root.
    pub fn entrypoints(&self, identifier: &oci::PinnedIdentifier) -> PathBuf {
        self.path(identifier).join("entrypoints")
    }

    /// Returns the `digest` file path for the package that owns `content_path`.
    ///
    /// `content_path` may be a real path or a symlink; symlinks are resolved
    /// before navigating to the sibling file.
    pub fn digest_file_for_content(&self, content_path: &Path) -> Result<PathBuf> {
        Ok(package_dir_for_content(content_path)?.join(super::cas_path::DIGEST_FILENAME))
    }

    /// Lists all package directories currently present in the store.
    ///
    /// A package directory is identified by the presence of a `content/` child
    /// directory. Recursion stops at that point so that package-installed files
    /// (which may themselves contain arbitrary subdirectories) are never traversed.
    ///
    /// Returns an empty vec if the store root does not exist yet.
    pub async fn list_all(&self) -> Result<Vec<PackageDir>> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        crate::utility::fs::DirWalker::new(self.root.clone(), classify_package_dir)
            .max_depth(MAX_WALK_DEPTH)
            .walk()
            .await
    }
}

/// Resolves `path` (following any install symlinks) to the package root
/// directory. Accepts either a `content/` child directory or the package root
/// itself, so callers don't need to know which they hold.
///
/// - When `path` resolves to `packages/.../<digest>/content`, returns
///   `packages/.../<digest>` (the parent — the package root).
/// - When `path` resolves to `packages/.../<digest>` (the package root), returns
///   it unchanged. This is the shape produced by the flattened install layout
///   where `symlinks/{registry}/{repo}/current` and
///   `symlinks/{registry}/{repo}/candidates/{tag}` target the package root
///   directly and consumers traverse into `content/` or `entrypoints/` as
///   needed.
fn package_dir_for_content(path: &Path) -> Result<PathBuf> {
    let canonical = dunce::canonicalize(path).map_err(|e| Error::InternalFile(path.to_path_buf(), e))?;
    if canonical.file_name() == Some(std::ffi::OsStr::new("content")) {
        canonical
            .parent()
            .map(|p| p.to_path_buf())
            .ok_or(Error::InternalPathInvalid(canonical))
    } else {
        Ok(canonical)
    }
}

/// Registry directory + CAS shard depth (algorithm/prefix/suffix).
const MAX_WALK_DEPTH: usize = 1 + super::cas_path::CAS_SHARD_DEPTH;

/// Directory names that are part of the package layout and must not be
/// recursed into during the store walk.
const PACKAGE_SKIP_NAMES: &[&str] = &["content", "refs"];

/// Classifies a directory for the generic walker.
///
/// - If a `content/` subdirectory exists and the path is valid CAS →
///   [`WalkDecision::leaf`] with a [`PackageDir`].
/// - If `content/` exists but the path is invalid → [`WalkDecision::skip`].
/// - Otherwise → [`WalkDecision::descend_skip`], skipping `content`, `refs`.
fn classify_package_dir(dir: &Path, _depth: usize) -> crate::utility::fs::WalkDecision<PackageDir> {
    if dir.join("content").is_dir() {
        if super::cas_path::is_valid_cas_path(dir) {
            return crate::utility::fs::WalkDecision::leaf(PackageDir { dir: dir.to_path_buf() });
        }
        log::warn!("Skipping content/ dir not matching CAS layout: {}", dir.display());
        return crate::utility::fs::WalkDecision::skip();
    }
    crate::utility::fs::WalkDecision::descend_skip(PACKAGE_SKIP_NAMES)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci;

    const SHA256_HEX: &str = "43567c07f1a6b07b5e8dc052108c9d4c4a32130e18bcbd8a78c53af3e90325d9";

    fn digest() -> oci::Digest {
        oci::Digest::Sha256(SHA256_HEX.to_string())
    }

    fn pinned(registry: &str, repository: &str) -> oci::PinnedIdentifier {
        let id = oci::Identifier::new_registry(repository, registry).clone_with_digest(digest());
        oci::PinnedIdentifier::try_from(id).unwrap()
    }

    // ---- path construction ------------------------------------------------

    #[test]
    fn path_uses_only_registry_and_digest_not_repository() {
        let store = PackageStore::new("/packages");
        let id_a = pinned("example.com", "cmake");
        let id_b = pinned("example.com", "ninja");
        // Different repos, same digest and registry -> same path
        assert_eq!(store.path(&id_a), store.path(&id_b));
    }

    #[test]
    fn path_flat_registry() {
        let store = PackageStore::new("/packages");
        let id = pinned("example.com", "cmake");
        let expected = Path::new("/packages")
            .join("example.com")
            .join("sha256")
            .join("43")
            .join("567c07f1a6b07b5e8dc052108c9d4c");
        assert_eq!(store.path(&id), expected);
    }

    #[test]
    fn path_port_containing_registry_is_slugified() {
        let store = PackageStore::new("/packages");
        let id = pinned("localhost:5000", "cmake");
        let expected = Path::new("/packages")
            .join("localhost_5000")
            .join("sha256")
            .join("43")
            .join("567c07f1a6b07b5e8dc052108c9d4c");
        assert_eq!(store.path(&id), expected);
    }

    // ---- identifier-based accessors ---------------------------------------

    #[test]
    fn content_is_path_join_content() {
        let store = PackageStore::new("/packages");
        let id = pinned("example.com", "cmake");
        let p = store.content(&id);
        assert_eq!(p.file_name().unwrap(), "content");
        assert_eq!(p.parent().unwrap(), store.path(&id));
    }

    #[test]
    fn metadata_is_path_join_metadata_json() {
        let store = PackageStore::new("/packages");
        let id = pinned("example.com", "cmake");
        let p = store.metadata(&id);
        assert_eq!(p.file_name().unwrap(), "metadata.json");
        assert_eq!(p.parent().unwrap(), store.path(&id));
    }

    #[test]
    fn manifest_is_path_join_manifest_json() {
        let store = PackageStore::new("/packages");
        let id = pinned("example.com", "cmake");
        let p = store.manifest(&id);
        assert_eq!(p.file_name().unwrap(), "manifest.json");
        assert_eq!(p.parent().unwrap(), store.path(&id));
    }

    #[test]
    fn resolve_is_path_join_resolve_json() {
        let store = PackageStore::new("/packages");
        let id = pinned("example.com", "cmake");
        let p = store.resolve(&id);
        assert_eq!(p.file_name().unwrap(), "resolve.json");
        assert_eq!(p.parent().unwrap(), store.path(&id));
    }

    #[test]
    fn install_status_is_path_join_install_json() {
        let store = PackageStore::new("/packages");
        let id = pinned("example.com", "cmake");
        let p = store.install_status(&id);
        assert_eq!(p.file_name().unwrap(), "install.json");
        assert_eq!(p.parent().unwrap(), store.path(&id));
    }

    #[test]
    fn digest_file_is_path_join_digest() {
        let store = PackageStore::new("/packages");
        let id = pinned("example.com", "cmake");
        let p = store.digest_file(&id);
        assert_eq!(p.file_name().unwrap(), "digest");
        assert_eq!(p.parent().unwrap(), store.path(&id));
    }

    // ---- PackageDir accessors ---------------------------------------------

    #[test]
    fn package_dir_accessors() {
        let pkg = PackageDir {
            dir: PathBuf::from("/pkg/reg/sha256/43/rest"),
        };
        assert_eq!(pkg.content(), PathBuf::from("/pkg/reg/sha256/43/rest/content"));
        assert_eq!(pkg.metadata(), PathBuf::from("/pkg/reg/sha256/43/rest/metadata.json"));
        assert_eq!(pkg.manifest(), PathBuf::from("/pkg/reg/sha256/43/rest/manifest.json"));
        assert_eq!(pkg.resolve(), PathBuf::from("/pkg/reg/sha256/43/rest/resolve.json"));
        assert_eq!(
            pkg.install_status(),
            PathBuf::from("/pkg/reg/sha256/43/rest/install.json")
        );
        assert_eq!(pkg.digest_file(), PathBuf::from("/pkg/reg/sha256/43/rest/digest"));
        assert_eq!(
            pkg.refs_symlinks_dir(),
            PathBuf::from("/pkg/reg/sha256/43/rest/refs/symlinks")
        );
        assert_eq!(pkg.refs_deps_dir(), PathBuf::from("/pkg/reg/sha256/43/rest/refs/deps"));
        assert_eq!(
            pkg.refs_layers_dir(),
            PathBuf::from("/pkg/reg/sha256/43/rest/refs/layers")
        );
        assert_eq!(
            pkg.refs_blobs_dir(),
            PathBuf::from("/pkg/reg/sha256/43/rest/refs/blobs")
        );
    }

    // ---- *_for_content methods --------------------------------------------

    #[test]
    fn metadata_for_content_returns_sibling_metadata_json() {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let store = PackageStore::new(&root);
        let obj = root.join("obj");
        let content = obj.join("content");
        std::fs::create_dir_all(&content).unwrap();
        let result = store.metadata_for_content(&content).unwrap();
        assert_eq!(result, obj.join("metadata.json"));
    }

    #[test]
    fn refs_symlinks_dir_for_content_returns_sibling() {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let store = PackageStore::new(&root);
        let obj = root.join("obj");
        let content = obj.join("content");
        std::fs::create_dir_all(&content).unwrap();
        let result = store.refs_symlinks_dir_for_content(&content).unwrap();
        assert_eq!(result, obj.join("refs").join("symlinks"));
    }

    #[test]
    fn refs_deps_dir_for_content_returns_sibling() {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let store = PackageStore::new(&root);
        let obj = root.join("obj");
        let content = obj.join("content");
        std::fs::create_dir_all(&content).unwrap();
        let result = store.refs_deps_dir_for_content(&content).unwrap();
        assert_eq!(result, obj.join("refs").join("deps"));
    }

    #[test]
    fn refs_layers_dir_for_content_returns_sibling() {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let store = PackageStore::new(&root);
        let obj = root.join("obj");
        let content = obj.join("content");
        std::fs::create_dir_all(&content).unwrap();
        let result = store.refs_layers_dir_for_content(&content).unwrap();
        assert_eq!(result, obj.join("refs").join("layers"));
    }

    #[test]
    fn refs_blobs_dir_for_content_returns_sibling() {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let store = PackageStore::new(&root);
        let obj = root.join("obj");
        let content = obj.join("content");
        std::fs::create_dir_all(&content).unwrap();
        let result = store.refs_blobs_dir_for_content(&content).unwrap();
        assert_eq!(result, obj.join("refs").join("blobs"));
    }

    #[test]
    fn resolve_for_content_returns_sibling_resolve_json() {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let store = PackageStore::new(&root);
        let obj = root.join("obj");
        let content = obj.join("content");
        std::fs::create_dir_all(&content).unwrap();
        let result = store.resolve_for_content(&content).unwrap();
        assert_eq!(result, obj.join("resolve.json"));
    }

    #[test]
    fn metadata_for_content_accepts_package_root() {
        // After the layout flatten, install symlinks (`current`,
        // `candidates/{tag}`) target the package root rather than the
        // `content/` child. `*_for_content` must therefore accept the package
        // root and return the root's sibling files directly, without
        // climbing one level higher.
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let store = PackageStore::new(&root);
        let pkg_root = root.join("obj");
        std::fs::create_dir_all(pkg_root.join("content")).unwrap();
        let result = store.metadata_for_content(&pkg_root).unwrap();
        assert_eq!(result, pkg_root.join("metadata.json"));
    }

    #[test]
    fn metadata_for_content_follows_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let store = PackageStore::new(&root);
        let obj = root.join("obj");
        let content = obj.join("content");
        std::fs::create_dir_all(&content).unwrap();

        let link = root.join("link");
        crate::symlink::create(&content, &link).unwrap();
        let result = store.metadata_for_content(&link).unwrap();
        assert_eq!(result, obj.join("metadata.json"));
    }

    // ---- list_all ---------------------------------------------------------

    #[tokio::test]
    async fn list_all_returns_empty_when_root_absent() {
        let store = PackageStore::new("/nonexistent/path/that/does/not/exist");
        assert_eq!(store.list_all().await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn list_all_finds_single_package() {
        let dir = tempfile::tempdir().unwrap();
        let content = dir
            .path()
            .join("example.com/sha256/43/567c07f1a6b07b5e8dc052108c9d4c/content");
        std::fs::create_dir_all(&content).unwrap();

        let store = PackageStore::new(dir.path());
        let packages = store.list_all().await.unwrap();
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].content(), content);
    }

    #[tokio::test]
    async fn list_all_skips_invalid_cas_path() {
        let dir = tempfile::tempdir().unwrap();

        // Valid package
        let valid = dir
            .path()
            .join("example.com/sha256/43/567c07f1a6b07b5e8dc052108c9d4c/content");
        std::fs::create_dir_all(&valid).unwrap();

        // Invalid: wrong algorithm
        let invalid = dir
            .path()
            .join("example.com/md5/43/567c07f1a6b07b5e8dc052108c9d4c/content");
        std::fs::create_dir_all(&invalid).unwrap();

        let store = PackageStore::new(dir.path());
        let packages = store.list_all().await.unwrap();
        assert_eq!(packages.len(), 1);
    }

    #[tokio::test]
    async fn list_all_does_not_recurse_into_content_subdirs() {
        let dir = tempfile::tempdir().unwrap();
        let content = dir
            .path()
            .join("example.com/sha256/43/567c07f1a6b07b5e8dc052108c9d4c/content");
        // Nested content/ inside the package should not produce a second result
        std::fs::create_dir_all(content.join("subdir/content")).unwrap();

        let store = PackageStore::new(dir.path());
        let packages = store.list_all().await.unwrap();
        assert_eq!(packages.len(), 1);
    }

    #[tokio::test]
    async fn list_all_does_not_recurse_into_refs_subdirs() {
        let dir = tempfile::tempdir().unwrap();
        let pkg_dir = dir.path().join("example.com/sha256/43/567c07f1a6b07b5e8dc052108c9d4c");
        std::fs::create_dir_all(pkg_dir.join("content")).unwrap();
        // refs/ directories should be skipped, not descended into
        std::fs::create_dir_all(pkg_dir.join("refs/symlinks")).unwrap();
        std::fs::create_dir_all(pkg_dir.join("refs/deps")).unwrap();

        let store = PackageStore::new(dir.path());
        let packages = store.list_all().await.unwrap();
        assert_eq!(packages.len(), 1);
    }
}
