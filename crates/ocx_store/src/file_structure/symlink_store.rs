// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::{Path, PathBuf};

/// `Candidate` is the tag-pinned install link; `Current` the `--select` link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SymlinkKind {
    Candidate,
    Current,
}

/// Stable per-repository links to package roots (never `content/`), safe to embed in shell profiles.
///
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

    fn base(&self, identifier: &ocx_oci::PackageRef) -> PathBuf {
        self.root
            .join(super::slugify(identifier.registry()))
            .join(super::repository_path(identifier.repository()))
    }

    pub fn current(&self, identifier: &ocx_oci::PackageRef) -> PathBuf {
        self.base(identifier).join("current")
    }

    pub fn candidates(&self, identifier: &ocx_oci::PackageRef) -> PathBuf {
        self.base(identifier).join("candidates")
    }

    pub fn candidate(&self, identifier: &ocx_oci::PackageRef) -> PathBuf {
        self.candidates(identifier).join(identifier.tag_or_latest())
    }

    pub fn symlink(&self, identifier: &ocx_oci::PackageRef, kind: SymlinkKind) -> PathBuf {
        match kind {
            SymlinkKind::Candidate => self.candidate(identifier),
            SymlinkKind::Current => self.current(identifier),
        }
    }

    /// Per-repo lock every `current` rewrite takes, or a rewrite interleaves with the entry-points index update.
    pub fn select_lock(&self, identifier: &ocx_oci::PackageRef) -> PathBuf {
        self.base(identifier).join(".select.lock")
    }

    pub fn registry_dir(&self, registry: &str) -> PathBuf {
        self.root.join(super::slugify(registry))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id_with_tag() -> ocx_oci::PackageRef {
        ocx_oci::PackageRef::new_registry("cmake", "example.com").clone_with_tag("3.28")
    }

    fn id_nested_with_tag() -> ocx_oci::PackageRef {
        ocx_oci::PackageRef::new_registry("org/sub/pkg", "example.com").clone_with_tag("1.0")
    }

    fn id_no_tag() -> ocx_oci::PackageRef {
        ocx_oci::PackageRef::new_registry("cmake", "example.com")
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

    // ── collision check — drives entrypoint collision check ───────────────
    // Tests for select-time collision check live in tasks/install.rs tests.
}
