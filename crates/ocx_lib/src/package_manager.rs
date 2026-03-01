pub mod error;
pub mod tasks;

use crate::{file_structure, oci};

/// Central facade for package operations (find, install, uninstall, etc.).
///
/// `PackageManager` holds all the context that tasks need — file structure,
/// index, OCI client — and is cheap to [`Clone`].
/// When tasks are migrated into this module, they become methods on
/// `PackageManager`, and any task can issue sub-tasks by calling other methods
/// on the same instance (e.g. installing transitive dependencies).
///
/// Progress reporting is handled via `tracing` spans emitted by task code;
/// the CLI wires up `tracing-indicatif` (or similar) to visualize them.
#[derive(Clone)]
pub struct PackageManager {
    file_structure: file_structure::FileStructure,
    index: oci::index::Index,
    client: Option<oci::Client>,
    default_registry: String,
}

impl PackageManager {
    pub fn new(
        file_structure: file_structure::FileStructure,
        index: oci::index::Index,
        client: Option<oci::Client>,
        default_registry: impl Into<String>,
    ) -> Self {
        Self {
            file_structure,
            index,
            client,
            default_registry: default_registry.into(),
        }
    }

    pub fn file_structure(&self) -> &file_structure::FileStructure {
        &self.file_structure
    }

    pub fn index(&self) -> &oci::index::Index {
        &self.index
    }

    /// Returns the OCI client, or `Err(OfflineMode)` if none is available.
    pub fn client(&self) -> crate::Result<&oci::Client> {
        self.client.as_ref().ok_or(crate::Error::OfflineMode)
    }

    pub fn default_registry(&self) -> &str {
        &self.default_registry
    }

    pub fn is_offline(&self) -> bool {
        self.client.is_none()
    }
}
