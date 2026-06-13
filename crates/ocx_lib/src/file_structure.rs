// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

mod blob_store;
mod cas_path;
pub mod error;
mod layer_store;
mod package_store;
mod state_store;
mod store_layout;
mod symlink_store;
mod tag_store;
mod temp_store;

pub use blob_store::{BlobDir, BlobStore};
#[cfg(test)]
pub(crate) use blob_store::{WRITE_BLOB_CALL_COUNT, WRITE_BLOB_TEST_LOCK};
pub use cas_path::{CasTier, DIGEST_FILENAME, cas_ref_name, read_digest_file, write_digest_file};
pub use layer_store::{LayerDir, LayerStore};
pub use package_store::{PackageDir, PackageStore};
pub use state_store::StateStore;
pub use store_layout::StoreLayout;
pub use symlink_store::{SymlinkKind, SymlinkStore};
pub use tag_store::TagStore;
pub use temp_store::{StaleEntry, TempAcquireResult, TempDir, TempEntry, TempStore};

/// Root layout of the local OCX data directory.
///
/// `FileStructure` is a thin composite that provides typed, well-named access
/// to the top-level stores:
///
/// - **`blobs`**      — content-addressed raw blob store
/// - **`layers`**     — content-addressed extracted layer store
/// - **`packages`**   — content-addressed package store (content, metadata, refs)
/// - **`tags`**       — tag-to-digest mapping store (local index)
/// - **`symlinks`**   — install symlinks (candidate / current)
/// - **`state`**      — persistent runtime state (update-check timestamps, etc.)
/// - **`temp`**       — package-zone staging directories for in-progress downloads
/// - **`layer_temp`** — cache-zone staging directories for in-progress layer extractions
///
/// In the default single-root layout (`OCX_CACHE_DIR`, `OCX_PACKAGES_DIR`, and
/// `OCX_STATE_DIR` all unset), `temp` and `layer_temp` point at the same
/// directory (`$OCX_HOME/temp`). When zone overrides are in effect, each temp
/// store is co-located with its tier so that every publish is an intra-volume
/// atomic rename.
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
    pub state: StateStore,
    /// Package-zone staging temp. Co-located with `packages/` so the final
    /// rename is always intra-volume.
    pub temp: TempStore,
    /// Cache-zone staging temp for layer extractions. Co-located with
    /// `layers/` so layer publish is always an intra-volume rename.
    ///
    /// In the default single-root layout this is the same directory as
    /// `temp`; they diverge only when `OCX_CACHE_DIR` ≠ `OCX_PACKAGES_DIR`.
    pub layer_temp: TempStore,
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
    ///
    /// All stores are derived as `root.join(<name>)`. This is the pre-M2
    /// single-root layout; `layer_temp` is initialized to the same directory
    /// as `temp` so the invariant "each temp is co-located with its tier"
    /// holds trivially in the unified-zone case.
    pub fn with_root(root: std::path::PathBuf) -> Self {
        Self {
            blobs: BlobStore::new(root.join("blobs")),
            layers: LayerStore::new(root.join("layers")),
            packages: PackageStore::new(root.join("packages")),
            tags: TagStore::new(root.join("tags")),
            symlinks: SymlinkStore::new(root.join("symlinks")),
            state: StateStore::new(root.join("state")),
            temp: TempStore::new(root.join("temp")),
            layer_temp: TempStore::new(root.join("temp")),
            root,
        }
    }

    /// Creates a `FileStructure` from a pre-resolved [`StoreLayout`].
    ///
    /// Each store is rooted at the zone directory prescribed by the layout:
    ///
    /// - `blobs`, `layers`, `layer_temp` → cache zone (`layout.cache()`)
    /// - `packages`, `temp`              → packages zone (`layout.packages_root()`)
    /// - `tags`                          → `layout.tags_root()` or `{cache}/tags`
    /// - `symlinks`, `state`             → state zone (`layout.state()`)
    ///
    /// When all zone overrides are absent (the default), this produces the
    /// same layout as `with_root($OCX_HOME)`.
    pub fn with_layout(_layout: StoreLayout) -> Self {
        unimplemented!()
    }

    /// Returns the root directory of this file structure (e.g., `~/.ocx`).
    pub fn root(&self) -> &std::path::Path {
        &self.root
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

    // ── with_root_derives_seven_stores (M2 baseline characterization) ─────────
    //
    // CHARACTERIZATION TEST — locks the current (pre-M2) single-root layout.
    //
    // Today `FileStructure::with_root(root)` derives all seven stores directly
    // as `root.join(<name>)`.  The M2 `StoreLayout` resolver changes this by
    // adding zone overrides (`OCX_CACHE_DIR`, `OCX_STATE_DIR`, `OCX_PACKAGES_DIR`),
    // but the **default** (unset) behaviour must stay byte-identical to today —
    // `with_root` becomes a shim for `with_layout(StoreLayout::from_root(root))`.
    //
    // Requirement traced to: system_design_shared_store.md §5 M2, plan_shared_store P1.1
    // ("with_root == with_layout(from_root) parity").
    #[test]
    fn with_root_derives_seven_stores() {
        let root = std::path::PathBuf::from("/home/user/.ocx");
        let fs = FileStructure::with_root(root.clone());

        assert_eq!(fs.blobs.root(), root.join("blobs"), "blobs store must be root/blobs");
        assert_eq!(
            fs.layers.root(),
            root.join("layers"),
            "layers store must be root/layers"
        );
        assert_eq!(
            fs.packages.root(),
            root.join("packages"),
            "packages store must be root/packages"
        );
        assert_eq!(fs.tags.root(), root.join("tags"), "tags store must be root/tags");
        assert_eq!(
            fs.symlinks.root(),
            root.join("symlinks"),
            "symlinks store must be root/symlinks"
        );
        assert_eq!(fs.state.root(), root.join("state"), "state store must be root/state");
        assert_eq!(fs.temp.root(), root.join("temp"), "temp store must be root/temp");
        // root() accessor round-trips.
        assert_eq!(fs.root(), root.as_path(), "root() must return the original root");
    }
}
