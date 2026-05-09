// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::{
    oci,
    package::install_info::InstallInfo,
    package_manager::{self, concurrency::Concurrency, error::PackageError, error::PackageErrorKind},
};

use super::super::PackageManager;

impl PackageManager {
    /// Downloads a package and creates install symlinks.
    ///
    /// Delegates to [`PackageManager::pull`] for the actual download and
    /// transitive dependency resolution (see that method for concurrency
    /// safety), then optionally creates:
    ///
    /// - A **candidate** symlink at `symlinks/{repo}/candidates/{tag}` when
    ///   `candidate` is `true` — pins this version as an installed candidate.
    /// - A **current** symlink at `symlinks/{repo}/current` when `select` is
    ///   `true` — makes this version the active selection.
    ///
    /// Both symlinks target the package root; consumers traverse into
    /// `<symlink>/content/`, `<symlink>/entrypoints/`, or `<symlink>/metadata.json`.
    /// Symlinks are managed via [`ReferenceManager::link`] which also creates
    /// back-references in the object store for GC tracking.
    pub async fn install(
        &self,
        package: &oci::Identifier,
        platforms: Vec<oci::Platform>,
        candidate: bool,
        select: bool,
    ) -> Result<InstallInfo, PackageErrorKind> {
        let install_info = self.pull(package, platforms).await?;

        create_install_symlinks(self, package, &install_info, candidate, select).await?;

        Ok(install_info)
    }

    /// Installs multiple packages in parallel using a shared singleflight
    /// group for cross-package diamond dependency deduplication.
    ///
    /// Phase 1: [`pull_all`](PackageManager::pull_all) downloads all packages
    /// and their transitive deps with a shared singleflight group.
    /// Phase 2: Install symlinks are created sequentially (cheap I/O, no
    /// contention benefit from parallelism).
    pub async fn install_all(
        &self,
        packages: Vec<oci::Identifier>,
        platforms: Vec<oci::Platform>,
        candidate: bool,
        select: bool,
        concurrency: Concurrency,
    ) -> Result<Vec<InstallInfo>, package_manager::error::Error> {
        // Phase 1: Pull all packages with shared singleflight group.
        let infos = self.pull_all(&packages, platforms, concurrency).await?;

        // Phase 2: Create symlinks sequentially.
        if candidate || select {
            for (pkg, info) in packages.iter().zip(infos.iter()) {
                create_install_symlinks(self, pkg, info, candidate, select)
                    .await
                    .map_err(|kind| {
                        package_manager::error::Error::InstallFailed(vec![PackageError::new(pkg.clone(), kind)])
                    })?;
            }
        }

        Ok(infos)
    }
}

/// Creates candidate and/or current symlinks for a single package.
///
/// Delegates to [`super::common::wire_selection`] for the `current` symlink
/// update plus the per-registry entry-points index update. Collision
/// detection, lock acquisition, and rollback all live in the shared helper so
/// this path and the `command/select.rs` path stay byte-equivalent.
#[allow(clippy::result_large_err)]
async fn create_install_symlinks(
    mgr: &PackageManager,
    package: &oci::Identifier,
    info: &InstallInfo,
    candidate: bool,
    select: bool,
) -> Result<(), PackageErrorKind> {
    super::common::wire_selection(mgr.file_structure(), package, info, candidate, select).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    /// PackageDir::entrypoints() returns sibling of content/ — verify path shape.
    #[test]
    fn package_dir_entrypoints_path_is_sibling_of_content() {
        use crate::file_structure::PackageDir;
        let dir = std::path::PathBuf::from("/packages/sha256/ab/cdef");
        let pkg_dir = PackageDir { dir };
        assert_eq!(
            pkg_dir.entrypoints(),
            std::path::PathBuf::from("/packages/sha256/ab/cdef/entrypoints")
        );
        assert_eq!(
            pkg_dir.content().parent(),
            pkg_dir.entrypoints().parent(),
            "entrypoints must be sibling of content"
        );
    }
}
