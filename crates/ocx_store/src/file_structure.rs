// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

mod assemble;
mod blob_store;
mod cas_path;
pub mod error;
mod layer_store;
mod package_store;
mod shim_bin_store;
mod shim_store;
mod state_store;
mod symlink_store;
mod temp_store;
mod toolchain_store;

pub use assemble::{
    AssembleError, AssemblyError, AssemblyStats, assemble_from_layer, assemble_from_layers,
    assemble_from_layers_with_layouts,
};
pub use blob_store::{BlobDir, BlobStore};
pub use cas_path::{
    CasTier, DIGEST_FILENAME, DigestFileError, cas_ref_name, cas_shard_path, read_digest_file, write_digest_file,
};
pub use layer_store::{LayerDir, LayerStore};
pub use package_store::{PackageDir, PackageDirError, PackageStore, record_origin};
pub use shim_bin_store::ShimBinStore;
pub use shim_store::{ShimDir, ShimStore};
pub use state_store::{BinEntryStamp, RenderStamp, RenderStampScope, RenderStampTarget, StateStore};
pub use symlink_store::{SymlinkKind, SymlinkStore};
pub use temp_store::{StaleEntry, TempAcquireResult, TempDir, TempEntry, TempStore};
pub use toolchain_store::{
    DEFAULT_SHELL, TREE_OWN_DEPTH1_NAMES, ToolchainHome, ToolchainPathComponent, ToolchainPathError, ToolchainStore,
};

/// Root layout of the local OCX data directory.
///
/// `FileStructure` is a thin composite that provides typed, well-named access
/// to ten top-level stores:
///
/// - **`blobs`**    — content-addressed raw blob store
/// - **`layers`**   — content-addressed extracted layer store
/// - **`packages`** — content-addressed package store (content, metadata, refs)
/// - **`index`**    — self-contained index collection at the default machine-local
///   home (`index/`), reached through
///   `IndexStore::machine_local`
///   rather than owned as a field (the index sits above the store in the crate
///   map): a first-class store sibling to `blobs/`/`layers/`/`packages/`,
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
/// - **`toolchain`** — the global rendered toolchain home (`toolchain/`):
///   launcher trampolines under `shells/default/bin` and one
///   `links/<group>/<entry>` directory link per locked tool. Outside the GC graph, never walked by `ocx clean`,
///   and a wrapper over one `ToolchainHome` so the global tier and a project
///   tier share one grammar (see the `toolchain_store` module docs)
///
/// plus one non-store path:
///
/// - **`locks`**    — machine-global cross-process lock directory
///   (`$OCX_HOME/locks`); sharded, content-keyed advisory lock files, outside
///   the GC graph, kept out of the (possibly redirected/read-only) index home
///
/// Default root: `~/.ocx` (resolved via [`ocx_config::home::default_ocx_root`]).
#[derive(Debug, Clone)]
pub struct FileStructure {
    root: std::path::PathBuf,
    pub blobs: BlobStore,
    pub layers: LayerStore,
    pub packages: PackageStore,
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
    /// The **global** rendered toolchain home (`$OCX_HOME/toolchain/`) —
    /// launcher trampolines under `shells/default/bin`,
    /// `links/<group>/<entry>` directory links to package roots, the `active`
    /// link that puts them on `PATH`, and the `.gitignore` that hides the tree
    /// (C-001).
    /// Outside the three GC tiers, never walked by `ocx clean`, exactly as
    /// [`ShimBinStore`] is. A project's home is the same grammar at a
    /// different root and is deliberately NOT a field here: it depends on
    /// which project is in scope, so it is resolved per call by
    /// `resolve_toolchain_home`
    /// (C-002). See the `toolchain_store` module docs.
    pub toolchain: ToolchainStore,
    /// Machine-global cross-process lock directory (`$OCX_HOME/locks`). Not a
    /// CAS store and never in the GC graph — sharded, content-keyed advisory
    /// lock files written by [`ocx_util::fs::lock_scoped`]. Kept out of
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
        let root = ocx_config::home::default_ocx_root().expect("Could not determine default OCX root directory.");
        Self::with_root(root)
    }

    /// Creates a `FileStructure` rooted at `root`.
    pub fn with_root(root: std::path::PathBuf) -> Self {
        let locks = root.join("locks");
        Self {
            blobs: BlobStore::new(root.join("blobs")),
            layers: LayerStore::new(root.join("layers")),
            packages: PackageStore::new(root.join("packages")),
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
            // `toolchain/` — the global rendered home. A sibling namespace to
            // the three CAS tiers, built once here so no caller re-derives the
            // tree location by a literal join (C-001).
            toolchain: ToolchainStore::new(root.join("toolchain")),
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
    pub fn patch_descriptor_path(&self, identifier: &ocx_oci::PackageRef) -> PathBuf {
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
    pub fn patch_companion_path(&self, identifier: &ocx_oci::PackageRef) -> PathBuf {
        self.root
            .join("state")
            .join("patch-companions")
            .join(slugify(identifier.registry()))
            .join(repository_path(identifier.repository()))
            .with_added_extension("json")
    }

    /// `$OCX_HOME/symlinks/<ocx cli id>/current/content/bin` — the directory the
    /// installed `ocx` itself resolves from.
    ///
    /// Derived from the symlink store and [`ocx_oci::ocx_cli_identifier`], never
    /// joined from a literal: the identifier honours the `__OCX_SELF_IMAGE` seam,
    /// so a hand-spelled `ocx.sh/ocx/cli` would be right on a developer machine and
    /// wrong under every test that moves it.
    ///
    /// It lives here rather than beside `ocx self setup`, which writes it: this
    /// is a derivation over [`Self::symlinks`]' own layout, and the three tiers
    /// that read it — the session PATH fingerprint, the launcher generator and
    /// the per-prompt activation sequencer — all sit *below* the installer, so
    /// asking it for the path was the one edge that pointed upward at `setup`.
    pub fn ocx_install_bin_path(&self) -> PathBuf {
        self.symlinks
            .current(&ocx_oci::ocx_cli_identifier())
            .join("content")
            .join("bin")
    }
}

use std::path::PathBuf;

use ocx_util::prelude::StringExt;

/// Convert an OCI identifier component (registry, repository, tag) into a
/// filesystem-safe path segment using [`StringExt::to_relaxed_slug`].
///
/// `pub` because it is not merely an internal detail: it is the key
/// `IndexStore` addresses a source's subtree
/// by, so two configured namespaces
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
pub fn repository_path(repository: &str) -> PathBuf {
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

    // ── C-001: the global toolchain store ────────────────────────────────────

    /// C-001 — the global store is built once from the composite root, so no
    /// caller ever re-derives the tree location by a literal join.
    #[test]
    fn the_global_toolchain_store_is_rooted_at_toolchain_under_the_composite_root() {
        let root = std::path::PathBuf::from("/ocx");
        let structure = FileStructure::with_root(root.clone());
        assert_eq!(
            structure.toolchain.root(),
            root.join("toolchain"),
            "the store must be built from the composite root, never re-derived"
        );
    }

    /// C-001 — the store `FileStructure` owns and the home it wraps are one
    /// grammar: every accessor reachable through the field answers exactly what
    /// the wrapped home answers.
    #[test]
    fn the_composite_roots_toolchain_field_answers_through_its_one_home() {
        let structure = FileStructure::with_root(std::path::PathBuf::from("/ocx"));
        assert_eq!(structure.toolchain.bin(), structure.toolchain.home().bin());
        assert_eq!(
            structure.toolchain.shell_bin(DEFAULT_SHELL),
            structure.toolchain.home().shell_bin(DEFAULT_SHELL)
        );
        assert_eq!(structure.toolchain.gitignore(), structure.toolchain.home().gitignore());
        assert_eq!(
            structure.toolchain.bin(),
            structure.root().join("toolchain").join("active").join("bin"),
            "C-078 — the PATH-facing trampoline directory is `<root>/toolchain/active/bin`"
        );
        assert_eq!(
            structure.toolchain.shell_bin(DEFAULT_SHELL),
            structure
                .root()
                .join("toolchain")
                .join("shells")
                .join("default")
                .join("bin"),
            "C-078 — the renderer writes the physical `<root>/toolchain/shells/default/bin`"
        );
    }

    /// C-001, validation item 25 — the rendered tree is a **sibling** of the
    /// GC-walked stores: never inside one, and never containing one. That
    /// disjointness is what keeps `ocx clean`'s walk from ever reaching it, so
    /// a stale tree is litter re-rendered by the next `ocx pull` rather than
    /// collected content.
    #[test]
    fn the_toolchain_tree_sits_outside_every_gc_walked_store() {
        let structure = FileStructure::with_root(std::path::PathBuf::from("/ocx"));
        let toolchain = structure.toolchain.root().to_path_buf();

        for walked in [
            structure.packages.root(),
            structure.layers.root(),
            structure.blobs.root(),
            structure.shims.root(),
        ] {
            assert!(
                !toolchain.starts_with(walked),
                "the toolchain tree must not live inside the GC-walked {walked:?}"
            );
            assert!(
                !walked.starts_with(&toolchain),
                "a GC-walked store must not live inside the toolchain tree ({walked:?})"
            );
        }
    }
}
