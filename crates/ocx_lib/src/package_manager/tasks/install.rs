// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use tokio::task::JoinSet;

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
    /// Phase 2: Install symlinks are created in parallel via a [`JoinSet`].
    /// Candidate symlinks land at distinct per-tag paths
    /// (`candidates/{tag}`), so concurrent writes never collide on the same
    /// file (same-tag duplication is prevented upstream by pull singleflight).
    /// Only the floating `current` symlink is contended; that write is guarded
    /// by the per-repo `.select.lock` inside
    /// [`super::common::wire_selection`]. Results are collected in completion
    /// order and all errors are gathered before returning.
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

        // Phase 2: Create symlinks in parallel.
        if candidate || select {
            let mut tasks: JoinSet<(usize, Result<(), PackageErrorKind>)> = JoinSet::new();

            for (index, (pkg, info)) in packages.iter().zip(infos.iter()).enumerate() {
                let mgr = self.clone();
                let pkg = pkg.clone();
                let info = info.clone();
                tasks.spawn(async move {
                    let result = create_install_symlinks(&mgr, &pkg, &info, candidate, select).await;
                    (index, result)
                });
            }

            let mut errors: Vec<PackageError> = Vec::new();
            while let Some(join_result) = tasks.join_next().await {
                match join_result {
                    Ok((index, Err(kind))) => {
                        errors.push(PackageError::new(packages[index].clone(), kind));
                    }
                    Ok((_, Ok(()))) => {}
                    Err(panic) => {
                        // A task panicked — abort remaining and propagate.
                        tasks.abort_all();
                        std::panic::resume_unwind(panic.into_panic());
                    }
                }
            }

            if !errors.is_empty() {
                return Err(package_manager::error::Error::InstallFailed(errors));
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
