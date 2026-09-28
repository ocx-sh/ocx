// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;
use std::sync::Arc;

use crate::install_info::InstallInfo;

/// Resolved context for a single direct dependency, available during env interpolation.
#[derive(Debug, Clone)]
pub enum DependencyContext {
    Full(Arc<InstallInfo>),
    /// Identifier and content path only, for callers without an [`InstallInfo`].
    PathOnly {
        id: ocx_oci::PinnedPackageRef,
        path: PathBuf,
    },
}

impl DependencyContext {
    pub fn full(install_info: Arc<InstallInfo>) -> Self {
        Self::Full(install_info)
    }

    pub fn path_only(id: ocx_oci::PinnedPackageRef, path: PathBuf) -> Self {
        Self::PathOnly { id, path }
    }

    pub fn install_info(&self) -> Option<&Arc<InstallInfo>> {
        match self {
            Self::Full(info) => Some(info),
            Self::PathOnly { .. } => None,
        }
    }

    /// The dependency's absolute content path.
    pub fn install_path(&self) -> PathBuf {
        match self {
            Self::Full(info) => info.dir().content(),
            Self::PathOnly { path, .. } => path.clone(),
        }
    }

    pub fn identifier(&self) -> &ocx_oci::PinnedPackageRef {
        match self {
            Self::Full(info) => info.identifier(),
            Self::PathOnly { id, .. } => id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn pinned(repo: &str) -> ocx_oci::PinnedPackageRef {
        let hex = "a".repeat(64);
        let id: ocx_oci::PackageRef = format!("ocx.sh/{repo}:1.0@sha256:{hex}").parse().unwrap();
        ocx_oci::PinnedPackageRef::try_from(id).unwrap()
    }

    /// `DependencyContext::path_only` — `install_path()` returns the supplied path.
    #[test]
    fn dependency_context_path_only_resolves_install_path() {
        let path = PathBuf::from("/__OCX_SENTINEL__");
        let ctx = DependencyContext::path_only(pinned("cmake"), path.clone());
        assert_eq!(ctx.install_path(), path);
        assert!(ctx.install_info().is_none(), "PathOnly carries no InstallInfo");
    }

    /// `DependencyContext::Full` — accessors read through to the wrapped InstallInfo.
    #[test]
    fn dependency_context_full_reads_through_install_info() {
        use crate::metadata::Metadata;
        use crate::metadata::bundle::{Bundle, Version};
        use crate::metadata::dependency::Dependencies;
        use crate::metadata::entrypoint::Entrypoints;
        use crate::metadata::env::Env;
        use crate::resolved_package::ResolvedPackage;

        let dir = TempDir::new().unwrap();
        let pkg_root = dir.path().to_path_buf();
        std::fs::create_dir_all(pkg_root.join("content")).unwrap();
        let id = pinned("cmake");
        let info = Arc::new(InstallInfo::new(
            id.clone(),
            Metadata::Bundle(Bundle {
                binaries: None,
                version: Version::V1,
                strip_components: None,
                env: Env::default(),
                dependencies: Dependencies::default(),
                entrypoints: Entrypoints::default(),
                integrations: Default::default(),
            }),
            ResolvedPackage::new(),
            ocx_store::file_structure::PackageDir { dir: pkg_root.clone() },
        ));
        let ctx = DependencyContext::full(Arc::clone(&info));

        assert_eq!(ctx.install_path(), pkg_root.join("content"));
        assert_eq!(ctx.identifier(), &id);
        assert!(ctx.install_info().is_some(), "Full carries the InstallInfo");
        assert!(Arc::ptr_eq(ctx.install_info().unwrap(), &info));
    }
}
