// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

mod blob_store;
mod cas_path;
pub mod error;
mod index_store;
mod layer_store;
mod package_store;
mod state_store;
mod symlink_store;
mod temp_store;

pub use blob_store::{BlobDir, BlobStore};
#[cfg(test)]
pub(crate) use blob_store::{WRITE_BLOB_CALL_COUNT, WRITE_BLOB_TEST_LOCK};
pub use cas_path::{CasTier, DIGEST_FILENAME, cas_ref_name, cas_shard_path, read_digest_file, write_digest_file};
pub use index_store::{CatalogEntryStatus, CatalogTransaction, IndexStore, RootReadResult, SOURCE_LOCK_TIMEOUT};
pub use layer_store::{LayerDir, LayerStore};
pub use package_store::{PackageDir, PackageStore};
pub use state_store::StateStore;
pub use symlink_store::{SymlinkKind, SymlinkStore};
pub use temp_store::{StaleEntry, TempAcquireResult, TempDir, TempEntry, TempStore};

/// Root layout of the local OCX data directory.
///
/// `FileStructure` is a thin composite that provides typed, well-named access
/// to seven top-level stores:
///
/// - **`blobs`**    — content-addressed raw blob store
/// - **`layers`**   — content-addressed extracted layer store
/// - **`packages`** — content-addressed package store (content, metadata, refs)
/// - **`index`**    — self-contained index collection at the default machine-local
///   home (`index/`): a first-class store sibling to `blobs/`/`layers/`/`packages/`,
///   one index per source — `<source>/{config.json,c/,p/}` holding the hosted
///   wire grammar (root documents + dispatch-object CAS, A2) plus a flat
///   opaque-blob CAS for content that is not a package manifest (config
///   blobs, managed-config payloads). Redirected wholesale by `--index` /
///   `OCX_INDEX` at the CLI seam (`adr_index_indirection.md` A1)
/// - **`symlinks`** — install symlinks (candidate / current)
/// - **`state`**    — persistent runtime state (update-check timestamps, etc.)
/// - **`temp`**     — temporary staging directories for in-progress downloads
///
/// plus one non-store path:
///
/// - **`locks`**    — machine-global cross-process lock directory
///   (`$OCX_HOME/locks`); sharded, content-keyed advisory lock files, outside
///   the GC graph, kept out of the (possibly redirected/read-only) index home
///
/// Default root: `~/.ocx` (resolved via [`default_ocx_root`]).
#[derive(Debug, Clone)]
pub struct FileStructure {
    root: std::path::PathBuf,
    pub blobs: BlobStore,
    pub layers: LayerStore,
    pub packages: PackageStore,
    pub index: IndexStore,
    pub symlinks: SymlinkStore,
    pub state: StateStore,
    pub temp: TempStore,
    /// Machine-global cross-process lock directory (`$OCX_HOME/locks`). Not a
    /// CAS store and never in the GC graph — sharded, content-keyed advisory
    /// lock files written by [`crate::utility::fs::lock_scoped`]. Kept out of
    /// the index home so a redirected (`--index`) or read-only shipped index
    /// copy never accumulates lock litter.
    pub locks: std::path::PathBuf,
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
        let locks = root.join("locks");
        Self {
            blobs: BlobStore::new(root.join("blobs")),
            layers: LayerStore::new(root.join("layers")),
            packages: PackageStore::new(root.join("packages")),
            // Default machine-local index home — a first-class store
            // sibling to `blobs/`/`layers/`/`packages/`, not runtime state
            // buried under `state/` (`adr_index_indirection.md` A1). `--index`
            // / `OCX_INDEX` redirect the whole collection at the CLI seam, but
            // its locks always stay machine-global under `$OCX_HOME/locks`.
            index: IndexStore::new(root.join("index")).with_locks_root(locks.clone()),
            symlinks: SymlinkStore::new(root.join("symlinks")),
            state: StateStore::new(root.join("state")),
            temp: TempStore::new(root.join("temp")),
            locks,
            root,
        }
    }

    /// Returns the root directory of this file structure (e.g., `~/.ocx`).
    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    /// Machine-local path holding the patch-descriptor discovery state
    /// (the `__ocx.patch` three-state record — a `BTreeMap<String, String>`
    /// tag→digest map) for `identifier`.
    ///
    /// Layout: `{root}/state/patch-descriptors/{registry_slug}/{repo}.json`.
    /// This is a per-machine cache of "did we look for a patch descriptor at
    /// this (registry, repo) pair", NOT the committed reproducibility index
    /// snapshot — so it lives under `state/`, never in the redirectable index
    /// home, and never carries `--index` / `OCX_INDEX` redirection.
    pub fn patch_descriptor_path(&self, identifier: &crate::oci::Identifier) -> PathBuf {
        self.root
            .join("state")
            .join("patch-descriptors")
            .join(slugify(identifier.registry()))
            .join(repository_path(identifier.repository()))
            .with_added_extension("json")
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
