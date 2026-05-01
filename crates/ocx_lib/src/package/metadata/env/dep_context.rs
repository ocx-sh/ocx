// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Resolved context for a single direct dependency, available during env
//! interpolation.
//!
//! Lives in its own module so the env-resolution surface is one concept per
//! file: the resolver computes; the dep context describes the inputs.

use std::path::PathBuf;
use std::sync::Arc;

use crate::oci;
use crate::package::install_info::InstallInfo;

/// Resolved context for a single direct dependency, available during env interpolation.
///
/// Keyed by `dep.name()` (the explicit name or repository basename). Two
/// variants encode the available data so consumers can never silently fan out
/// from a fake install record:
///
/// - [`DependencyContext::Full`] — backs the runtime path. Carries the real
///   `Arc<InstallInfo>`, so future template fields
///   (`${deps.NAME.version}`, `${deps.NAME.digest}`) can read metadata
///   without an extra lookup.
/// - [`DependencyContext::PathOnly`] — backs the publish-time validator and
///   the env-only runtime callers (the two-env composer) where no full
///   `InstallInfo` is loaded. Only `installPath` is resolvable;
///   metadata-dependent fields return `None`.
#[derive(Debug, Clone)]
pub enum DependencyContext {
    /// Full install record — every field on [`InstallInfo`] is available.
    Full(Arc<InstallInfo>),
    /// Identifier and resolved content path only — no metadata, no resolved deps.
    PathOnly { id: oci::PinnedIdentifier, path: PathBuf },
}

impl DependencyContext {
    /// Constructs a `DependencyContext` wrapping real install info.
    pub fn full(install_info: Arc<InstallInfo>) -> Self {
        Self::Full(install_info)
    }

    /// Constructs a context from an identifier and a content path only.
    ///
    /// Used by call sites that have no full `InstallInfo` available —
    /// `validate_entrypoints` (publish-time sentinels) and the two-env
    /// composer (runtime, where only `${...installPath}` resolution is
    /// required). Metadata-dependent template fields are
    /// unresolvable on this variant and return `None`.
    pub fn path_only(id: oci::PinnedIdentifier, path: PathBuf) -> Self {
        Self::PathOnly { id, path }
    }

    /// Returns the underlying `Arc<InstallInfo>` when the variant carries one.
    ///
    /// Returns `None` for [`DependencyContext::PathOnly`] — there is no install record.
    pub fn install_info(&self) -> Option<&Arc<InstallInfo>> {
        match self {
            Self::Full(info) => Some(info),
            Self::PathOnly { .. } => None,
        }
    }

    /// Returns the absolute content path for this dependency (`packages/.../content/`).
    pub fn install_path(&self) -> PathBuf {
        match self {
            Self::Full(info) => info.dir().content(),
            Self::PathOnly { path, .. } => path.clone(),
        }
    }

    /// Returns the full pinned OCI identifier for this dependency.
    pub fn identifier(&self) -> &oci::PinnedIdentifier {
        match self {
            Self::Full(info) => info.identifier(),
            Self::PathOnly { id, .. } => id,
        }
    }

    /// Resolves a named field to a string value.
    ///
    /// `"installPath"` is supported on every variant. Future
    /// metadata-dependent fields (`"version"`, `"digest"`) will resolve only
    /// on [`DependencyContext::Full`] and return `None` on
    /// [`DependencyContext::PathOnly`] — the type discriminates at compile
    /// time, no synthetic-empty fallbacks.
    pub fn resolve_field(&self, field: &str) -> Option<String> {
        match field {
            "installPath" => Some(self.install_path().to_string_lossy().into_owned()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn pinned(repo: &str) -> oci::PinnedIdentifier {
        let hex = "a".repeat(64);
        let id: oci::Identifier = format!("ocx.sh/{repo}:1.0@sha256:{hex}").parse().unwrap();
        oci::PinnedIdentifier::try_from(id).unwrap()
    }

    /// `DependencyContext::path_only` — `install_path()` returns the supplied path.
    #[test]
    fn dependency_context_path_only_resolves_install_path() {
        let path = PathBuf::from("/__OCX_SENTINEL__");
        let ctx = DependencyContext::path_only(pinned("cmake"), path.clone());
        assert_eq!(ctx.install_path(), path);
        assert_eq!(ctx.resolve_field("installPath").as_deref(), Some("/__OCX_SENTINEL__"));
        assert!(ctx.install_info().is_none(), "PathOnly carries no InstallInfo");
    }

    /// `DependencyContext::resolve_field` — unknown fields return None.
    #[test]
    fn dependency_context_resolve_field_unknown_returns_none() {
        let dir = TempDir::new().unwrap();
        let ctx = DependencyContext::path_only(pinned("cmake"), dir.path().to_path_buf());
        assert!(ctx.resolve_field("version").is_none());
        assert!(ctx.resolve_field("digest").is_none());
        assert!(ctx.resolve_field("").is_none());
    }

    /// `DependencyContext::Full` — accessors read through to the wrapped InstallInfo.
    #[test]
    fn dependency_context_full_reads_through_install_info() {
        use crate::package::metadata::Metadata;
        use crate::package::metadata::bundle::{Bundle, Version};
        use crate::package::metadata::dependency::Dependencies;
        use crate::package::metadata::entrypoint::Entrypoints;
        use crate::package::metadata::env::Env;
        use crate::package::resolved_package::ResolvedPackage;

        let dir = TempDir::new().unwrap();
        let pkg_root = dir.path().to_path_buf();
        std::fs::create_dir_all(pkg_root.join("content")).unwrap();
        let id = pinned("cmake");
        let info = Arc::new(InstallInfo::new(
            id.clone(),
            Metadata::Bundle(Bundle {
                version: Version::V1,
                strip_components: None,
                env: Env::default(),
                dependencies: Dependencies::default(),
                entrypoints: Entrypoints::default(),
            }),
            ResolvedPackage::new(),
            crate::file_structure::PackageDir { dir: pkg_root.clone() },
        ));
        let ctx = DependencyContext::full(Arc::clone(&info));

        assert_eq!(ctx.install_path(), pkg_root.join("content"));
        assert_eq!(ctx.identifier(), &id);
        assert!(ctx.install_info().is_some(), "Full carries the InstallInfo");
        assert!(Arc::ptr_eq(ctx.install_info().unwrap(), &info));
    }
}
