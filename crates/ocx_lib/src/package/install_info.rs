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
    /// The platform the install resolved to, when known.
    ///
    /// Set by the install/pull path from the resolution chain; `None` on paths
    /// that build an `InstallInfo` without platform context (e.g. `find_symlink`,
    /// composer test fixtures). Consumed by the candidate-symlink gate to avoid
    /// pointing a host's `candidates/{tag}` slot at a foreign-platform root
    /// (issue #179).
    platform: Option<oci::Platform>,
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
            platform: None,
        }
    }

    /// Records the platform this install resolved to, returning `self` for
    /// chaining after [`new`](Self::new).
    #[must_use]
    pub fn with_platform(mut self, platform: oci::Platform) -> Self {
        self.platform = Some(platform);
        self
    }

    /// The platform this install resolved to, or `None` when the constructing
    /// path had no platform context.
    pub fn platform(&self) -> Option<&oci::Platform> {
        self.platform.as_ref()
    }

    /// Whether this install's resolved platform is runnable on the current host.
    ///
    /// The single host-only gate (issue #179) shared by the candidate/`current`
    /// symlink writer ([`wire_selection`](crate::package_manager::tasks)) and the
    /// CLI install reporter, so both agree on whether a host symlink was written.
    /// `None`/[`Any`](oci::Platform::any)/unknown-host all resolve to `true`.
    pub fn is_host_runnable(&self) -> bool {
        oci::Platform::host_can_run(self.platform())
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
