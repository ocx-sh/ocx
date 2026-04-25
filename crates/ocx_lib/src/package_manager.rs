// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

pub mod concurrency;
pub mod error;

mod tasks;

// Re-export types needed by other modules and CLI commands.
pub use concurrency::Concurrency;
pub use error::DependencyError;
pub use tasks::profile_resolve::{ProfileEntryResolution, ResolvedProfileEntry};

use crate::{file_structure, oci, profile::ProfileManager};

/// Central facade for package operations (find, install, uninstall, etc.).
///
/// `PackageManager` holds all the context that tasks need — file structure,
/// index, OCI client — and is cheap to [`Clone`].
///
/// Environment variable resolution uses persisted `resolve.json` files
/// written at install time — see [`resolve_env`](Self::resolve_env).
///
/// Progress reporting is handled via `tracing` spans emitted by task code;
/// the CLI wires up `tracing-indicatif` (or similar) to visualize them.
#[derive(Clone)]
pub struct PackageManager {
    file_structure: file_structure::FileStructure,
    index: oci::index::Index,
    client: Option<oci::Client>,
    default_registry: String,
    profile: ProfileManager,
}

impl PackageManager {
    pub fn new(
        file_structure: file_structure::FileStructure,
        index: oci::index::Index,
        client: Option<oci::Client>,
        default_registry: impl Into<String>,
    ) -> Self {
        let default_registry = default_registry.into();
        let profile = ProfileManager::new(file_structure.clone());
        Self {
            file_structure,
            index,
            client,
            default_registry,
            profile,
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

    pub fn profile(&self) -> &ProfileManager {
        &self.profile
    }

    pub fn is_offline(&self) -> bool {
        self.client.is_none()
    }

    /// Boundary primitive for hook-style commands (`shell-hook`, `hook-env`,
    /// future `generate direnv`) that must NOT contact any registry,
    /// regardless of the global `--remote` / `--offline` flags.
    ///
    /// Builds a fresh [`PackageManager`] using the supplied local cache
    /// `local_index` as the *only* index source: chain mode is forced to
    /// [`oci::index::ChainMode::Offline`], and the OCI client is dropped to
    /// `None`. Any incidental tag/manifest lookup short-circuits to the
    /// local cache; an attempt to use the (now-absent) client surfaces as
    /// `Error::OfflineMode`. This is the layer the security boundary docs
    /// in ADR §5B (decision 5B) reference — see
    /// `.claude/artifacts/adr_project_toolchain_config.md`.
    ///
    /// Caller passes the local-index handle separately because the manager
    /// holds a type-erased `Index` (which may be `Default`, `Remote`, or
    /// already `Offline`); reaching back through the type-erased boundary
    /// would couple this primitive to `ChainedIndex` internals. The CLI
    /// `Context` already exposes `local_index().clone()`, so the call site
    /// is `context.manager().offline_view(context.local_index().clone())`.
    pub fn offline_view(&self, local_index: oci::index::LocalIndex) -> Self {
        let offline_index = oci::index::Index::from_chained(local_index, Vec::new(), oci::index::ChainMode::Offline);
        Self {
            file_structure: self.file_structure.clone(),
            index: offline_index,
            client: None,
            default_registry: self.default_registry.clone(),
            profile: self.profile.clone(),
        }
    }
}
