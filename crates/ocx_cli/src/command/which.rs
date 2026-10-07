// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use ocx_package_manager::composer::lazy_mode_for_package;
use ocx_package_manager::error::{PackageError, PackageErrorKind};
use ocx_package_manager::{self, PackageManager};
use ocx_project::lazy::LazyMode;
use ocx_store::file_structure::FileStructure;
use ocx_util::fs::path_exists_lossy;
use tokio::task::JoinSet;

use crate::api::data::path_kind::PathKind;
use crate::{api, conventions, options};

/// Resolve one or more packages and print their package root paths.
///
/// The package root holds `content/` (installed files), `entrypoints/`
/// (generated launchers) and per-package files such as `metadata.json`.
/// `--candidate` or `--current` return the stable install symlink instead,
/// for editor configs, Makefiles or scripts; it targets the same root. Nothing
/// is downloaded. Each entry reports `package` for a materialized root, or
/// `shim` for a `--lazy-mode always` package whose content has not downloaded
/// yet; the install symlinks only ever resolve to `package`. Scripting:
/// `ocx package which --candidate --format json cmake:3.28 | jq -r '.["cmake:3.28"].path'`
#[derive(Parser)]
pub struct Which {
    #[clap(flatten)]
    platform: options::PlatformOption,

    #[clap(flatten)]
    content_path: options::ContentPath,

    #[clap(flatten)]
    lazy_mode: options::LazyMode,

    /// Package identifiers to resolve.
    #[arg(required = true, num_args = 1.., value_name = "PACKAGE")]
    packages: Vec<options::Identifier>,
}

impl Which {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let identifiers = options::Identifier::transform_all(self.packages.clone(), context.default_registry())?;

        let manager = context.manager();
        let fs = context.file_structure();

        let source = self.content_path.link_source(identifiers.len())?;
        let entries: Vec<api::data::paths::LocatedPath> = if let Some(source) = source {
            // Always `PathKind::Package`: `install` and `select`, the only link writers, never accept `--lazy-mode`.
            let infos = manager.find_symlink_all(identifiers.clone(), &source).await?;
            self.packages
                .iter()
                .zip(infos)
                .map(|(raw, info)| api::data::paths::LocatedPath {
                    package: raw.raw().to_string(),
                    path: info.dir().dir.clone(),
                    kind: PathKind::Package,
                })
                .collect()
        } else {
            let platform = conventions::platform_or_default(self.platform.platform.clone());
            // Two tiers, not five: an OCI-tier command reads no `ocx.toml`.
            let mode = lazy_mode_for_package(self.lazy_mode.mode());
            let located = locate_all(manager, fs, &identifiers, &platform, mode).await?;
            self.packages
                .iter()
                .zip(located)
                .map(|(raw, (path, kind))| api::data::paths::LocatedPath {
                    package: raw.raw().to_string(),
                    path,
                    kind,
                })
                .collect()
        };

        context.api().report(&api::data::paths::LocatedPaths::new(entries))?;

        Ok(ExitCode::SUCCESS)
    }
}

/// One located package: the directory to report, and which kind of directory it is.
type Located = (PathBuf, PathKind);

/// Which directory `ocx package which` reports for one package; `None` becomes [`PackageErrorKind::NotFound`].
fn located_directory(mode: LazyMode, package_root: Option<PathBuf>, shim_root: Option<PathBuf>) -> Option<Located> {
    // A materialized package wins under either policy: its `entrypoints/` and `bin/` shadow the shim on `PATH`.
    if let Some(root) = package_root {
        return Some((root, PathKind::Package));
    }
    match mode {
        LazyMode::Always => shim_root.map(|root| (root, PathKind::Shim)),
        LazyMode::Never => None,
    }
}

/// Locates one package in the object store, then the shim store; never materializes anything.
///
/// # Errors
///
/// [`PackageErrorKind::NotFound`] when neither form exists; anything else `find` or `resolve` raises, verbatim.
async fn locate(
    manager: &PackageManager,
    file_structure: &FileStructure,
    package: &ocx_oci::PackageRef,
    platform: ocx_oci::Platform,
    mode: LazyMode,
) -> Result<Located, PackageErrorKind> {
    let package_root = match manager.find(package, platform.clone()).await {
        Ok(info) => Some(info.dir().root().to_path_buf()),
        Err(PackageErrorKind::NotFound) => None,
        Err(kind) => return Err(kind),
    };

    // Only under `always`: probing under the default `never` spends a network round trip on a discarded value.
    let shim_root = if package_root.is_some() || mode != LazyMode::Always {
        None
    } else {
        let resolved = manager.resolve(package, platform).await?;
        let shim = file_structure.shims.shim_dir(&resolved.pinned);
        // `prepare_lazy` publishes the whole tree by one rename, so existence is completeness.
        path_exists_lossy(shim.root()).await.then(|| shim.root().to_path_buf())
    };

    located_directory(mode, package_root, shim_root).ok_or(PackageErrorKind::NotFound)
}

/// Locates every requested package concurrently, preserving request order so the exit code is deterministic.
///
/// # Errors
///
/// [`Error::FindFailed`](ocx_package_manager::error::Error::FindFailed) with one [`PackageError`] per
/// failed identifier, in request order.
async fn locate_all(
    manager: &PackageManager,
    file_structure: &FileStructure,
    packages: &[ocx_oci::PackageRef],
    platform: &ocx_oci::Platform,
    mode: LazyMode,
) -> Result<Vec<Located>, ocx_package_manager::error::Error> {
    let mut tasks: JoinSet<(usize, Result<Located, PackageErrorKind>)> = JoinSet::new();
    for (index, package) in packages.iter().enumerate() {
        let manager = manager.clone();
        let file_structure = file_structure.clone();
        let package = package.clone();
        let platform = platform.clone();
        tasks.spawn(async move {
            let result = locate(&manager, &file_structure, &package, platform, mode).await;
            (index, result)
        });
    }

    let mut slots: Vec<Option<Located>> = (0..packages.len()).map(|_| None).collect();
    let mut failures: Vec<(usize, PackageError)> = Vec::new();
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok((index, Ok(located))) => slots[index] = Some(located),
            Ok((index, Err(kind))) => failures.push((index, PackageError::new(packages[index].clone(), kind))),
            Err(join_error) => {
                tasks.abort_all();
                std::panic::resume_unwind(join_error.into_panic());
            }
        }
    }

    if !failures.is_empty() {
        failures.sort_by_key(|(index, _)| *index);
        let errors = failures.into_iter().map(|(_, error)| error).collect();
        return Err(ocx_package_manager::error::Error::FindFailed(errors));
    }

    Ok(slots
        .into_iter()
        .map(|slot| slot.expect("every slot is filled when no task failed"))
        .collect())
}

#[cfg(test)]
mod tests {
    use ocx_exit::{ClassifyExitCode as _, ExitCode};
    use ocx_package_manager::error::PackageErrorKind;

    use super::*;

    /// The object-store package root — the parent of `content/`.
    fn package_root() -> PathBuf {
        PathBuf::from("/store/packages/example")
    }

    /// The published shim tree of the same tool, deferred.
    fn shim_root() -> PathBuf {
        PathBuf::from("/store/shims/example")
    }

    // ── The four policy × state cells ─────────────────────────────────
    //
    // The scenario's expected results, in its own order:
    //   not-found 79 / real path / shim path / real path.

    /// Cell 1 — `never`, nothing on disk. Nothing to report, which the caller
    /// turns into the not-found kind (exit 79, pinned below).
    #[test]
    fn eager_policy_with_nothing_on_disk_locates_nothing() {
        assert_eq!(located_directory(LazyMode::Never, None, None), None);
    }

    /// Cell 2 — `never`, package materialized. The real package root.
    #[test]
    fn eager_policy_reports_the_package_root() {
        assert_eq!(
            located_directory(LazyMode::Never, Some(package_root()), None),
            Some((package_root(), PathKind::Package))
        );
    }

    /// Cell 3 — `always`, nothing materialized but a shim published. The shim
    /// directory, announced as one: a consumer must be able to tell a shim tree
    /// from a package root without probing disk.
    #[test]
    fn lazy_policy_reports_the_shim_directory() {
        assert_eq!(
            located_directory(LazyMode::Always, None, Some(shim_root())),
            Some((shim_root(), PathKind::Shim))
        );
    }

    /// Cell 4 — `always`, package materialized. Still the real package root:
    /// the policy says how the tool *would* compose, not what is on disk.
    #[test]
    fn lazy_policy_reports_the_package_root_once_it_is_materialized() {
        assert_eq!(
            located_directory(LazyMode::Always, Some(package_root()), None),
            Some((package_root(), PathKind::Package))
        );
    }

    // ── What makes those four cells discriminating ───────────────────────────

    /// Under `never` a published shim is **not** an answer.
    ///
    /// This is the assertion that reds on a body ignoring `mode` altogether —
    /// `package_root.or(shim_root)` satisfies all four cells above and fails
    /// only here. Without it the policy parameter is decoration.
    #[test]
    fn eager_policy_never_reports_a_shim_even_when_one_is_published() {
        assert_eq!(
            located_directory(LazyMode::Never, None, Some(shim_root())),
            None,
            "under lazy-mode never a shim tree is not what the caller asked to be pointed at"
        );
    }

    /// `always` with neither form present is still nothing: the policy *admits*
    /// a published shim as an answer, it does not invent a path to one that was
    /// never generated. `which` never materializes, so it can only report
    /// directories that already exist.
    #[test]
    fn lazy_policy_with_nothing_on_disk_locates_nothing() {
        assert_eq!(located_directory(LazyMode::Always, None, None), None);
    }

    /// Both forms present: the package root wins.
    ///
    /// Once the first invocation has materialized the tool, its `entrypoints/`
    /// and `bin/` shadow the shim on `PATH`, so reporting the shim would name
    /// a directory the caller's own environment no longer routes through.
    #[test]
    fn a_materialized_package_outranks_its_own_shim() {
        assert_eq!(
            located_directory(LazyMode::Always, Some(package_root()), Some(shim_root())),
            Some((package_root(), PathKind::Package))
        );
    }

    /// The "nothing located" the cells above produce is the kind that exits 79.
    ///
    /// Binds the cells' `not-found 79` to the classifier rather than to prose: the
    /// cells assert `None`, `locate` maps `None` onto this kind, and this pins
    /// what the kind is worth at the process boundary.
    #[test]
    fn a_miss_is_the_not_found_kind_that_exits_79() {
        assert_eq!(PackageErrorKind::NotFound.classify(), Some(ExitCode::NotFound));
        assert_eq!(ExitCode::NotFound as u8, 79);
    }
}
