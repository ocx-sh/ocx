// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::sync::Arc;

use crate::{
    file_structure::{PackageDir, ShimDir},
    oci,
};

use super::{metadata, resolved_package::ResolvedPackage};

/// Everything a **deferred** tool contributes to composition that a
/// materialized package does not (plan contracts C-012 / C-013 / C-020,
/// [#302](https://github.com/ocx-sh/ocx/issues/302)).
///
/// A deferred tool is composed onto `PATH` with no package directory on disk:
/// its content materializes on the first invocation of one of its generated
/// launchers. Everything the composer needs is therefore either a *path* it can
/// compute (`PackageDir` is pure path arithmetic over the pinned identifier) or
/// a *carrier* it must read from somewhere other than the package directory —
/// and this struct is that somewhere.
///
/// **Deferral is an input, never a probe.** Nothing here is derived by asking
/// the filesystem whether a package directory exists: C-013 fixes the composed
/// env as a function of (lock, `lazy-mode`, metadata availability) alone, so a
/// composer that decided by probing would emit one env before the first
/// invocation and a different one after — the exact variance S-005 asserts is
/// absent.
#[derive(Debug, Clone)]
pub struct DeferredComposition {
    /// The generated shim directory (C-003 / C-008).
    ///
    /// Its `bin/` — never its root — is the PATH entry the composer pushes
    /// **first** for this root, which under the consumer's prepend semantics
    /// makes it the *lowest*-precedence entry of the block (C-012).
    shim: ShimDir,

    /// The tool's transitive closure, each member's metadata already read from
    /// the ref-linked config blob rather than from a package directory (C-020).
    ///
    /// Ordered deps-before-dependents and aligned with the synthesized
    /// [`InstallInfo::resolved`]`().dependencies` of the owning root, so the
    /// composer walks a deferred root's TC exactly as it walks a materialized
    /// one. Each member is itself an `InstallInfo` whose `dir()` names a
    /// package directory that does not exist yet — the one the shim will
    /// materialize into, and the one `${installPath}` must already resolve to.
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

    /// The closure member for `identifier`, matched on the advisory-stripped
    /// identifier (the same key the composer dedups TC entries by).
    ///
    /// `None` means the composer reached a TC entry this closure does not
    /// carry, which is the C-020 defect condition — a consumer must surface it,
    /// never fall back to reading a package directory that does not exist.
    pub fn member(&self, identifier: &oci::PinnedIdentifier) -> Option<&Arc<InstallInfo>> {
        let wanted = identifier.strip_advisory();
        self.closure
            .iter()
            .find(|member| member.identifier().strip_advisory() == wanted)
    }
}

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

    /// Set iff this root is a **deferred** tool — composed onto `PATH` with no
    /// package directory on disk (plan contract C-012).
    ///
    /// `None` on every other path, which is every path but the lazy compose-root
    /// builder, so no existing consumer changes behaviour. A consumer that
    /// writes an install symlink, or otherwise assumes `dir()` exists, must
    /// refuse a `Some` — a `candidates/{tag}` link pointing at a directory the
    /// shim has not created yet is a dangling install (C-021 keeps
    /// `--lazy-mode` off `install`/`select` for the same reason).
    deferred: Option<Arc<DeferredComposition>>,

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
            deferred: None,
            transport_registry: None,
        }
    }

    /// Marks this root as deferred, returning `self` for chaining after
    /// [`new`](Self::new) — the sibling of [`with_platform`](Self::with_platform).
    ///
    /// The only producer is the lazy compose-root builder
    /// (`composer::PackageManager::compose_roots`); nothing else may mint a
    /// root whose package directory does not exist.
    #[must_use]
    pub fn with_deferred(mut self, deferred: DeferredComposition) -> Self {
        self.deferred = Some(Arc::new(deferred));
        self
    }

    /// The deferred composition for this root, or `None` when it is an
    /// ordinary materialized package.
    pub fn deferred(&self) -> Option<&DeferredComposition> {
        self.deferred.as_deref()
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
