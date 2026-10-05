// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::Path;

use tokio::task::JoinSet;

use crate::{concurrency::Concurrency, error::PackageError, error::PackageErrorKind};
use ocx_package::install_info::InstallInfo;

use super::{super::PackageManager, patch_discovery::PatchDiscoveryMode};

impl PackageManager {
    /// Pulls a package, then creates its candidate and/or current symlinks and discovers patches.
    pub async fn install(
        &self,
        package: &ocx_oci::PackageRef,
        platform: ocx_oci::Platform,
        candidate: bool,
        select: bool,
    ) -> Result<InstallInfo, PackageErrorKind> {
        let install_info = self.pull(package, platform.clone()).await?;

        create_install_symlinks(self, package, &install_info, candidate, select).await?;

        self.discover_patches_best_effort(package, &platform, PatchDiscoveryMode::Revalidate)
            .await?;

        Ok(install_info)
    }

    /// Installs multiple packages in parallel, deduplicating shared dependencies.
    pub async fn install_all(
        &self,
        packages: Vec<ocx_oci::PackageRef>,
        platform: ocx_oci::Platform,
        candidate: bool,
        select: bool,
        concurrency: Concurrency,
        // `true` for internal installs of `ocx` itself (self update, bootstrap), which discover no patches.
        skip_discovery: bool,
    ) -> Result<Vec<InstallInfo>, crate::error::Error> {
        // Discovery runs below, once the symlinks exist.
        let infos = self.pull_all(&packages, platform.clone(), concurrency, true).await?;

        // Parallel is safe: candidate paths are per-tag and `current` is guarded by `.select.lock`.
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

            let mut indexed_errors: Vec<(usize, PackageError)> = Vec::new();
            while let Some(join_result) = tasks.join_next().await {
                match join_result {
                    Ok((index, Err(kind))) => {
                        indexed_errors.push((index, PackageError::new(packages[index].clone(), kind)));
                    }
                    Ok((_, Ok(()))) => {}
                    Err(panic) => {
                        tasks.abort_all();
                        std::panic::resume_unwind(panic.into_panic());
                    }
                }
            }

            if !indexed_errors.is_empty() {
                let errors = finalize_indexed_errors(indexed_errors);
                return Err(crate::error::Error::InstallFailed(errors));
            }
        }

        if !skip_discovery {
            self.discover_patches_all(&packages, &platform, PatchDiscoveryMode::Revalidate, concurrency)
                .await?;
        }

        Ok(infos)
    }
}

impl PackageManager {
    /// Points `link`, an absolute caller-chosen path, at `info`'s package root and records it as a GC root.
    ///
    /// Refuses a path inside the ocx home, where it could stand in for an ocx-owned link, and a path that
    /// holds anything but a link into the package store, so a user's file or directory is never replaced.
    #[allow(clippy::result_large_err)]
    pub async fn link_install(&self, info: &InstallInfo, link: &Path) -> Result<(), PackageErrorKind> {
        let fs = self.file_structure();
        let pkg_root = info.dir().dir.as_path();

        // A deferred package has no directory yet, so a link to it would publish a dangling install.
        if info.deferred().is_some() {
            return Err(PackageErrorKind::Internal(crate::error::file_error(
                pkg_root,
                std::io::Error::other("refusing to point an install link at a deferred package"),
            )));
        }

        if link.starts_with(fs.root()) || !link_is_replaceable(&fs.packages, link) {
            return Err(PackageErrorKind::LinkPathOccupied(link.to_path_buf()));
        }

        log::debug!("Creating install link at '{}'.", link.display());
        super::common::reference_manager(fs)
            .link(link, pkg_root)
            .map_err(|error| PackageErrorKind::Internal(error.into()))
    }
}

/// Absent, or a link into the package store; any other lookup failure is left for the link write to report.
fn link_is_replaceable(packages: &ocx_store::file_structure::PackageStore, link: &Path) -> bool {
    match std::fs::symlink_metadata(link) {
        Err(_) => true,
        Ok(_) if ocx_util::fs::symlink::is_link(link) => super::common::leads_into_store(packages, link),
        Ok(_) => false,
    }
}

/// Creates candidate and/or current symlinks for a single package.
#[allow(clippy::result_large_err)]
async fn create_install_symlinks(
    mgr: &PackageManager,
    package: &ocx_oci::PackageRef,
    info: &InstallInfo,
    candidate: bool,
    select: bool,
) -> Result<(), PackageErrorKind> {
    super::common::wire_selection(mgr.file_structure(), package, info, candidate, select).await?;
    Ok(())
}

/// Sorts errors back into input order, or the exit code (taken from the first error) depends on a completion race.
pub(super) fn finalize_indexed_errors(mut indexed_errors: Vec<(usize, PackageError)>) -> Vec<PackageError> {
    indexed_errors.sort_by_key(|(index, _)| *index);
    indexed_errors.into_iter().map(|(_, error)| error).collect()
}

#[cfg(test)]
mod link_install_tests {
    use std::path::{Path, PathBuf};

    use ocx_index::{ChainMode, Index, IndexStore, LocalConfig, LocalIndex};
    use ocx_package::{install_info::InstallInfo, resolved_package::ResolvedPackage};
    use ocx_store::file_structure::{FileStructure, PackageDir};

    use crate::{PackageManager, error::PackageErrorKind};

    struct Fixture {
        _tmp: tempfile::TempDir,
        manager: PackageManager,
        info: InstallInfo,
        outside: PathBuf,
    }

    /// An offline manager whose home holds one package root, plus a directory outside that home.
    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap();
        let home = root.join("home");
        let fs = FileStructure::with_root(home.clone());
        let pkg_root = fs.packages.root().join("registry").join("pkg");
        std::fs::create_dir_all(pkg_root.join("content")).unwrap();
        let index = Index::from_chained(
            LocalIndex::new(LocalConfig {
                index_store: IndexStore::new(home.join("index")),
            }),
            vec![],
            ChainMode::Offline,
        );
        let identifier = ocx_oci::PinnedPackageRef::try_from(
            ocx_oci::PackageRef::new_registry("pkg", "example.com")
                .clone_with_digest(ocx_oci::Digest::Sha256("a".repeat(64))),
        )
        .unwrap();
        let info = InstallInfo::new(
            identifier,
            serde_json::from_str(r#"{"type":"bundle","version":1,"env":[]}"#).unwrap(),
            ResolvedPackage::new(),
            PackageDir::with_root(pkg_root),
        );
        let outside = root.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        Fixture {
            _tmp: tmp,
            manager: PackageManager::new(fs, index, None, "example.com"),
            info,
            outside,
        }
    }

    fn assert_occupied(result: Result<(), PackageErrorKind>, link: &Path) {
        match result {
            Err(PackageErrorKind::LinkPathOccupied(path)) => assert_eq!(path, link),
            other => panic!("expected LinkPathOccupied, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn absent_path_is_linked_and_rooted() {
        let f = fixture();
        let link = f.outside.join("tools").join("pkg");
        f.manager.link_install(&f.info, &link).await.unwrap();
        assert_eq!(std::fs::read_link(&link).unwrap(), f.info.dir().dir);
        let back_ref = f
            .info
            .dir()
            .refs_symlinks_dir()
            .join(ocx_store::reference_manager::ReferenceManager::name_for_path(&link));
        assert_eq!(std::fs::read_link(back_ref).unwrap(), link);
    }

    #[tokio::test]
    async fn existing_store_link_is_relinked() {
        let f = fixture();
        let link = f.outside.join("pkg");
        f.manager.link_install(&f.info, &link).await.unwrap();
        f.manager.link_install(&f.info, &link).await.unwrap();
        assert_eq!(std::fs::read_link(&link).unwrap(), f.info.dir().dir);
    }

    #[tokio::test]
    async fn regular_file_is_refused_and_kept() {
        let f = fixture();
        let link = f.outside.join("pkg");
        std::fs::write(&link, b"user data").unwrap();
        assert_occupied(f.manager.link_install(&f.info, &link).await, &link);
        assert_eq!(std::fs::read(&link).unwrap(), b"user data");
    }

    #[tokio::test]
    async fn directory_is_refused() {
        let f = fixture();
        let link = f.outside.join("pkg");
        std::fs::create_dir(&link).unwrap();
        assert_occupied(f.manager.link_install(&f.info, &link).await, &link);
        assert!(link.is_dir());
    }

    #[tokio::test]
    async fn link_outside_the_store_is_refused() {
        let f = fixture();
        let target = f.outside.join("elsewhere");
        std::fs::create_dir(&target).unwrap();
        let link = f.outside.join("pkg");
        ocx_util::fs::symlink::create(&target, &link).unwrap();
        assert_occupied(f.manager.link_install(&f.info, &link).await, &link);
        assert_eq!(std::fs::read_link(&link).unwrap(), target);
    }

    #[tokio::test]
    async fn path_inside_the_ocx_home_is_refused() {
        let f = fixture();
        let link = f.manager.file_structure().root().join("symlinks").join("pkg");
        assert_occupied(f.manager.link_install(&f.info, &link).await, &link);
        assert!(!link.exists());
    }
}

#[cfg(test)]
mod tests {

    use crate::error::{PackageError, PackageErrorKind};

    /// Regression: `install_all` collects symlink failures in `JoinSet`
    /// completion order, which is nondeterministic. The exit-code classifier
    /// derives the code from the first `PackageError`, so the batch must be
    /// sorted by spawn index before being wrapped in `InstallFailed`. This test
    /// feeds errors that arrive in reverse completion order with distinct kinds
    /// (different exit codes) into the production `finalize_indexed_errors`
    /// helper and asserts that it produces a stable, input-ordered
    /// classification regardless of arrival order — deleting the sort inside
    /// the helper makes this fail.
    #[test]
    fn install_failures_are_sorted_by_index_for_deterministic_exit_code() {
        fn package(name: &str) -> ocx_oci::PackageRef {
            ocx_oci::PackageRef::new_registry(name, "example.com")
        }

        // Index 0 fails with NotFound (→ NotFound exit code 79); index 1 fails
        // with SelectionAmbiguous (→ DataError exit code 65). If ordering were
        // by completion, classification would flip nondeterministically.
        let kind0 = PackageErrorKind::NotFound;
        let kind1 = PackageErrorKind::SelectionAmbiguous(vec![package("pkg1")]);

        // Arrive in reverse completion order (index 1 then index 0), as a race
        // could produce.
        let indexed_errors: Vec<(usize, PackageError)> = vec![
            (1, PackageError::new(package("pkg1"), kind1)),
            (0, PackageError::new(package("pkg0"), kind0)),
        ];
        let errors = super::finalize_indexed_errors(indexed_errors);

        assert_eq!(errors[0].identifier.repository(), "pkg0", "first error must be index 0");
    }

    /// Regression for the Phase 3 (patch-discovery) parallelization: like the
    /// Phase 2 symlink loop, discovery now runs in a `JoinSet` whose
    /// `join_next` yields in nondeterministic completion order. Required-companion
    /// failures for ≥2 packages must therefore be sorted by spawn index before
    /// being wrapped in `InstallFailed`, so the exit-code classifier (which reads
    /// the first `PackageError`) is input-order-stable across runs regardless of
    /// which task completes first. This feeds discovery-flavored errors
    /// (`RequiredCompanionFailed`, which delegates classification to its source)
    /// in reverse completion order through the production `finalize_indexed_errors`
    /// helper and asserts the classification is stable across repeated folds —
    /// a completion-order-dependent implementation, or a deleted sort inside the
    /// helper, would flip this.
    #[test]
    fn discovery_failures_are_sorted_by_index_for_deterministic_exit_code() {
        fn package(name: &str) -> ocx_oci::PackageRef {
            ocx_oci::PackageRef::new_registry(name, "example.com")
        }

        fn required_companion_failed(companion: &str, source: PackageErrorKind) -> PackageErrorKind {
            PackageErrorKind::RequiredCompanionFailed {
                companion: package(companion),
                source: Box::new(source),
            }
        }

        // Index 0's required companion fails NotFound (→ NotFound exit code 79);
        // index 1's fails SelectionAmbiguous (→ DataError exit code 65). Input
        // order must decide the batch classification, not completion order.

        // Fold twice to prove the sorted selection is stable across repeated runs
        // (the same guarantee that survives nondeterministic JoinSet arrival).
        for _ in 0..2 {
            // Arrive in reverse completion order (index 1 then index 0).
            let indexed_errors: Vec<(usize, PackageError)> = vec![
                (
                    1,
                    PackageError::new(
                        package("base1"),
                        required_companion_failed(
                            "companion1",
                            PackageErrorKind::SelectionAmbiguous(vec![package("companion1")]),
                        ),
                    ),
                ),
                (
                    0,
                    PackageError::new(
                        package("base0"),
                        required_companion_failed("companion0", PackageErrorKind::NotFound),
                    ),
                ),
            ];
            let errors = super::finalize_indexed_errors(indexed_errors);

            assert_eq!(
                errors[0].identifier.repository(),
                "base0",
                "first error must be index 0"
            );
        }
    }

    /// PackageDir::entrypoints() returns sibling of content/ — verify path shape.
    #[test]
    fn package_dir_entrypoints_path_is_sibling_of_content() {
        use ocx_store::file_structure::PackageDir;
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
