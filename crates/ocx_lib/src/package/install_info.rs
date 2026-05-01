// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::{file_structure::PackageDir, oci};

use super::{metadata, resolved_package::ResolvedPackage};

#[derive(Debug, Clone)]
pub struct InstallInfo {
    identifier: oci::PinnedIdentifier,
    metadata: metadata::Metadata,
    resolved: ResolvedPackage,
    dir: PackageDir,
}

impl InstallInfo {
    pub fn new(
        identifier: oci::PinnedIdentifier,
        metadata: metadata::Metadata,
        resolved: ResolvedPackage,
        dir: PackageDir,
    ) -> Self {
        Self {
            identifier,
            metadata,
            resolved,
            dir,
        }
    }

    pub fn identifier(&self) -> &oci::PinnedIdentifier {
        &self.identifier
    }

    pub fn metadata(&self) -> &metadata::Metadata {
        &self.metadata
    }

    pub fn resolved(&self) -> &ResolvedPackage {
        &self.resolved
    }

    pub fn dir(&self) -> &PackageDir {
        &self.dir
    }
}
