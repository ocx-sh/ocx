// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Zone-based store layout resolver for `FileStructure`.
//!
//! `StoreLayout` captures the resolved root directories for each zone once
//! (flags ▸ env ▸ default) and passes them to
//! [`FileStructure::with_layout`](super::FileStructure::with_layout).
//!
//! Zone mapping (§5 M2 of the shared-store system design):
//!
//! ```text
//! OCX_CACHE_DIR   → blobs/, layers/  (+ layer staging temp)
//! OCX_PACKAGES_DIR→ packages/        (+ package staging temp)
//! (default: cache zone)
//! OCX_STATE_DIR   → symlinks/, state/, projects/
//! OCX_INDEX       → tags/  (defaults under cache zone)
//! ```
//!
//! When all three zone overrides are absent, every store collapses to
//! `$OCX_HOME`, reproducing today's single-root layout exactly.

use std::path::PathBuf;

/// Resolved zone roots for the OCX local store.
///
/// Constructed via [`StoreLayout::from_root`] (single-root, today's default)
/// or [`StoreLayout::resolve`] (zone overrides from env / caller). Pass to
/// [`FileStructure::with_layout`](super::FileStructure::with_layout) to
/// construct a zone-aware `FileStructure`.
///
/// All paths are resolved eagerly at construction; no env reads happen after
/// the constructor returns.
#[derive(Debug, Clone)]
// Fields are read only in stub accessor methods marked `unimplemented!()`;
// the dead-code lint fires during the stub phase before the impls land.
#[allow(dead_code)]
pub struct StoreLayout {
    /// Root for the cache zone: blobs, layers, and layer-staging temp.
    ///
    /// Defaults to `$OCX_HOME`.
    home: PathBuf,
    /// Root for the cache zone (blobs, layers, layer-staging temp).
    ///
    /// Always equal to `home` when constructed via `from_root`. May differ
    /// when `OCX_CACHE_DIR` is set.
    cache: PathBuf,
    /// Root for the packages zone (assembled packages and package-staging temp).
    ///
    /// Defaults to the resolved `cache` root when `OCX_PACKAGES_DIR` is unset.
    packages: PathBuf,
    /// Root for the state zone (symlinks, state, projects).
    ///
    /// Defaults to `home` when `OCX_STATE_DIR` is unset.
    state: PathBuf,
    /// Explicit override for the local index (tags).
    ///
    /// `None` means "default under cache": `{cache}/tags`. When `Some`, the
    /// caller has explicitly overridden via `OCX_INDEX` or `--index`.
    index: Option<PathBuf>,
}

impl StoreLayout {
    /// Builds a `StoreLayout` where every zone is rooted at `root`.
    ///
    /// Produces the exact same paths as the pre-M2 `FileStructure::with_root`:
    /// all seven stores under `root`. Use this to preserve backward
    /// compatibility for callers that already have a resolved root path.
    pub fn from_root(_root: PathBuf) -> Self {
        unimplemented!()
    }

    /// Resolves a `StoreLayout` from explicit zone overrides.
    ///
    /// Each `Option<PathBuf>` parameter maps to an override env var. When
    /// `None`, the zone falls back to its documented default:
    ///
    /// - `cache`    defaults to `home`
    /// - `packages` defaults to the resolved `cache`
    /// - `state`    defaults to `home`
    /// - `index`    stays `None` (caller reads from `OCX_INDEX` separately)
    ///
    /// Resolution is idempotent: calling with the same values returns the
    /// same layout.
    pub fn resolve(
        _home: PathBuf,
        _cache: Option<PathBuf>,
        _packages: Option<PathBuf>,
        _state: Option<PathBuf>,
        _index: Option<PathBuf>,
    ) -> Self {
        unimplemented!()
    }

    /// Root for the content zone shared between blobs and layers.
    ///
    /// Corresponds to `OCX_CACHE_DIR`, defaulting to `$OCX_HOME`.
    pub fn cache(&self) -> &std::path::Path {
        unimplemented!()
    }

    /// `$OCX_HOME` — the overall store root.
    ///
    /// Used as the default for cache and state zones and as the root
    /// returned by `FileStructure::root()`.
    pub fn home(&self) -> &std::path::Path {
        unimplemented!()
    }

    /// Root for the packages zone.
    ///
    /// Corresponds to `OCX_PACKAGES_DIR`, defaulting to the resolved cache root.
    pub fn packages_root(&self) -> &std::path::Path {
        unimplemented!()
    }

    /// Root for the per-instance state zone (symlinks, state, projects).
    ///
    /// Corresponds to `OCX_STATE_DIR`, defaulting to `$OCX_HOME`.
    pub fn state(&self) -> &std::path::Path {
        unimplemented!()
    }

    /// Resolved path for the local tag index root, or `None` for the default.
    ///
    /// `None` means "use `{cache}/tags`". When `Some`, the value is an
    /// explicit override from `OCX_INDEX` or `--index`.
    pub fn tags_root(&self) -> Option<&std::path::Path> {
        unimplemented!()
    }
}
