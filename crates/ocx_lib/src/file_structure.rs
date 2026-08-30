// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

mod blob_store;
mod cas_path;
pub mod error;
mod index_store;
mod layer_store;
mod package_store;
mod shim_bin_store;
mod shim_store;
mod state_store;
mod symlink_store;
mod temp_store;

pub use blob_store::{BlobDir, BlobStore};
pub use cas_path::{CasTier, DIGEST_FILENAME, cas_ref_name, cas_shard_path, read_digest_file, write_digest_file};
pub use index_store::{CatalogEntryStatus, CatalogTransaction, IndexStore, RootReadResult, SOURCE_LOCK_TIMEOUT};
pub use layer_store::{LayerDir, LayerStore};
pub use package_store::{PackageDir, PackageStore, record_origin};
pub use shim_bin_store::ShimBinStore;
pub use shim_store::{ShimDir, ShimStore};
pub use state_store::StateStore;
pub use symlink_store::{SymlinkKind, SymlinkStore};
pub use temp_store::{StaleEntry, TempAcquireResult, TempDir, TempEntry, TempStore};

/// Root layout of the local OCX data directory.
///
/// `FileStructure` is a thin composite that provides typed, well-named access
/// to nine top-level stores:
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
/// - **`shim_bin`** — content-addressed store for the embedded `ocx-shim`
///   executable blob (`.bin/ocx-shim/`), hardlinked by every generated
///   Windows launcher; outside the GC graph, never walked by `ocx clean`
///   (see the `shim_bin_store` module docs)
/// - **`shims`** — identity-keyed store for generated shim directories
///   (`shims/`), the on-disk form of a deferred tool: launchers under `bin/`,
///   with `digest` and `refs/` as siblings. Unlike the three CAS tiers the
///   repository IS part of its path, and its GC liveness is rooted directly in
///   the lock pins (see the `shim_store` module docs)
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
    /// Content-addressed store for the embedded `ocx-shim` executable blob
    /// (`$OCX_HOME/.bin/ocx-shim/`), hardlinked by every generated Windows
    /// entrypoint launcher. Outside the three GC tiers — never walked by
    /// `ocx clean` (plan decision D4). See the `shim_bin_store` module docs.
    pub shim_bin: ShimBinStore,
    /// Identity-keyed store for generated shim directories
    /// (`$OCX_HOME/shims/`) — the on-disk form of a deferred tool. Keyed by
    /// registry + repository + digest (the repository IS in the path, unlike
    /// [`PackageStore`]), with launchers under `bin/`. Walked by `ocx clean`,
    /// but rooted directly in the lock pins rather than reachable from a
    /// package. See the `shim_store` module docs.
    pub shims: ShimStore,
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
            // `.bin/ocx-shim/` — a sibling namespace to the three CAS tiers
            // above, not nested under any of them; see the `shim_bin_store`
            // module docs for why it stays outside the GC graph.
            shim_bin: ShimBinStore::new(root.join(".bin").join("ocx-shim")),
            // `shims/` — a sibling namespace to the three CAS tiers, holding
            // the deferred form of a tool. Present exactly when the matching
            // `packages/` entry is absent.
            shims: ShimStore::new(root.join("shims")),
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

    /// Machine-local path holding the patch tier's own companion pins for
    /// `identifier`'s repository — a `BTreeMap<String, String>` tag→digest map
    /// recording the top (image-index) digest each companion tag was last
    /// resolved to.
    ///
    /// Layout: `{root}/state/patch-companions/{registry_slug}/{repo}.json`.
    /// A companion is a package the user never named, so its tag→digest
    /// binding is patch-tier state and must never become a package-tier pin in
    /// the local index (`subsystem-oci`: a pin moves only when named). Like
    /// [`patch_descriptor_path`](Self::patch_descriptor_path) it lives under
    /// `state/` and never carries `--index` / `OCX_INDEX` redirection.
    pub fn patch_companion_path(&self, identifier: &crate::oci::Identifier) -> PathBuf {
        self.root
            .join("state")
            .join("patch-companions")
            .join(slugify(identifier.registry()))
            .join(repository_path(identifier.repository()))
            .with_added_extension("json")
    }
}

/// The current user's home directory.
///
/// One resolver, because two of them disagreed. `std::env::home_dir` (stable
/// since 1.85), never `dirs::home_dir`: on Windows the former reads
/// `%USERPROFILE%` first and only asks the OS for the registered profile path
/// when that is unset, while `dirs` calls `SHGetKnownFolderPath`
/// unconditionally — so a sandbox, container or CI runner that overrides
/// `%USERPROFILE%` handed one `ocx` invocation two different home directories.
/// They also disagree on Unix over an empty `pw_dir` (#381).
///
/// `setup::home_env_from_environment` deliberately stays on its own resolver.
/// It answers a different question — the *login shell's* home, for writing
/// `.bashrc`/`.zshrc` — and reads `$HOME` first for exactly that reason.
pub fn home_directory() -> Option<PathBuf> {
    std::env::home_dir()
}

/// Returns the OCX data root directory.
///
/// Resolution order:
/// 1. `OCX_HOME` environment variable (if set and non-empty)
/// 2. `~/.ocx` (fallback, via [`home_directory`])
///
/// **The one definition of that default.** `$OCX_HOME` has to name a single
/// directory for a whole invocation, so every caller resolves it here —
/// including the config loader's `home_*_path()` accessors, which used to
/// re-derive it and could land somewhere else.
///
/// Read through [`crate::env::var`], the project-wide shim, so a test injects
/// `OCX_HOME` the same way it does for every other variable.
pub fn default_ocx_root() -> Option<PathBuf> {
    if let Some(home) = crate::env::var("OCX_HOME").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(home));
    }
    home_directory().map(|home| home.join(".ocx"))
}

use std::path::PathBuf;

use crate::prelude::StringExt;

/// Convert an OCI identifier component (registry, repository, tag) into a
/// filesystem-safe path segment using [`StringExt::to_relaxed_slug`].
///
/// `pub` because it is not merely an internal detail: it is the key
/// [`IndexStore`] addresses a source's subtree by, so two configured namespaces
/// differing only in a non-`[a-zA-Z0-9._-]` character share one directory. A
/// caller validating a source name against configuration has to compare on this
/// form, or its verdict applies to a different subtree than the one written —
/// see `ocx index regenerate`'s published-only guard.
pub fn slugify(value: &str) -> String {
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

    /// C-002 (#381): the `$OCX_HOME` default has one definition, and the
    /// config loader's accessors resolve through it.
    ///
    /// The two used to be separate resolvers reading the fallback home through
    /// different APIs (`std::env::home_dir` here, `dirs::home_dir` there),
    /// which can name different directories — so this asserts the *paths*
    /// agree, not that both functions merely return `Some`.
    #[test]
    fn the_ocx_home_default_has_one_definition() {
        let env = crate::test::env::lock();
        let home = env.isolate_project_home();

        assert_eq!(
            default_ocx_root().as_deref(),
            Some(home.path()),
            "OCX_HOME must be read through the shared shim, not std::env::var"
        );
        assert_eq!(
            crate::config::loader::ConfigLoader::home_path(),
            Some(home.path().join("config.toml")),
            "the loader's home tier must sit under the same root"
        );
        assert_eq!(
            crate::config::loader::ConfigLoader::home_sigstore_trusted_root_path(),
            Some(home.path().join("sigstore").join("trusted-root.json")),
            "the trust-root convention path must sit under the same root"
        );

        // An empty value is not a root. Falling back keeps `$OCX_HOME=""` from
        // resolving every store to the process working directory.
        env.set("OCX_HOME", "");
        let fallback = home_directory().map(|h| h.join(".ocx"));
        assert_eq!(default_ocx_root(), fallback, "an empty OCX_HOME must fall back");
        assert_eq!(
            crate::config::loader::ConfigLoader::home_path(),
            fallback.map(|d| d.join("config.toml")),
            "the two must still agree on the fallback"
        );
    }

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
