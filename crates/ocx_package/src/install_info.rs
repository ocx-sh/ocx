// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::sync::Arc;

use ocx_store::{file_structure::PackageDir, file_structure::ShimDir};

use super::{metadata, resolved_package::ResolvedPackage};

/// What a deferred tool (composed onto `PATH` before its package directory
/// exists) adds to composition beyond a materialized package.
///
/// Deferral is an input, never a filesystem probe, or the composed env differs
/// before and after the first launcher invocation.
#[derive(Debug, Clone)]
pub struct DeferredComposition {
    /// Its `bin/` is pushed first for this root, making it the lowest-precedence entry.
    shim: ShimDir,

    /// The tool's transitive closure, deps before dependents; each member's
    /// `dir()` does not exist yet.
    closure: Vec<Arc<InstallInfo>>,
}

impl DeferredComposition {
    /// Binds a shim directory to the closure its carriers were read from.
    pub fn new(shim: ShimDir, closure: Vec<Arc<InstallInfo>>) -> Self {
        Self { shim, closure }
    }

    /// The generated shim directory whose `bin/` the composer pushes first.
    pub fn shim(&self) -> &ShimDir {
        &self.shim
    }

    /// The closure member for `identifier`, matched advisory-stripped.
    ///
    /// `None` is a defect to surface; never fall back to reading the package
    /// directory, which does not exist.
    pub fn member(&self, identifier: &ocx_oci::PinnedPackageRef) -> Option<&Arc<InstallInfo>> {
        let wanted = identifier.strip_advisory();
        self.closure
            .iter()
            .find(|member| member.identifier().strip_advisory() == wanted)
    }
}

#[derive(Debug, Clone)]
pub struct InstallInfo {
    identifier: ocx_oci::PinnedPackageRef,
    metadata: metadata::Metadata,
    resolved: ResolvedPackage,
    dir: PackageDir,
    /// The platform the install resolved to; `None` where nothing resolves.
    platform: Option<ocx_oci::Platform>,

    /// Set iff this root is deferred; a consumer that assumes `dir()` exists
    /// must refuse a `Some`, or it writes a dangling install symlink.
    deferred: Option<Arc<DeferredComposition>>,

    /// The registry the content is fetched from, before `[mirrors]` rewrites;
    /// unlike [`identifier`](Self::identifier)'s logical registry.
    transport_registry: Option<String>,

    /// Further names a forwarding composition resolved this digest under; patch rules
    /// match them alongside [`identifier`](Self::identifier).
    aliases: Vec<ocx_oci::PackageRef>,
}

impl InstallInfo {
    pub fn new(
        identifier: ocx_oci::PinnedPackageRef,
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
            deferred: None,
            transport_registry: None,
            aliases: Vec::new(),
        }
    }

    /// Marks this root as deferred; only the lazy compose-root builder may call it.
    #[must_use]
    pub fn with_deferred(mut self, deferred: DeferredComposition) -> Self {
        self.deferred = Some(Arc::new(deferred));
        self
    }

    pub fn deferred(&self) -> Option<&DeferredComposition> {
        self.deferred.as_deref()
    }

    #[must_use]
    pub fn with_platform(mut self, platform: ocx_oci::Platform) -> Self {
        self.platform = Some(platform);
        self
    }

    #[must_use]
    pub fn with_transport_registry(mut self, registry: impl Into<String>) -> Self {
        self.transport_registry = Some(registry.into());
        self
    }

    #[must_use]
    pub fn with_aliases(mut self, aliases: Vec<ocx_oci::PackageRef>) -> Self {
        self.aliases = aliases;
        self
    }

    pub fn aliases(&self) -> &[ocx_oci::PackageRef] {
        &self.aliases
    }

    pub fn platform(&self) -> Option<&ocx_oci::Platform> {
        self.platform.as_ref()
    }

    pub fn transport_registry(&self) -> Option<&str> {
        self.transport_registry.as_deref()
    }

    /// Whether the resolved platform runs on this host; `None`,
    /// [`Any`](ocx_oci::Platform::any) and an unknown host yield `true`.
    pub fn is_host_runnable(&self) -> bool {
        ocx_oci::Platform::host_can_run(self.platform())
    }

    pub fn identifier(&self) -> &ocx_oci::PinnedPackageRef {
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
