// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

mod blob_store;
mod cas_path;
pub mod error;
mod layer_store;
mod package_store;
mod symlink_store;
mod tag_store;
mod temp_store;

pub use blob_store::{BlobDir, BlobGuard, BlobStore};
pub use cas_path::{CasTier, DIGEST_FILENAME, cas_ref_name, read_digest_file, write_digest_file};
pub use layer_store::{LayerDir, LayerStore};
pub use package_store::{PackageDir, PackageStore};
pub use symlink_store::{SymlinkKind, SymlinkStore};
pub use tag_store::TagStore;
pub use temp_store::{StaleEntry, TempAcquireResult, TempDir, TempEntry, TempStore};

/// Root layout of the local OCX data directory.
///
/// `FileStructure` is a thin composite that provides typed, well-named access
/// to six top-level stores:
///
/// - **`blobs`**    — content-addressed raw blob store
/// - **`layers`**   — content-addressed extracted layer store
/// - **`packages`** — content-addressed package store (content, metadata, refs)
/// - **`tags`**     — tag-to-digest mapping store (local index)
/// - **`symlinks`** — install symlinks (candidate / current)
/// - **`temp`**     — temporary staging directories for in-progress downloads
///
/// Default root: `~/.ocx` (resolved via [`default_ocx_root`]).
#[derive(Debug, Clone)]
pub struct FileStructure {
    root: std::path::PathBuf,
    pub blobs: BlobStore,
    pub layers: LayerStore,
    pub packages: PackageStore,
    pub tags: TagStore,
    pub symlinks: SymlinkStore,
    pub temp: TempStore,
}

impl Default for FileStructure {
    fn default() -> Self {
        Self::new()
    }
}

impl FileStructure {
    /// Creates a `FileStructure` rooted at the default OCX data directory (`~/.ocx`).
    pub fn new() -> Self {
        let root = default_ocx_root().expect("Could not determine default OCX root directory.");
        Self::with_root(root)
    }

    /// Creates a `FileStructure` rooted at `root`.
    pub fn with_root(root: std::path::PathBuf) -> Self {
        Self {
            blobs: BlobStore::new(root.join("blobs")),
            layers: LayerStore::new(root.join("layers")),
            packages: PackageStore::new(root.join("packages")),
            tags: TagStore::new(root.join("tags")),
            symlinks: SymlinkStore::new(root.join("symlinks")),
            temp: TempStore::new(root.join("temp")),
            root,
        }
    }

    /// Returns the root directory of this file structure (e.g., `~/.ocx`).
    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    /// Returns the path to the profile manifest file (`$OCX_HOME/profile.json`).
    pub fn profile_manifest(&self) -> std::path::PathBuf {
        self.root.join("profile.json")
    }
}

/// Returns the OCX data root directory.
///
/// Resolution order:
/// 1. `OCX_HOME` environment variable (if set and non-empty)
/// 2. `~/.ocx` (fallback)
pub fn default_ocx_root() -> Option<std::path::PathBuf> {
    if let Ok(home) = std::env::var("OCX_HOME")
        && !home.is_empty()
    {
        return Some(std::path::PathBuf::from(home));
    }
    std::env::home_dir().map(|home| home.join(".ocx"))
}

use std::path::PathBuf;

use crate::prelude::StringExt;

/// Convert an OCI identifier component (registry, repository, tag) into a
/// filesystem-safe path segment using [`StringExt::to_relaxed_slug`].
pub(crate) fn slugify(value: &str) -> String {
    value.to_relaxed_slug()
}

/// Converts an OCI repository name into a relative path with OS-native separators.
///
/// Repository names can contain `/` for nested repos (e.g. `org/project/tool`).
/// Each segment becomes a separate path component, ensuring native separators
/// on all platforms — `PathBuf::join("a/b")` embeds the literal `/` which
/// produces mixed separators on Windows.
pub(crate) fn repository_path(repository: &str) -> PathBuf {
    repository.split('/').collect()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn repository_path_single_segment() {
        assert_eq!(repository_path("cmake"), Path::new("cmake"));
    }

    #[test]
    fn repository_path_two_segments() {
        let expected = Path::new("org").join("cmake");
        assert_eq!(repository_path("org/cmake"), expected);
    }

    #[test]
    fn repository_path_three_segments() {
        let expected = Path::new("a").join("b").join("c");
        assert_eq!(repository_path("a/b/c"), expected);
    }
}
