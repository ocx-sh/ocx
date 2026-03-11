// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

mod index_store;
mod install_store;
mod object_store;
mod temp_store;

pub use index_store::IndexStore;
pub use install_store::{InstallStore, SymlinkKind};
pub use object_store::{ObjectDir, ObjectStore};
pub use temp_store::{TempAcquireResult, TempDir, TempStore};

/// Root layout of the local OCX data directory.
///
/// `FileStructure` is a thin composite that provides typed, well-named access
/// to each of the three top-level stores:
///
/// - **`objects`**  — content-addressed binary store (immutable blobs)
/// - **`index`**    — cached OCI index (tags, manifests)
/// - **`installs`** — install symlinks (candidate / current)
///
/// Callers that only need one of the sub-stores can receive an individual
/// `ObjectStore`, `IndexStore`, or `InstallStore` reference instead.
///
/// Default root: `~/.ocx` (resolved via [`default_ocx_root`]).
#[derive(Debug, Clone)]
pub struct FileStructure {
    pub objects: ObjectStore,
    pub index: IndexStore,
    pub installs: InstallStore,
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
            objects: ObjectStore::new(root.join("objects")),
            index: IndexStore::new(root.join("index")),
            installs: InstallStore::new(root.join("installs")),
            temp: TempStore::new(root.join("temp")),
        }
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

use crate::prelude::StringExt;

/// Convert an OCI identifier component (registry, repository, tag) into a
/// filesystem-safe path segment using [`StringExt::to_relaxed_slug`].
pub(crate) fn slugify(value: &str) -> String {
    value.to_relaxed_slug()
}
