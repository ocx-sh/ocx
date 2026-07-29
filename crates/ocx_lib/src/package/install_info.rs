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
    /// Set from the resolution chain by every path that has one — `pull`'s
    /// `setup_owned` and `find` alike, so a package reached from the store and
    /// the same package freshly pulled describe themselves identically. `None`
    /// only on paths that resolve nothing (e.g. `find_symlink`, composer test
    /// fixtures).
    ///
    /// Consumed by the candidate-symlink gate, to avoid pointing a host's
    /// `candidates/{tag}` slot at a foreign-platform root (issue #179), and by
    /// the execution record, which reports it as the package's *selected*
    /// platform — distinct from the platform the invocation requested.
    platform: Option<oci::Platform>,
    /// The registry this install's content is fetched from, when known.
    ///
    /// Distinct from [`identifier`](Self::identifier)'s registry, which is the
    /// *logical* namespace the package is named by. Index indirection separates
    /// the two: an `ocx.sh` index root can point at `ghcr.io/acme/tool`, and
    /// then the logical name says `ocx.sh` while every byte comes from
    /// `ghcr.io`. This field is that second half — the host the transport
    /// addressed, before any `[mirrors]` rewrite of it.
    ///
    /// Set from `ResolvedChain::transport_pinned` by the paths that resolve
    /// through the index — `pull`'s `setup_owned` and `find` alike, so a package
    /// reached from the store and the same package freshly pulled describe
    /// themselves identically. `None` on paths that resolve nothing
    /// (`find_symlink`, `install_info_from_package_root`, composer fixtures),
    /// where no provenance is in hand to record.
    ///
    /// Consumed by the execution record's `resolution.registries`, which answers
    /// "where did the content come from" and must not answer with the logical
    /// namespace instead.
    transport_registry: Option<String>,
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
            transport_registry: None,
        }
    }

    /// Records the platform this install resolved to, returning `self` for
    /// chaining after [`new`](Self::new).
    #[must_use]
    pub fn with_platform(mut self, platform: oci::Platform) -> Self {
        self.platform = Some(platform);
        self
    }

    /// Records the registry this install's content is fetched from, returning
    /// `self` for chaining after [`new`](Self::new).
    #[must_use]
    pub fn with_transport_registry(mut self, registry: impl Into<String>) -> Self {
        self.transport_registry = Some(registry.into());
        self
    }

    /// The platform this install resolved to, or `None` when the constructing
    /// path had no platform context.
    pub fn platform(&self) -> Option<&oci::Platform> {
        self.platform.as_ref()
    }

    /// The registry this install's content is fetched from, or `None` when the
    /// constructing path resolved nothing through the index.
    pub fn transport_registry(&self) -> Option<&str> {
        self.transport_registry.as_deref()
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
