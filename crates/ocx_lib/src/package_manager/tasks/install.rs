// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use tokio::task::JoinSet;

use crate::{
    oci,
    package::install_info::InstallInfo,
    package_manager::{
        self,
        concurrency::{self, Concurrency},
        error::PackageError,
        error::PackageErrorKind,
    },
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
        let install_info = self.pull(package, platforms.clone()).await?;

        create_install_symlinks(self, package, &install_info, candidate, select).await?;

        // Fire patch discovery after the base install and its symlinks are
        // materialized. This is the user-requested-install boundary — the only
        // site that triggers discovery. Companion installs (install_companion)
        // and transitive-dep pulls (setup_dependencies) do NOT call this.
        //
        // Discovery is a side effect of the install, so a non-required patch tier
        // whose server is empty or unreachable must not abort the base install:
        // gate the failure on the tier posture (see `install_discovery_error_is_fatal`).
        if let Err(error) = self.discover_and_install_patches(package, &platforms).await {
            if super::patch_discovery::install_discovery_error_is_fatal(self.patches(), &error) {
                return Err(error);
            }
            crate::log::warn!(
                "patch discovery for '{package}' failed (patch tier not required): {error}; \
                 continuing without companions"
            );
        }

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
    ///
    /// ## `skip_discovery` flag
    ///
    /// When `skip_discovery` is `true`, Phase 3 (patch discovery) is skipped
    /// entirely. Pass `true` for internal-engine paths — `self_update`,
    /// `bootstrap` — where discovery is nonsensical (the package being
    /// installed is `ocx` itself, not a user-requested tool). Pass `false`
    /// at the user-facing `ocx package install` boundary so companions are
    /// discovered after every user-requested install.
    pub async fn install_all(
        &self,
        packages: Vec<oci::Identifier>,
        platforms: Vec<oci::Platform>,
        candidate: bool,
        select: bool,
        concurrency: Concurrency,
        skip_discovery: bool,
    ) -> Result<Vec<InstallInfo>, package_manager::error::Error> {
        // Phase 1: Pull all packages with shared singleflight group.
        // Clone platforms so Phase 3 (patch discovery) can still borrow it
        // after pull_all() consumes the owned Vec.
        let infos = self.pull_all(&packages, platforms.clone(), concurrency).await?;

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

            // `JoinSet::join_next` yields in completion order, which is
            // nondeterministic. The exit-code classifier
            // (`package_manager::error::Error::classify`) derives the code from
            // the first `PackageError`, so an unsorted collection would make the
            // exit code depend on a race when ≥2 packages fail at once. Keep the
            // spawn index with each error and sort by it before building the
            // batch error, restoring deterministic input-order classification.
            let mut indexed_errors: Vec<(usize, PackageError)> = Vec::new();
            while let Some(join_result) = tasks.join_next().await {
                match join_result {
                    Ok((index, Err(kind))) => {
                        indexed_errors.push((index, PackageError::new(packages[index].clone(), kind)));
                    }
                    Ok((_, Ok(()))) => {}
                    Err(panic) => {
                        // A task panicked — abort remaining and propagate.
                        tasks.abort_all();
                        std::panic::resume_unwind(panic.into_panic());
                    }
                }
            }

            if !indexed_errors.is_empty() {
                let errors = finalize_indexed_errors(indexed_errors);
                return Err(package_manager::error::Error::InstallFailed(errors));
            }
        }

        // Phase 3: Patch discovery — run after all base installs and their
        // symlinks are materialized. This is the user-requested-install
        // boundary for install_all(); each package gets its own discovery
        // call so the tag-store three-state is recorded per-identifier.
        // Discovery runs in parallel via a `JoinSet` (mirrors the Phase 2
        // symlink loop): `discover_and_install_patches` takes `&self`, tag
        // writes go through cross-process-atomic `LockedJsonFile`, and
        // concurrent same-digest companion installs dedup via the pull
        // singleflight + content-addressed writes, so the tasks are safe to
        // fan out without additional locking.
        //
        // Skipped when skip_discovery=true — internal-engine paths
        // (self_update, bootstrap) pass true because looking up a patch
        // descriptor for ocx itself is nonsensical and could trigger spurious
        // required-companion errors that abort the self-update. No JoinSet is
        // built in that case.
        //
        // Required-companion failures collected with spawn index and sorted by
        // it before building the batch error, so input-order classification is
        // stable across the nondeterministic `JoinSet` completion order —
        // matching the symlink-error pattern above.
        //
        // Capped by the same `concurrency` limit as Phase 1's `pull_all` —
        // mirrors the outer-dispatch semaphore pattern in `pull.rs::pull_all`
        // so an uncapped fan-out of discovery calls (each doing index + patch
        // metadata lookups) can't outrun the pull concurrency the caller asked
        // for.
        if !skip_discovery {
            let semaphore = concurrency.semaphore();
            // Ok payload is the per-package companion-install count; install_all's
            // callers don't report it (only `ocx patch sync` does), so it's
            // discarded below — the type just has to match `discover_and_install_patches`.
            let mut tasks: JoinSet<(usize, Result<usize, PackageErrorKind>)> = JoinSet::new();

            for (index, pkg) in packages.iter().enumerate() {
                let mgr = self.clone();
                let pkg = pkg.clone();
                let platforms = platforms.clone();
                let sem = semaphore.clone();
                tasks.spawn(async move {
                    // Permit lives for the full discovery call; drop happens
                    // after the await returns, releasing the slot for the
                    // next queued discovery task.
                    let _permit = concurrency::acquire_permit(&sem).await;
                    // Gate discovery failure on the tier posture: a non-required
                    // patch tier whose server is empty/unreachable warns and
                    // continues without companions rather than failing the install.
                    let result = match mgr.discover_and_install_patches(&pkg, &platforms).await {
                        Err(error)
                            if !super::patch_discovery::install_discovery_error_is_fatal(mgr.patches(), &error) =>
                        {
                            crate::log::warn!(
                                "patch discovery for '{pkg}' failed (patch tier not required): {error}; \
                                 continuing without companions"
                            );
                            Ok(0)
                        }
                        other => other,
                    };
                    (index, result)
                });
            }

            let mut indexed_discovery_errors: Vec<(usize, PackageError)> = Vec::new();
            while let Some(join_result) = tasks.join_next().await {
                match join_result {
                    Ok((index, Err(kind))) => {
                        indexed_discovery_errors.push((index, PackageError::new(packages[index].clone(), kind)));
                    }
                    Ok((_, Ok(_))) => {}
                    Err(panic) => {
                        // A task panicked — abort remaining and propagate.
                        tasks.abort_all();
                        std::panic::resume_unwind(panic.into_panic());
                    }
                }
            }

            if !indexed_discovery_errors.is_empty() {
                let errors = finalize_indexed_errors(indexed_discovery_errors);
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

/// Sorts `(spawn_index, error)` pairs by index and unwraps them into a plain
/// `Vec<PackageError>`.
///
/// `JoinSet::join_next` yields in completion order, which is nondeterministic.
/// The exit-code classifier derives the code from the *first* `PackageError`
/// in a batch, so an unsorted collection would make the exit code depend on
/// a race whenever ≥2 packages fail concurrently. Shared by the Phase 2
/// (symlink) and Phase 3 (patch discovery) error-collection loops in
/// `install_all` — both need the identical restore-input-order step.
fn finalize_indexed_errors(mut indexed_errors: Vec<(usize, PackageError)>) -> Vec<PackageError> {
    indexed_errors.sort_by_key(|(index, _)| *index);
    indexed_errors.into_iter().map(|(_, error)| error).collect()
}

#[cfg(test)]
mod tests {
    use crate::cli::ClassifyExitCode;
    use crate::cli::ExitCode;
    use crate::oci;
    use crate::package_manager::error::{Error, PackageError, PackageErrorKind};

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
        fn package(name: &str) -> oci::Identifier {
            oci::Identifier::new_registry(name, "example.com")
        }

        // Index 0 fails with NotFound (→ NotFound exit code 79); index 1 fails
        // with SelectionAmbiguous (→ DataError exit code 65). If ordering were
        // by completion, classification would flip nondeterministically.
        let kind0 = PackageErrorKind::NotFound;
        let kind1 = PackageErrorKind::SelectionAmbiguous(vec![package("pkg1")]);
        let expected_first_code = kind0.classify();

        // Arrive in reverse completion order (index 1 then index 0), as a race
        // could produce.
        let indexed_errors: Vec<(usize, PackageError)> = vec![
            (1, PackageError::new(package("pkg1"), kind1)),
            (0, PackageError::new(package("pkg0"), kind0)),
        ];
        let errors = super::finalize_indexed_errors(indexed_errors);

        assert_eq!(errors[0].identifier.repository(), "pkg0", "first error must be index 0");
        let code = Error::InstallFailed(errors).classify();
        assert_eq!(
            code, expected_first_code,
            "exit code must derive from input-order-first failure"
        );
        assert_eq!(code, Some(ExitCode::NotFound));
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
        fn package(name: &str) -> oci::Identifier {
            oci::Identifier::new_registry(name, "example.com")
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
        let expected_first_code = PackageErrorKind::NotFound.classify();

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
            let code = Error::InstallFailed(errors).classify();
            assert_eq!(
                code, expected_first_code,
                "discovery exit code must derive from input-order-first failure"
            );
            assert_eq!(code, Some(ExitCode::NotFound));
        }
    }

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
