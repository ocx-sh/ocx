// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

use crate::oci;

use super::super::PackageManager;
use super::garbage_collection::GarbageCollector;

impl PackageManager {
    /// Purge a single object and its orphaned transitive dependencies.
    ///
    /// Returns the list of actually deleted object directories (may be empty
    /// if the object is still reachable from another root).
    pub async fn purge(&self, identifier: &oci::PinnedIdentifier) -> crate::Result<Vec<PathBuf>> {
        let obj_dir = self.file_structure().packages.path(identifier);
        let profile = self.profile.snapshot();
        let gc = GarbageCollector::build(self.file_structure(), &profile).await?;
        gc.purge(&[obj_dir]).await
    }

    /// Batch purge: single graph build for all identifiers, single deletion pass.
    ///
    /// More efficient than calling [`purge`] in a loop because the
    /// reachability graph is built once.
    pub async fn purge_all(&self, identifiers: &[oci::PinnedIdentifier]) -> crate::Result<Vec<PathBuf>> {
        let obj_dirs: Vec<PathBuf> = identifiers
            .iter()
            .map(|id| self.file_structure().packages.path(id))
            .collect();
        let profile = self.profile.snapshot();
        let gc = GarbageCollector::build(self.file_structure(), &profile).await?;
        gc.purge(&obj_dirs).await
    }
}
