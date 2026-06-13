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
    /// Creates a `FileStructure` from the ambient environment.
    ///
    /// `$OCX_HOME` (or `~/.ocx`) resolves the home root, then the zone-override
    /// env vars (`OCX_CACHE_DIR`, `OCX_PACKAGES_DIR`, `OCX_STATE_DIR`,
    /// `OCX_INDEX`) are applied via [`StoreLayout::resolve_from_env`]. When no
    /// zone overrides are set this is identical to `with_root($OCX_HOME)`. This
    /// is the env-aware constructor used by `ocx_lib` consumers (e.g.
    /// `self activate`'s install-symlink bin path) that build a `FileStructure`
    /// without going through the CLI option layer.
    pub fn new() -> Self {
        let root = default_ocx_root().expect("Could not determine default OCX root directory.");
        Self::with_layout(StoreLayout::resolve_from_env(root))
    }

    /// Creates a `FileStructure` rooted at `root`.
    ///
    /// All stores are derived as `root.join(<name>)`. This is the single-root
    /// layout; it is now a thin shim over
    /// [`with_layout(StoreLayout::from_root(root))`](Self::with_layout), which
    /// keeps the ~25 existing call sites untouched while routing every store
    /// derivation through the one zone resolver.
    pub fn with_root(root: std::path::PathBuf) -> Self {
        Self::with_layout(StoreLayout::from_root(root))
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
    pub fn with_layout(layout: StoreLayout) -> Self {
        let cache = layout.cache();
        let packages_root = layout.packages_root();
        let state = layout.state();
        // tags ▸ explicit OCX_INDEX override, else `{cache}/tags`.
        let tags_root = layout
            .tags_root()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| cache.join("tags"));
        Self {
            blobs: BlobStore::new(cache.join("blobs")),
            layers: LayerStore::new(cache.join("layers")),
            packages: PackageStore::new(packages_root.join("packages")),
            tags: TagStore::new(tags_root),
            symlinks: SymlinkStore::new(state.join("symlinks")),
            state: StateStore::new(state.join("state")),
            temp: TempStore::new(packages_root.join("temp")),
            layer_temp: TempStore::new(cache.join("temp")),
            root: layout.home().to_path_buf(),
        }
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

    // ── with_layout_maps_seven_stores_to_zones (P1.2 specification) ──────────
    //
    // Requirement: system_design_shared_store.md §5 M2 —
    // "blobs, layers, layer_temp → cache zone; packages, temp → packages zone;
    //  tags → tags_root or {cache}/tags; symlinks, state → state zone."
    //
    // `FileStructure::with_layout(layout)` must wire each store's root to the
    // correct zone from the layout.  This test uses distinct paths per zone so
    // any mis-assignment is immediately visible.
    //
    // Traced to: plan_shared_store P1.2 "with_layout_maps_seven_stores_to_zones".
    #[test]
    fn with_layout_maps_seven_stores_to_zones() {
        let home = std::path::PathBuf::from("/home/user/.ocx");
        let cache = std::path::PathBuf::from("/mnt/shared/cache");
        let packages = std::path::PathBuf::from("/ephemeral/packages");
        let state = std::path::PathBuf::from("/home/user/.ocx-local");
        let index = std::path::PathBuf::from("/custom/index");

        let layout = StoreLayout::resolve(
            home.clone(),
            Some(cache.clone()),
            Some(packages.clone()),
            Some(state.clone()),
            Some(index.clone()),
        );
        let fs = FileStructure::with_layout(layout);

        // Cache zone: blobs, layers, layer_temp
        assert_eq!(
            fs.blobs.root(),
            cache.join("blobs"),
            "blobs must be under the cache zone"
        );
        assert_eq!(
            fs.layers.root(),
            cache.join("layers"),
            "layers must be under the cache zone"
        );
        assert_eq!(
            fs.layer_temp.root(),
            cache.join("temp"),
            "layer_temp must be under the cache zone"
        );

        // Packages zone: packages store, package staging temp
        assert_eq!(
            fs.packages.root(),
            packages.join("packages"),
            "packages store must be under the packages zone"
        );
        assert_eq!(
            fs.temp.root(),
            packages.join("temp"),
            "package staging temp must be under the packages zone"
        );

        // Index: explicit override wins
        assert_eq!(
            fs.tags.root(),
            index.as_path(),
            "tags store must use the explicit OCX_INDEX override"
        );

        // State zone: symlinks, state
        assert_eq!(
            fs.symlinks.root(),
            state.join("symlinks"),
            "symlinks must be under the state zone"
        );
        assert_eq!(
            fs.state.root(),
            state.join("state"),
            "state store must be under the state zone"
        );
    }

    // ── with_root_is_with_layout_from_root (P1.2 specification) ──────────────
    //
    // Requirement: §5 M2 — "`with_root` becomes `with_layout(StoreLayout::from_root(root))`".
    // Every store root produced by `with_root(r)` must equal the root produced
    // by `with_layout(StoreLayout::from_root(r))` — bit-identical layout.
    //
    // Traced to: plan_shared_store P1.2 "with_root == with_layout(from_root) parity".
    #[test]
    fn with_root_is_with_layout_from_root() {
        let root = std::path::PathBuf::from("/home/user/.ocx");

        let via_root = FileStructure::with_root(root.clone());
        let via_layout = FileStructure::with_layout(StoreLayout::from_root(root));

        assert_eq!(
            via_root.blobs.root(),
            via_layout.blobs.root(),
            "blobs root must match between with_root and with_layout(from_root)"
        );
        assert_eq!(
            via_root.layers.root(),
            via_layout.layers.root(),
            "layers root must match"
        );
        assert_eq!(
            via_root.packages.root(),
            via_layout.packages.root(),
            "packages root must match"
        );
        assert_eq!(via_root.tags.root(), via_layout.tags.root(), "tags root must match");
        assert_eq!(
            via_root.symlinks.root(),
            via_layout.symlinks.root(),
            "symlinks root must match"
        );
        assert_eq!(via_root.state.root(), via_layout.state.root(), "state root must match");
        assert_eq!(via_root.temp.root(), via_layout.temp.root(), "temp root must match");
        assert_eq!(
            via_root.layer_temp.root(),
            via_layout.layer_temp.root(),
            "layer_temp root must match"
        );
        assert_eq!(via_root.root(), via_layout.root(), "root() must match");
    }

    // ── package_temp_under_packages_zone_layer_temp_under_cache_zone ──────────
    //
    // Requirement: §5 M2 — "layer_temp → cache zone; temp → packages zone".
    // When zones are split (OCX_CACHE_DIR ≠ OCX_PACKAGES_DIR), each temp
    // store must co-locate with its tier so every publish is an intra-volume
    // atomic rename.
    //
    // Traced to: plan_shared_store P1.2 "package_temp_under_packages_zone_layer_temp_under_cache_zone".
    #[test]
    fn package_temp_under_packages_zone_layer_temp_under_cache_zone() {
        let home = std::path::PathBuf::from("/home/user/.ocx");
        let cache = std::path::PathBuf::from("/mnt/shared/cache");
        let packages = std::path::PathBuf::from("/ephemeral/packages");

        let layout = StoreLayout::resolve(home, Some(cache.clone()), Some(packages.clone()), None, None);
        let fs = FileStructure::with_layout(layout);

        // package staging temp must co-locate with packages zone
        assert!(
            fs.temp.root().starts_with(&packages),
            "package temp ({}) must be under the packages zone ({})",
            fs.temp.root().display(),
            packages.display()
        );
        // layer staging temp must co-locate with cache zone
        assert!(
            fs.layer_temp.root().starts_with(&cache),
            "layer_temp ({}) must be under the cache zone ({})",
            fs.layer_temp.root().display(),
            cache.display()
        );
        // They must be distinct directories when zones differ
        assert_ne!(
            fs.temp.root(),
            fs.layer_temp.root(),
            "temp and layer_temp must differ when cache and packages zones differ"
        );
    }

    // ── temps_collapse_when_zones_unified ─────────────────────────────────────
    //
    // Requirement: §5 M2 — "When zones unified, temp/layer_temp point at the
    // same dir — identical to today."  In the default single-root layout both
    // temp stores must share a single directory.
    //
    // Traced to: plan_shared_store P1.2 "temps_collapse_when_zones_unified".
    #[test]
    fn temps_collapse_when_zones_unified() {
        let root = std::path::PathBuf::from("/home/user/.ocx");
        let fs = FileStructure::with_root(root);

        assert_eq!(
            fs.temp.root(),
            fs.layer_temp.root(),
            "temp and layer_temp must point at the same directory in the unified-zone (single-root) layout"
        );
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
