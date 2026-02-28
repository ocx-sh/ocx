use crate::{
    Result,
    oci
};

/// Selects which install symlink path [`FileStructure`] should return.
///
/// - `Candidate` — the tag-pinned symlink written by `ocx install`.
/// - `Current`   — the selection symlink written by `ocx install --select`,
///                 analogous to `update-alternatives --set` on Linux.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SymlinkKind {
    Candidate,
    Current,
}

#[derive(Debug, Clone)]
pub struct FileStructure {
    root: std::path::PathBuf,
}

impl Default for FileStructure {
    fn default() -> Self {
        Self::new()
    }
}

impl FileStructure {
    pub fn new() -> Self {
        Self {
            root: default_ocx_root().expect("Could not determine default OCX root directory."),
        }
    }

    pub fn with_root(root: std::path::PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &std::path::PathBuf {
        &self.root
    }

    pub fn objects(&self) -> std::path::PathBuf {
        self.root.join("objects")
    }

    pub fn object(&self, identifier: &oci::Identifier) -> Result<std::path::PathBuf> {
        let digest = identifier.digest().ok_or_else(|| {
            crate::error::Error::UndefinedWithMessage(format!(
                "Object store is only well-defined for identifiers with sha256 digest, got identifier with reference: {}",
                identifier
            ))
        })?;
        Ok(self.objects().join(format!(
            "{}/{}",
            identifier.reference.registry(),
            identifier.reference.repository(),
        )).join(digest.to_path()))
    }

    pub fn object_metadata(&self, identifier: &oci::Identifier) -> Result<std::path::PathBuf> {
        Ok(self.object(identifier)?.join("metadata.json"))
    }

    pub fn object_content(&self, identifier: &oci::Identifier) -> Result<std::path::PathBuf> {
        Ok(self.object(identifier)?.join("content"))
    }

    /// Resolves `content_path` (following symlinks) and returns the object root
    /// directory that contains it.  Used internally to derive sibling paths such
    /// as `metadata.json` and `refs/`.
    fn object_dir_for_content(&self, content_path: &std::path::Path) -> Result<std::path::PathBuf> {
        let canonical = std::fs::canonicalize(content_path)
            .map_err(|e| crate::error::Error::InternalFile(content_path.to_path_buf(), e))?;
        canonical.parent()
            .map(|p| p.to_path_buf())
            .ok_or_else(|| crate::error::Error::InternalPathInvalid(canonical))
    }

    /// Returns the `metadata.json` path for the object that owns `content_path`.
    ///
    /// `content_path` may be a real path or a symlink; symlinks are resolved
    /// before navigating to the sibling file.
    pub fn object_metadata_for_content(&self, content_path: &std::path::Path) -> Result<std::path::PathBuf> {
        Ok(self.object_dir_for_content(content_path)?.join("metadata.json"))
    }

    /// Returns the `refs/` directory for the object that owns `content_path`.
    ///
    /// `content_path` may be a real path or a symlink; symlinks are resolved
    /// before navigating to the sibling directory.
    pub fn object_refs_dir(&self, content_path: &std::path::Path) -> Result<std::path::PathBuf> {
        Ok(self.object_dir_for_content(content_path)?.join("refs"))
    }

    pub fn indices(&self) -> std::path::PathBuf {
        self.root.join("index")
    }

    pub fn installs(&self) -> std::path::PathBuf {
        self.root.join("installs")
    }

    pub fn install(&self, identifier: &oci::Identifier) -> std::path::PathBuf {
        self.installs().join(format!(
            "{}/{}",
            identifier.reference.registry(),
            identifier.reference.repository(),
        ))
    }

    pub fn install_current(&self, identifier: &oci::Identifier) -> std::path::PathBuf {
        self.install(identifier).join("current")
    }

    pub fn install_candidates(&self, identifier: &oci::Identifier) -> std::path::PathBuf {
        self.install(identifier).join("candidates")
    }

    pub fn install_candidate(&self, identifier: &oci::Identifier) -> std::path::PathBuf {
        self.install_candidates(identifier).join(identifier.tag_or_latest())
    }

    /// Returns the symlink path for `identifier` selected by `kind`.
    pub fn install_symlink(&self, identifier: &oci::Identifier, kind: SymlinkKind) -> std::path::PathBuf {
        match kind {
            SymlinkKind::Candidate => self.install_candidate(identifier),
            SymlinkKind::Current => self.install_current(identifier),
        }
    }
}

pub fn default_ocx_root() -> Option<std::path::PathBuf> {
    std::env::home_dir().map(|home| home.join(".ocx"))
}
