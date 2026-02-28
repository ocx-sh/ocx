use std::path::PathBuf;

use crate::oci;

/// Selects which install symlink path [`InstallStore`] should return.
///
/// - `Candidate` — the tag-pinned symlink written by `ocx install`.
/// - `Current`   — the selection symlink written by `ocx install --select`,
///                 analogous to `update-alternatives --set` on Linux.
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
            .join(identifier.reference.registry())
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
