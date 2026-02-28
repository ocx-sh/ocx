mod object_store;
mod index_store;
mod install_store;

pub use object_store::{ObjectDir, ObjectStore};
pub use index_store::IndexStore;
pub use install_store::{InstallStore, SymlinkKind};

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
        }
    }
}

pub fn default_ocx_root() -> Option<std::path::PathBuf> {
    std::env::home_dir().map(|home| home.join(".ocx"))
}
