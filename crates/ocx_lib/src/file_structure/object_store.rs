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
        let digest = identifier.digest().ok_or_else(|| {
            Error::UndefinedWithMessage(format!(
                "Object store is only well-defined for identifiers with a digest, got: {}",
                identifier
            ))
        })?;
        Ok(self
            .root
            .join(identifier.reference.registry())
            .join(identifier.reference.repository())
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
        let canonical = std::fs::canonicalize(content_path)
            .map_err(|e| Error::InternalFile(content_path.to_path_buf(), e))?;
        canonical
            .parent()
            .map(|p| p.to_path_buf())
            .ok_or_else(|| Error::InternalPathInvalid(canonical))
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
        let entries = std::fs::read_dir(dir)
            .map_err(|e| Error::InternalFile(dir.to_path_buf(), e))?;
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
