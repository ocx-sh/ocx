// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::{Path, PathBuf};

use crate::oci;

/// Selects which install symlink path [`SymlinkStore`] should return.
///
/// - `Candidate` — the tag-pinned symlink written by `ocx install`.
/// - `Current`   — the selection symlink written by `ocx install --select`,
///   analogous to `update-alternatives --set` on Linux.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SymlinkKind {
    Candidate,
    Current,
}

/// Manages symlinks for installed packages.
///
/// Symlink store provides stable, human-readable paths to package roots
/// that remain constant across version upgrades, making them safe to embed in
/// shell profiles or toolchain configurations. Both `current` and
/// `candidates/{tag}` target the **package root** (not `content/`) so a single
/// per-repo symlink covers every consumer: tools that need files traverse
/// `<symlink>/content`, shell PATH integrations traverse `<symlink>/entrypoints`,
/// and metadata consumers read `<symlink>/metadata.json`.
///
/// Layout:
/// ```text
/// {root}/
///   {registry}/
///     {repository}/
///       current               — symlink → packages/{...}/        (package root)
///       candidates/
///         {tag}               — symlink → packages/{...}/        (package root)
/// ```
#[derive(Debug, Clone)]
pub struct SymlinkStore {
    root: PathBuf,
}

impl SymlinkStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the base directory for all symlinks belonging to the given identifier.
    fn base(&self, identifier: &oci::Identifier) -> PathBuf {
        self.root
            .join(super::slugify(identifier.registry()))
            .join(super::repository_path(identifier.repository()))
    }

    /// Returns the `current` symlink path for the given identifier.
    ///
    /// Targets the package root: `packages/{registry}/{algorithm}/{2hex}/{30hex}`.
    /// Consumers traverse into `<current>/content/`, `<current>/entrypoints/`,
    /// or `<current>/metadata.json` as needed.
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

    /// Returns the per-repo selection lock path: `{base}/.select.lock`.
    ///
    /// Serializes `current` symlink updates across `install --select`,
    /// `deselect`, `uninstall --deselect`, and the standalone `select`
    /// command so concurrent invocations cannot interleave a symlink
    /// rewrite with the per-registry entry-points index update.
    pub fn select_lock(&self, identifier: &oci::Identifier) -> PathBuf {
        self.base(identifier).join(".select.lock")
    }

    /// Returns the per-registry directory: `{root}/{registry_slug}/`.
    ///
    /// Used by the entry-points index file and lock, which live one level
    /// above the per-repo `current` symlinks.
    pub fn registry_dir(&self, registry: &str) -> PathBuf {
        self.root.join(super::slugify(registry))
    }

    /// Returns the per-registry entry-points index file path:
    /// `{root}/{registry_slug}/.entrypoints-index.json`.
    ///
    /// The index is the canonical source of truth for "which currently-selected
    /// package owns launcher name X across all repos under this registry."
    /// Mediates collision detection across repos under a single registry.
    pub fn entrypoints_index(&self, registry: &str) -> PathBuf {
        self.registry_dir(registry).join(".entrypoints-index.json")
    }

    /// Returns the per-registry entry-points index lock path:
    /// `{root}/{registry_slug}/.entrypoints-index.lock`.
    ///
    /// The index lock is a coarser lock than `.select.lock`: it serializes the
    /// full read → collision-check → atomic-pair-update → index-write sequence
    /// across all repos in a registry. Lock order MUST be `index_lock` →
    /// `select_lock`; the reverse risks deadlock under concurrent installs.
    pub fn entrypoints_index_lock(&self, registry: &str) -> PathBuf {
        self.registry_dir(registry).join(".entrypoints-index.lock")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci;

    fn id_with_tag() -> oci::Identifier {
        oci::Identifier::new_registry("cmake", "example.com").clone_with_tag("3.28")
    }

    fn id_nested_with_tag() -> oci::Identifier {
        oci::Identifier::new_registry("org/sub/pkg", "example.com").clone_with_tag("1.0")
    }

    fn id_no_tag() -> oci::Identifier {
        oci::Identifier::new_registry("cmake", "example.com")
    }

    // ── path structure ────────────────────────────────────────────────────────

    #[test]
    fn current_path_structure() {
        let store = SymlinkStore::new("/symlinks");
        assert_eq!(
            store.current(&id_with_tag()),
            PathBuf::from("/symlinks/example.com/cmake/current")
        );
    }

    #[test]
    fn candidates_dir_path_structure() {
        let store = SymlinkStore::new("/symlinks");
        assert_eq!(
            store.candidates(&id_with_tag()),
            PathBuf::from("/symlinks/example.com/cmake/candidates")
        );
    }

    #[test]
    fn candidate_uses_tag() {
        let store = SymlinkStore::new("/symlinks");
        assert_eq!(
            store.candidate(&id_with_tag()),
            PathBuf::from("/symlinks/example.com/cmake/candidates/3.28")
        );
    }

    #[test]
    fn candidate_falls_back_to_latest_when_no_tag() {
        let store = SymlinkStore::new("/symlinks");
        assert_eq!(
            store.candidate(&id_no_tag()),
            PathBuf::from("/symlinks/example.com/cmake/candidates/latest")
        );
    }

    // ── nested repository ─────────────────────────────────────────────────

    #[test]
    fn current_path_nested_repo() {
        let store = SymlinkStore::new("/symlinks");
        let expected = PathBuf::from("/symlinks")
            .join("example.com")
            .join("org")
            .join("sub")
            .join("pkg")
            .join("current");
        assert_eq!(store.current(&id_nested_with_tag()), expected);
    }

    #[test]
    fn candidate_path_nested_repo() {
        let store = SymlinkStore::new("/symlinks");
        let expected = PathBuf::from("/symlinks")
            .join("example.com")
            .join("org")
            .join("sub")
            .join("pkg")
            .join("candidates")
            .join("1.0");
        assert_eq!(store.candidate(&id_nested_with_tag()), expected);
    }

    // ── symlink dispatch ──────────────────────────────────────────────────

    #[test]
    fn symlink_candidate_returns_candidate_path() {
        let store = SymlinkStore::new("/symlinks");
        assert_eq!(
            store.symlink(&id_with_tag(), SymlinkKind::Candidate),
            store.candidate(&id_with_tag()),
        );
    }

    #[test]
    fn symlink_current_returns_current_path() {
        let store = SymlinkStore::new("/symlinks");
        assert_eq!(
            store.symlink(&id_with_tag(), SymlinkKind::Current),
            store.current(&id_with_tag()),
        );
    }

    // ── root accessor ─────────────────────────────────────────────────────

    #[test]
    fn root_returns_store_root() {
        let store = SymlinkStore::new("/symlinks");
        assert_eq!(store.root(), Path::new("/symlinks"));
    }

    // ── collision check — drives check_entry_point_collisions stub ────────
    // Tests for select-time collision check live in tasks/install.rs tests.
    // The collision-check function is in tasks/install.rs (check_entry_point_collisions).
}
