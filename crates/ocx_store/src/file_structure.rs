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

/// Root layout of the local OCX data directory; the index lives in `IndexStore`, above this crate.
#[derive(Debug, Clone)]
pub struct FileStructure {
    root: std::path::PathBuf,
    pub blobs: BlobStore,
    pub layers: LayerStore,
    pub packages: PackageStore,
    pub symlinks: SymlinkStore,
    pub state: StateStore,
    pub temp: TempStore,
    /// Outside the GC tiers — never walked by `ocx clean`.
    pub shim_bin: ShimBinStore,
    /// Deferred tools; walked by `ocx clean`, rooted in the lock pins rather than reachable from a package.
    pub shims: ShimStore,
    /// The global toolchain home; outside the GC tiers — never walked by `ocx clean`.
    pub toolchain: ToolchainStore,
    /// Cross-process lock directory; outside the index home so a redirected or read-only index gathers no lock files.
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
            shim_bin: ShimBinStore::new(root.join(".bin").join("ocx-shim")),
            shims: ShimStore::new(root.join("shims")),
            toolchain: ToolchainStore::new(root.join("toolchain")),
            locks,
            root,
        }
    }

    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    /// Machine-local patch-descriptor discovery state for `identifier`; never follows `--index` redirection.
    pub fn patch_descriptor_path(&self, identifier: &ocx_oci::PackageRef) -> PathBuf {
        self.root
            .join("state")
            .join("patch-descriptors")
            .join(slugify(identifier.registry()))
            .join(repository_path(identifier.repository()))
            .with_added_extension("json")
    }

    /// Machine-local companion pins for `identifier`'s repository; never follows `--index` redirection.
    ///
    /// Kept out of the local index: a companion was never named, so writing it there would move a package-tier pin.
    pub fn patch_companion_path(&self, identifier: &ocx_oci::PackageRef) -> PathBuf {
        self.root
            .join("state")
            .join("patch-companions")
            .join(slugify(identifier.registry()))
            .join(repository_path(identifier.repository()))
            .with_added_extension("json")
    }

    /// The directory the installed `ocx` itself resolves from.
    ///
    /// Derived from [`ocx_oci::ocx_cli_identifier`], never a literal, or `__OCX_SELF_IMAGE` stops moving it under test.
    pub fn ocx_install_bin_path(&self) -> PathBuf {
        self.symlinks
            .current(&ocx_oci::ocx_cli_identifier())
            .join("content")
            .join("bin")
    }
}

use std::path::PathBuf;

use ocx_util::prelude::StringExt;

/// Converts an OCI identifier component into a filesystem-safe path segment.
///
/// Lossy: a caller validating a source name must compare on this form, or its verdict applies to a different subtree.
pub fn slugify(value: &str) -> String {
    value.to_relaxed_slug()
}

/// Converts an OCI repository name into a relative path with OS-native separators.
///
/// Split per segment: `PathBuf::join("a/b")` would leave mixed separators on Windows.
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
