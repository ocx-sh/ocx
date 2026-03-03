use std::path::PathBuf;

use crate::oci;

/// Selects which install symlink path [`InstallStore`] should return.
///
/// - `Candidate` — the tag-pinned symlink written by `ocx install`.
/// - `Current`   — the selection symlink written by `ocx install --select`,
///   analogous to `update-alternatives --set` on Linux.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SymlinkKind {
    Candidate,
    Current,
}

/// Manages install symlinks for installed packages.
///
/// Install symlinks provide stable, human-readable paths to package content
/// that remain constant across version upgrades, making them safe to embed in
/// shell profiles or toolchain configurations.
///
/// Layout:
/// ```text
/// {root}/
///   {registry}/
///     {repository}/
///       current               — symlink (written by `ocx select`)
///       candidates/
///         {tag}               — symlink (written by `ocx install`)
/// ```
#[derive(Debug, Clone)]
pub struct InstallStore {
    root: PathBuf,
}

impl InstallStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Returns the base directory for all symlinks belonging to the given identifier.
    fn base(&self, identifier: &oci::Identifier) -> PathBuf {
        self.root
            .join(super::slugify(identifier.reference.registry()))
            .join(identifier.reference.repository())
    }

    /// Returns the `current` symlink path for the given identifier.
    pub fn current(&self, identifier: &oci::Identifier) -> PathBuf {
        self.base(identifier).join("current")
    }

    /// Returns the `candidates/` directory path for the given identifier.
    pub fn candidates(&self, identifier: &oci::Identifier) -> PathBuf {
        self.base(identifier).join("candidates")
    }

    /// Returns the candidate symlink path for the given identifier and tag.
    pub fn candidate(&self, identifier: &oci::Identifier) -> PathBuf {
        self.candidates(identifier).join(identifier.tag_or_latest())
    }

    /// Returns the symlink path selected by `kind`.
    pub fn symlink(&self, identifier: &oci::Identifier, kind: SymlinkKind) -> PathBuf {
        match kind {
            SymlinkKind::Candidate => self.candidate(identifier),
            SymlinkKind::Current => self.current(identifier),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci;

    fn id_with_tag() -> oci::Identifier {
        oci::Identifier::new_registry("cmake", "example.com").clone_with_tag("3.28")
    }

    fn id_no_tag() -> oci::Identifier {
        oci::Identifier::new_registry("cmake", "example.com")
    }

    // ── path structure ────────────────────────────────────────────────────────

    #[test]
    fn current_path_structure() {
        let store = InstallStore::new("/installs");
        assert_eq!(
            store.current(&id_with_tag()),
            PathBuf::from("/installs/example.com/cmake/current")
        );
    }

    #[test]
    fn candidates_dir_path_structure() {
        let store = InstallStore::new("/installs");
        assert_eq!(
            store.candidates(&id_with_tag()),
            PathBuf::from("/installs/example.com/cmake/candidates")
        );
    }

    #[test]
    fn candidate_uses_tag() {
        let store = InstallStore::new("/installs");
        assert_eq!(
            store.candidate(&id_with_tag()),
            PathBuf::from("/installs/example.com/cmake/candidates/3.28")
        );
    }

    #[test]
    fn candidate_falls_back_to_latest_when_no_tag() {
        let store = InstallStore::new("/installs");
        assert_eq!(
            store.candidate(&id_no_tag()),
            PathBuf::from("/installs/example.com/cmake/candidates/latest")
        );
    }

    // ── symlink dispatch ──────────────────────────────────────────────────────

    #[test]
    fn symlink_candidate_returns_candidate_path() {
        let store = InstallStore::new("/installs");
        assert_eq!(
            store.symlink(&id_with_tag(), SymlinkKind::Candidate),
            store.candidate(&id_with_tag()),
        );
    }

    #[test]
    fn symlink_current_returns_current_path() {
        let store = InstallStore::new("/installs");
        assert_eq!(
            store.symlink(&id_with_tag(), SymlinkKind::Current),
            store.current(&id_with_tag()),
        );
    }
}
