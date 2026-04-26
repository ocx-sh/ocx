// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::Serialize;

use crate::oci;

use super::{metadata, resolved_package::ResolvedPackage};

#[derive(Debug, Clone, Serialize)]
pub struct InstallInfo {
    pub identifier: oci::PinnedIdentifier,
    pub metadata: metadata::Metadata,
    pub resolved: ResolvedPackage,
    pub content: std::path::PathBuf,
}

impl InstallInfo {
    /// Returns the package root directory (the directory containing `content/`,
    /// `metadata.json`, `entrypoints/`, `refs/`, etc.).
    ///
    /// The `content` field always points at `<pkg_root>/content` by
    /// construction (see `tasks::common::find_in_store` and the pull
    /// pipeline). `parent()` navigates to the package root.
    pub fn package_root(&self) -> &std::path::Path {
        self.content.parent().unwrap_or(&self.content)
    }

    /// Constructs a minimal `InstallInfo` for use as a publish-time sentinel.
    ///
    /// The `content` path is the sentinel path passed by the caller; all other
    /// metadata fields use empty/default values. Only intended for use inside
    /// `validate_entrypoints` and `DependencyContext::sentinel` — not for
    /// real install records.
    pub fn new_for_sentinel(identifier: oci::PinnedIdentifier, content: std::path::PathBuf) -> Self {
        use super::metadata::bundle::{Bundle, Version};
        use super::metadata::entrypoint::Entrypoints;
        use super::metadata::{dependency::Dependencies, env::Env};
        Self {
            identifier,
            metadata: metadata::Metadata::Bundle(Bundle {
                version: Version::V1,
                strip_components: None,
                env: Env::default(),
                dependencies: Dependencies::default(),
                entrypoints: Entrypoints::default(),
            }),
            resolved: ResolvedPackage::new(),
            content,
        }
    }
}
