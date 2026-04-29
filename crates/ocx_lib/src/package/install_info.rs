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
    /// `content` is always `<pkg_root>/content` by construction (see
    /// `tasks::common::find_in_store` and the pull pipeline), so `parent()` is
    /// always `Some`. The `unwrap_or(&self.content)` branch is unreachable
    /// today — kept as a defensive fallback if a future content layout drops
    /// the `content/` child.
    pub fn package_root(&self) -> &std::path::Path {
        self.content.parent().unwrap_or(&self.content)
    }
}
