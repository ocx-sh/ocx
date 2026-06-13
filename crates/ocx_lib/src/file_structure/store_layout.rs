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
pub struct StoreLayout {
    /// `$OCX_HOME` — the overall store root.
    ///
    /// Acts as the fallback default for both the cache and state zones (and,
    /// transitively, packages via cache) when their dedicated overrides are
    /// absent. It is also the path returned by `FileStructure::root()`.
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
    pub fn from_root(root: PathBuf) -> Self {
        Self::resolve(root, None, None, None, None)
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
        home: PathBuf,
        cache: Option<PathBuf>,
        packages: Option<PathBuf>,
        state: Option<PathBuf>,
        index: Option<PathBuf>,
    ) -> Self {
        // cache defaults to home; packages defaults to the *resolved* cache;
        // state defaults to home. Defaulting happens here once so callers never
        // re-derive it. Empty-string overrides must be converted to `None` by
        // the caller (mirrors `default_ocx_root`'s `!is_empty` guard) — the
        // `resolve_from_env` helper enforces that contract.
        let cache = cache.unwrap_or_else(|| home.clone());
        let packages = packages.unwrap_or_else(|| cache.clone());
        let state = state.unwrap_or_else(|| home.clone());
        Self {
            home,
            cache,
            packages,
            state,
            index,
        }
    }

    /// Root for the content zone shared between blobs and layers.
    ///
    /// Corresponds to `OCX_CACHE_DIR`, defaulting to `$OCX_HOME`.
    pub fn cache(&self) -> &std::path::Path {
        &self.cache
    }

    /// `$OCX_HOME` — the overall store root.
    ///
    /// Used as the default for cache and state zones and as the root
    /// returned by `FileStructure::root()`.
    pub fn home(&self) -> &std::path::Path {
        &self.home
    }

    /// Root for the packages zone.
    ///
    /// Corresponds to `OCX_PACKAGES_DIR`, defaulting to the resolved cache root.
    pub fn packages_root(&self) -> &std::path::Path {
        &self.packages
    }

    /// Root for the per-instance state zone (symlinks, state, projects).
    ///
    /// Corresponds to `OCX_STATE_DIR`, defaulting to `$OCX_HOME`.
    pub fn state(&self) -> &std::path::Path {
        &self.state
    }

    /// Resolved path for the local tag index root, or `None` for the default.
    ///
    /// `None` means "use `{cache}/tags`". When `Some`, the value is an
    /// explicit override from `OCX_INDEX` or `--index`.
    pub fn tags_root(&self) -> Option<&std::path::Path> {
        self.index.as_deref()
    }

    /// Resolves a `StoreLayout` from `home` plus the zone-override environment
    /// variables (`OCX_CACHE_DIR`, `OCX_PACKAGES_DIR`, `OCX_STATE_DIR`,
    /// `OCX_INDEX`).
    ///
    /// Each variable is read via [`crate::env::var`] and an empty value is
    /// treated as unset (mirrors [`super::default_ocx_root`]'s `!is_empty`
    /// guard). This is the single lib-side env-reading seam for zone
    /// resolution; the CLI builds its own `StoreLayout` from parsed options +
    /// env so `--index` can win over `OCX_INDEX`.
    pub fn resolve_from_env(home: PathBuf) -> Self {
        use crate::env::keys;
        Self::resolve(
            home,
            non_empty_path(keys::OCX_CACHE_DIR),
            non_empty_path(keys::OCX_PACKAGES_DIR),
            non_empty_path(keys::OCX_STATE_DIR),
            non_empty_path(keys::OCX_INDEX),
        )
    }
}

/// Reads a zone-override environment variable as an absolute `PathBuf`,
/// returning `None` when unset, empty, or not absolute.
///
/// Empty-string env values are treated as unset per the §5 M2 resolution
/// contract. A relative value is rejected (CWD-relative zone roots are a
/// path-traversal / surprise-resolution hazard, CWE-22) with a `WARN` and
/// treated as unset, so the zone safely falls back to its default rather than
/// resolving against whatever CWD the process happens to have.
///
/// NOTE: `OCX_HOME` / [`super::default_ocx_root`] are intentionally left
/// unchanged here — they pre-date this guard and broadening it to them is a
/// separate follow-up (avoid scope creep).
fn non_empty_path(key: &str) -> Option<PathBuf> {
    let value = crate::env::var(key).filter(|value| !value.is_empty())?;
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        crate::log::warn!("{key} must be an absolute path; ignoring relative value");
        return None;
    }
    Some(path)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{StoreLayout, non_empty_path};

    // ── relative_zone_value_is_ignored (review-fix #4, CWE-22) ────────────────
    //
    // Requirement: a relative zone-override value must be rejected (it would
    // otherwise resolve against the process CWD — a path-traversal / surprise
    // hazard). `non_empty_path` must return `None` for a relative path and the
    // zone falls back to its default; an absolute value passes through.
    //
    // Traced to: P1 shared-store review-fix loop, finding #4 (WARN security).
    #[test]
    fn relative_zone_value_is_ignored() {
        use crate::env::keys;
        let env_guard = crate::test::env::lock();

        // A relative path → ignored (treated as unset).
        env_guard.set(keys::OCX_CACHE_DIR, "relative/cache");
        assert_eq!(
            non_empty_path(keys::OCX_CACHE_DIR),
            None,
            "a relative zone value must be ignored (CWE-22 guard)"
        );

        // An empty value → ignored (existing contract, unchanged).
        env_guard.set(keys::OCX_CACHE_DIR, "");
        assert_eq!(
            non_empty_path(keys::OCX_CACHE_DIR),
            None,
            "an empty zone value must be ignored"
        );

        // An absolute value → passes through unchanged.
        #[cfg(unix)]
        let absolute = "/mnt/shared/cache";
        #[cfg(windows)]
        let absolute = r"C:\mnt\shared\cache";
        env_guard.set(keys::OCX_CACHE_DIR, absolute);
        assert_eq!(
            non_empty_path(keys::OCX_CACHE_DIR),
            Some(PathBuf::from(absolute)),
            "an absolute zone value must pass the guard"
        );
    }

    // ── from_root_collapses_all_zones ─────────────────────────────────────────
    //
    // Requirement: system_design_shared_store.md §5 M2 — "When all three zone
    // overrides are absent (the default), this produces the same layout as
    // with_root($OCX_HOME)."  `from_root` must produce a layout where every
    // zone (cache, packages, state) equals `root`, and tags_root is None
    // (meaning "default under cache").
    #[test]
    fn from_root_collapses_all_zones() {
        let root = PathBuf::from("/home/user/.ocx");
        let layout = StoreLayout::from_root(root.clone());

        assert_eq!(layout.home(), root.as_path(), "home() must equal root");
        assert_eq!(layout.cache(), root.as_path(), "cache zone must collapse to root");
        assert_eq!(
            layout.packages_root(),
            root.as_path(),
            "packages zone must collapse to root when no override"
        );
        assert_eq!(
            layout.state(),
            root.as_path(),
            "state zone must collapse to root when no override"
        );
        assert!(
            layout.tags_root().is_none(),
            "tags_root must be None when no OCX_INDEX override (defaults under cache)"
        );
    }

    // ── cache_override_moves_blobs_layers_not_state ───────────────────────────
    //
    // Requirement: §5 M2 — "blobs, layers ← cache.join(..)"; state zone
    // defaults to home, NOT to cache. When OCX_CACHE_DIR is set, state must
    // still be home.
    #[test]
    fn cache_override_moves_blobs_layers_not_state() {
        let home = PathBuf::from("/home/user/.ocx");
        let cache = PathBuf::from("/mnt/shared/cache");
        let layout = StoreLayout::resolve(home.clone(), Some(cache.clone()), None, None, None);

        assert_eq!(layout.home(), home.as_path(), "home() must stay as the home root");
        assert_eq!(
            layout.cache(),
            cache.as_path(),
            "cache zone must equal the OCX_CACHE_DIR override"
        );
        assert_eq!(
            layout.state(),
            home.as_path(),
            "state zone must default to home, not follow cache"
        );
    }

    // ── packages_defaults_to_cache ────────────────────────────────────────────
    //
    // Requirement: §5 M2 — "packages defaults to resolved cache".
    // When OCX_PACKAGES_DIR is unset but OCX_CACHE_DIR is set, packages
    // must inherit the resolved cache root.
    #[test]
    fn packages_defaults_to_cache() {
        let home = PathBuf::from("/home/user/.ocx");
        let cache = PathBuf::from("/mnt/shared/cache");
        let layout = StoreLayout::resolve(home, Some(cache.clone()), None, None, None);

        assert_eq!(
            layout.packages_root(),
            cache.as_path(),
            "packages_root must default to the resolved cache zone when OCX_PACKAGES_DIR is unset"
        );
    }

    // ── packages_override_independent_of_cache ────────────────────────────────
    //
    // Requirement: §5 M2 — "packages ← packages_root.join('packages')".
    // When OCX_PACKAGES_DIR is explicitly set, it must be independent of
    // the cache override.
    #[test]
    fn packages_override_independent_of_cache() {
        let home = PathBuf::from("/home/user/.ocx");
        let cache = PathBuf::from("/mnt/shared/cache");
        let packages = PathBuf::from("/ephemeral/packages");
        let layout = StoreLayout::resolve(home, Some(cache.clone()), Some(packages.clone()), None, None);

        assert_eq!(
            layout.packages_root(),
            packages.as_path(),
            "packages_root must equal the explicit OCX_PACKAGES_DIR override"
        );
        assert_eq!(
            layout.cache(),
            cache.as_path(),
            "cache must remain the OCX_CACHE_DIR override independent of packages"
        );
    }

    // ── state_override_moves_symlinks_state ───────────────────────────────────
    //
    // Requirement: §5 M2 — "symlinks, state, projects ← state.join(..)".
    // OCX_STATE_DIR sets the state zone; cache must be unaffected.
    #[test]
    fn state_override_moves_symlinks_state() {
        let home = PathBuf::from("/home/user/.ocx");
        let state = PathBuf::from("/home/user/.ocx-local");
        let layout = StoreLayout::resolve(home.clone(), None, None, Some(state.clone()), None);

        assert_eq!(
            layout.state(),
            state.as_path(),
            "state zone must equal the OCX_STATE_DIR override"
        );
        assert_eq!(
            layout.cache(),
            home.as_path(),
            "cache zone must default to home when OCX_CACHE_DIR is unset"
        );
    }

    // ── index_defaults_under_cache ────────────────────────────────────────────
    //
    // Requirement: §5 M2 — "tags ← OCX_INDEX | cache.join('tags')".
    // When OCX_INDEX is unset, tags_root() returns None (the resolver builds
    // the path as cache/tags, but the None sentinel signals "default").
    #[test]
    fn index_defaults_under_cache() {
        let home = PathBuf::from("/home/user/.ocx");
        let layout = StoreLayout::resolve(home, None, None, None, None);

        assert!(
            layout.tags_root().is_none(),
            "tags_root must be None when OCX_INDEX is unset (caller uses cache/tags)"
        );
    }

    // ── index_explicit_beats_cache_default ────────────────────────────────────
    //
    // Requirement: §5 M2 — precedence: flag/env ▸ cache/tags.
    // When OCX_INDEX is explicitly set, tags_root() returns Some(path).
    #[test]
    fn index_explicit_beats_cache_default() {
        let home = PathBuf::from("/home/user/.ocx");
        let cache = PathBuf::from("/mnt/shared/cache");
        let index = PathBuf::from("/custom/tags");
        let layout = StoreLayout::resolve(home, Some(cache), None, None, Some(index.clone()));

        assert_eq!(
            layout.tags_root(),
            Some(index.as_path()),
            "tags_root must return Some(OCX_INDEX) when explicitly set"
        );
    }

    // ── empty_string_zone_treated_as_unset ────────────────────────────────────
    //
    // Requirement: §5 M2 — "Empty string = unset."
    // Mirrors the `!is_empty()` guard in `default_ocx_root` and the env-key
    // contract: callers pass `None` for "absent/empty" — a Some("") path
    // must never produce a different layout than `None`.
    //
    // NOTE: The contract is that callers convert empty-string env values to
    // `None` before calling `resolve`. This test verifies that `from_root`
    // and the None-path of resolve produce identical results, which is the
    // observable invariant for the "empty string treated as unset" rule.
    #[test]
    fn empty_string_zone_treated_as_unset() {
        let root = PathBuf::from("/home/user/.ocx");
        let from_root = StoreLayout::from_root(root.clone());
        let resolved_none = StoreLayout::resolve(root.clone(), None, None, None, None);

        assert_eq!(
            from_root.cache(),
            resolved_none.cache(),
            "from_root and resolve(None,None,None,None) must produce the same cache"
        );
        assert_eq!(
            from_root.packages_root(),
            resolved_none.packages_root(),
            "from_root and resolve(None,...) must produce the same packages_root"
        );
        assert_eq!(
            from_root.state(),
            resolved_none.state(),
            "from_root and resolve(...,None,...) must produce the same state"
        );
        assert_eq!(
            from_root.tags_root(),
            resolved_none.tags_root(),
            "from_root and resolve(...,None) must produce the same tags_root"
        );
    }
}
