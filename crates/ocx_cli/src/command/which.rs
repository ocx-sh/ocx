// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use ocx_lib::file_structure::FileStructure;
use ocx_lib::lazy::LazyMode;
use ocx_lib::oci;
use ocx_lib::package_manager::composer::lazy_mode_for_package;
use ocx_lib::package_manager::error::{PackageError, PackageErrorKind};
use ocx_lib::package_manager::{self, PackageManager};
use ocx_lib::utility::fs::path_exists_lossy;
use tokio::task::JoinSet;

use crate::api::data::path_kind::PathKind;
use crate::{api, conventions, options};

/// Resolve one or more packages and print their package root paths.
///
/// The package root is the directory containing the package's `content/` and
/// `entrypoints/` subdirectories (alongside `metadata.json`, `manifest.json`,
/// and other per-package files). Consumers traverse into `<root>/content/`
/// for installed files or `<root>/entrypoints/` for generated launchers.
///
/// By default, the content-addressed object-store package root is returned.
/// Use `--candidate` or `--current` to return the stable install symlink path
/// instead — useful when the path is embedded in editor configs, Makefiles,
/// or shell scripts that should not change on every package update. The
/// install symlinks themselves target the package root, so traversal into
/// `content/` or `entrypoints/` works identically through them.
///
/// No downloading is performed — the package must already be installed.
///
/// Every entry also reports which kind of directory it found: `package` for a
/// materialized package root, `shim` for a tool composed with `--lazy-mode
/// always` whose content has not downloaded yet. Once such a tool has been used
/// once, its content is on disk and the entry reports `package` again.
/// `--candidate` and `--current` always report `package`, because the install
/// symlinks they resolve are only ever written for materialized content.
///
/// Useful for scripting (use `--format json` for machine-readable output):
///
///   cmake_root=$(ocx package which --candidate --format json cmake:3.28 | jq -r '.["cmake:3.28"].path')
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

        let entries: Vec<api::data::paths::LocatedPath> = if let Some(kind) = self.content_path.symlink_kind() {
            // Validate the symlink resolves and the package is installed,
            // then report the symlink anchor itself (the stable per-repo
            // path that targets the package root). Consumers traverse into
            // `<anchor>/content` or `<anchor>/entrypoints` as needed.
            //
            // Always `PathKind::Package`, and `--lazy-mode` has nothing to
            // change here: `install` and `select` are the only commands that
            // write this namespace and they never accept the flag, so an anchor
            // can only ever target a materialized package root.
            let _ = manager.find_symlink_all(identifiers.clone(), kind).await?;
            self.packages
                .iter()
                .zip(identifiers.iter())
                .map(|(raw, id)| api::data::paths::LocatedPath {
                    package: raw.raw().to_string(),
                    path: fs.symlinks.symlink(id, kind),
                    kind: PathKind::Package,
                })
                .collect()
        } else {
            let platform = conventions::platform_or_default(self.platform.platform.clone());
            // Two tiers, not five: an OCI-tier command reads no `ocx.toml`, and
            // on Windows this resolves to `Never` whatever the tiers say.
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

/// One located package: the directory to report, and which kind of directory it
/// is. Named so the fan-out's `JoinSet` type stays readable.
type Located = (PathBuf, PathKind);

/// Which on-disk directory `ocx package which` reports for one package, given
/// the resolved `lazy-mode` and what the two probes found (contract C-016,
/// scenario S-007).
///
/// `None` means neither form is present, which the caller turns into
/// [`PackageErrorKind::NotFound`] — exit 79.
///
/// The whole of the policy lives here, so [`locate`] stays I/O and this stays a
/// total function over its results.
fn located_directory(mode: LazyMode, package_root: Option<PathBuf>, shim_root: Option<PathBuf>) -> Option<Located> {
    // A materialized package wins under either policy. Once its content is on
    // disk its `entrypoints/` and `bin/` shadow the shim on `PATH` (S-004), so
    // naming the shim would point at a directory the caller's own environment
    // has stopped routing through.
    if let Some(root) = package_root {
        return Some((root, PathKind::Package));
    }
    match mode {
        // Under this policy a published shim tree IS the tool's on-disk form:
        // its `bin/` is what composed onto `PATH`, and the package directory
        // does not exist yet.
        LazyMode::Always => shim_root.map(|root| (root, PathKind::Shim)),
        // Under `never` the caller asked to be pointed at materialized content.
        // A shim tree is not that, and this command materializes nothing to
        // make it so.
        LazyMode::Never => None,
    }
}

/// Locates one package: probes the object store, then — when nothing is
/// materialized — the shim store, and applies [`located_directory`].
///
/// **Never materializes** (C-016): no `find_or_install`, no `prepare_lazy`, no
/// store write of any kind. That is what makes `--lazy-mode` meaningful on a
/// command whose whole job is to report what is already there.
///
/// # Errors
///
/// - [`PackageErrorKind::NotFound`] — neither a package directory nor (under
///   [`LazyMode::Always`]) a published shim directory exists for this
///   identifier.
/// - Anything else `find` or `resolve` raises, propagated verbatim so the exit
///   code stays the one this command produced before the lazy policy existed.
async fn locate(
    manager: &PackageManager,
    file_structure: &FileStructure,
    package: &oci::Identifier,
    platform: oci::Platform,
    mode: LazyMode,
) -> Result<Located, PackageErrorKind> {
    let package_root = match manager.find(package, platform.clone()).await {
        Ok(info) => Some(info.dir().root().to_path_buf()),
        Err(PackageErrorKind::NotFound) => None,
        Err(kind) => return Err(kind),
    };

    // Only under `always`, which is the only policy `located_directory` can
    // return a shim for. `never` is the ladder's floor and therefore the
    // default, so probing there would spend a second `resolve` — a second
    // network round trip under `--remote` — on a value the next line discards.
    let shim_root = if package_root.is_some() || mode != LazyMode::Always {
        None
    } else {
        let resolved = manager.resolve(package, platform).await?;
        let shim = file_structure.shims.shim_dir(&resolved.pinned);
        // The published directory's existence is its completeness signal:
        // `prepare_lazy` stages the whole tree and publishes it by one rename,
        // so no consumer needs a second probe (C-020).
        path_exists_lossy(shim.root()).await.then(|| shim.root().to_path_buf())
    };

    located_directory(mode, package_root, shim_root).ok_or(PackageErrorKind::NotFound)
}

/// Locates every requested package concurrently, preserving request order.
///
/// Fans out one [`locate`] per identifier the way every other multi-package CLI
/// command that does per-item network work does (`package description pull`, `index
/// update`, `pull --dry-run`): an index-tagged `JoinSet`, results placed by
/// index, failures sorted by index so the surfaced error — and therefore the
/// exit code — is deterministic across runs.
///
/// # Errors
///
/// [`Error::FindFailed`](package_manager::error::Error::FindFailed) carrying one
/// [`PackageError`] per failed identifier, in request order — the same envelope
/// `find_all` produced before this command resolved packages one at a time.
async fn locate_all(
    manager: &PackageManager,
    file_structure: &FileStructure,
    packages: &[oci::Identifier],
    platform: &oci::Platform,
    mode: LazyMode,
) -> Result<Vec<Located>, package_manager::error::Error> {
    let mut tasks: JoinSet<(usize, Result<Located, PackageErrorKind>)> = JoinSet::new();
    for (index, package) in packages.iter().enumerate() {
        let manager = manager.clone();
        let file_structure = file_structure.clone();
        let package = package.clone();
        let platform = platform.clone();
        tasks.spawn(async move {
            let _spinner = manager.progress().spinner(format!("Finding '{package}'"));
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
        return Err(package_manager::error::Error::FindFailed(errors));
    }

    Ok(slots
        .into_iter()
        .map(|slot| slot.expect("every slot is filled when no task failed"))
        .collect())
}

#[cfg(test)]
mod tests {
    use ocx_lib::cli::{ClassifyExitCode as _, ExitCode};
    use ocx_lib::package_manager::error::PackageErrorKind;

    use super::*;

    /// The object-store package root — the parent of `content/`.
    fn package_root() -> PathBuf {
        PathBuf::from("/store/packages/example")
    }

    /// The published shim tree of the same tool, deferred.
    fn shim_root() -> PathBuf {
        PathBuf::from("/store/shims/example")
    }

    // ── S-007: the four policy × state cells ─────────────────────────────────
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
    /// never generated. `which` never materializes (C-016), so it can only
    /// report directories that already exist.
    #[test]
    fn lazy_policy_with_nothing_on_disk_locates_nothing() {
        assert_eq!(located_directory(LazyMode::Always, None, None), None);
    }

    /// Both forms present: the package root wins.
    ///
    /// Once the first invocation has materialized the tool, its `entrypoints/`
    /// and `bin/` shadow the shim on `PATH` (S-004), so reporting the shim would
    /// name a directory the caller's own environment no longer routes through.
    #[test]
    fn a_materialized_package_outranks_its_own_shim() {
        assert_eq!(
            located_directory(LazyMode::Always, Some(package_root()), Some(shim_root())),
            Some((package_root(), PathKind::Package))
        );
    }

    /// The "nothing located" the cells above produce is the kind that exits 79.
    ///
    /// Binds S-007's `not-found 79` to the classifier rather than to prose: the
    /// cells assert `None`, `locate` maps `None` onto this kind, and this pins
    /// what the kind is worth at the process boundary.
    #[test]
    fn a_miss_is_the_not_found_kind_that_exits_79() {
        assert_eq!(PackageErrorKind::NotFound.classify(), Some(ExitCode::NotFound));
        assert_eq!(ExitCode::NotFound as u8, 79);
    }
}
