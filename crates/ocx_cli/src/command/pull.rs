// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_project::{expand_all_keyword, lazy_mode_for_tool};

use crate::api;
use crate::app::project_context::load_project_with_lock_consenting;
use crate::conventions;
use crate::options;

/// Pre-warm the object store from the project `ocx.lock` without creating symlinks.
///
/// Pulls every digest-pinned lock entry across the requested groups of the
/// nearest `ocx.toml` into the local object store. Unlike `ocx package pull`
/// it is project-tier (driven by the lock file) and never touches the
/// candidate or current symlink namespace. A successful pull re-saves
/// `ocx.lock` byte-identically so direnv's `watch_file` fires, except under
/// `--dry-run`.
#[derive(Parser, Clone)]
pub struct Pull {
    /// Preview which locked packages are cached vs. would be fetched.
    ///
    /// Walks `ocx.lock`, resolves each entry through the local index
    /// (cache-first), and checks the store for the resolved digest.
    /// No store writes; the only network surface is
    /// the cache-miss path of resolve, which lock has typically already
    /// populated. Combine with `--offline` to forbid any network probe.
    /// Honors `--format json` and `--quiet`. The staleness gate still
    /// fires: a stale lock exits 65 before the dry-run preview prints.
    #[arg(long = "dry-run")]
    pub dry_run: bool,

    /// Restrict the pull to the named group(s).
    ///
    /// Repeatable and comma-separated: `-g ci,lint -g release`. The
    /// reserved name `default` selects the top-level `[tools]` table; the
    /// reserved name `all` expands to `default` + every declared `[group.*]`.
    /// When omitted, every `[tools]` and `[group.*]` entry from the lock
    /// is pulled.
    #[arg(short = 'g', long = "group", value_delimiter = ',')]
    pub groups: Vec<String>,

    #[clap(flatten)]
    pub platform: options::PlatformOption,

    /// Under `always`, a package gets its metadata, closure config blobs and launchers but no
    /// content, which downloads when one of those launchers first runs.
    #[clap(flatten)]
    pub lazy_mode: options::LazyMode,

    #[clap(flatten)]
    pub consent: options::Consent,
}

impl Pull {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        crate::app::project_context::ensure_group_segments_nonempty(&self.groups)?;

        // The consenting load, unlike read-only callers: build tooling runs `pull` in checkouts no
        // operator chose, so `OCX_NO_CONSENT` must be able to suppress the stamp.
        let ctx = load_project_with_lock_consenting(&context, self.consent.explicit()).await?;

        crate::app::project_context::ensure_groups_known(&self.groups, &ctx.config)?;

        let platform = conventions::platform_or_default(self.platform.platform.clone());
        let render_groups = render_groups(&self.groups, &ctx);
        let selected: Vec<&ocx_project::LockedTool> = if self.groups.is_empty() {
            ctx.lock.tools.iter().collect()
        } else {
            let expanded = expand_all_keyword(&self.groups, &ctx.config);
            ctx.lock
                .tools
                .iter()
                .filter(|t| expanded.iter().any(|g| g == &t.group))
                .collect()
        };
        let mut pinned: Vec<ocx_oci::PinnedPackageRef> = Vec::new();
        for tool in &selected {
            let id = host_pull_pinned(tool, &platform)?;
            // ponytail: O(n) dedup over tools — tiny.
            if !pinned.contains(&id) {
                pinned.push(id);
            }
        }

        // After the staleness gate, so a stale lock exits 65 before any preview prints.
        if self.dry_run {
            render_and_warn(&context, &ctx, &render_groups, &platform, true).await;
            return run_dry_run(&context, &pinned, platform).await;
        }

        let identifiers: Vec<ocx_oci::PackageRef> = pinned.iter().cloned().map(Into::into).collect();

        let mut modes: Vec<ocx_project::lazy::LazyMode> = Vec::with_capacity(identifiers.len());
        for (tool, identifier) in selected.iter().zip(identifiers.iter()) {
            modes.push(lazy_mode_for_tool(
                &ctx.config,
                identifier,
                Some(tool.group.as_str()),
                self.lazy_mode.mode(),
            ));
        }
        let eager: Vec<ocx_oci::PackageRef> = identifiers
            .iter()
            .zip(modes.iter())
            .filter(|(_, mode)| **mode == ocx_project::lazy::LazyMode::Never)
            .map(|(identifier, _)| identifier.clone())
            .collect();
        let info = context
            .manager()
            .pull_all(&eager, platform.clone(), context.concurrency())
            .await?;
        // Keyed, not positional: nothing guarantees `pull_all` returns exactly one entry per input.
        let eager_paths: std::collections::HashMap<&ocx_oci::PackageRef, &_> = eager.iter().zip(info.iter()).collect();

        let mut warmed: Vec<api::data::warmed_paths::WarmedPath> = Vec::with_capacity(identifiers.len());
        let mut advisories: Vec<ocx_package_manager::LazyAdvisory> = Vec::new();
        for (identifier, mode) in identifiers.iter().zip(modes.iter()) {
            let entry = if *mode == ocx_project::lazy::LazyMode::Never {
                // A missing result is a `pull_all` bug; skipped rather than reported with a fabricated path.
                let Some(info) = eager_paths.get(identifier) else {
                    continue;
                };
                api::data::warmed_paths::WarmedPath {
                    package: identifier.to_string(),
                    path: info.dir().root().to_path_buf(),
                    kind: api::data::path_kind::PathKind::Package,
                }
            } else {
                let prepared = context
                    .manager()
                    .prepare_lazy(identifier, platform.clone())
                    .await
                    .map_err(|kind| {
                        ocx_package_manager::error::Error::ResolveFailed(vec![
                            ocx_package_manager::error::PackageError::new(identifier.clone(), kind),
                        ])
                    })?;
                // Both channels, as `ocx env` does: stderr for humans, `--format json` for tooling.
                for advisory in &prepared.advisories {
                    context.ui().warn(advisory.to_string());
                }
                advisories.extend(prepared.advisories);
                // The shim directory: this run did not create the package directory.
                api::data::warmed_paths::WarmedPath {
                    package: identifier.to_string(),
                    path: prepared.shim.root().to_path_buf(),
                    kind: api::data::path_kind::PathKind::Shim,
                }
            };
            warmed.push(entry);
        }

        // Re-saved byte-identical to advance the mtime so direnv re-fires; never under `--dry-run`.
        ctx.lock
            .save(
                &ctx.lock_path,
                Some(&ctx.lock),
                context.file_structure().root(),
                &ctx.config_path,
            )
            .await?;

        // After the lock re-save, so the render stamp is the last thing written.
        render_and_warn(&context, &ctx, &render_groups, &platform, false).await;

        let paths = api::data::warmed_paths::WarmedPaths::new(warmed)
            .with_advisories(api::data::env::LazyAdvisoryReport::from_advisories(&advisories));
        context.api().report(&paths)?;

        Ok(ExitCode::SUCCESS)
    }
}

/// Resolves a locked tool to its host-platform pinned pull identifier.
///
/// # Errors
///
/// `ProjectErrorKind::NoHostLeaf` (exit 78) when the lock has no leaf for `host`.
fn host_pull_pinned(
    tool: &ocx_project::LockedTool,
    host: &ocx_oci::Platform,
) -> anyhow::Result<ocx_oci::PinnedPackageRef> {
    let id = ocx_project::host_leaf_identifier(tool, host).map_err(anyhow::Error::from)?;
    ocx_oci::PinnedPackageRef::try_from(id).map_err(|e| {
        anyhow::anyhow!(
            "locked leaf for binding '{}' is not a valid pinned identifier: {e}",
            tool.name
        )
    })
}

/// The groups this render covers: `-g` expanded, else every lock group plus the default group
/// unconditionally, or `bin/` stops reconciling once a hand edit drops the last default-group tool.
/// A `-g` without the default group leaves `bin/` untouched, since reconciling it needs the network.
fn render_groups(selected: &[String], project: &crate::app::project_context::ProjectContext) -> Vec<String> {
    if !selected.is_empty() {
        return expand_all_keyword(selected, &project.config);
    }
    let mut groups = vec![ocx_project::DEFAULT_GROUP.to_owned()];
    for tool in &project.lock.tools {
        if !groups.iter().any(|group| group == &tool.group) {
            groups.push(tool.group.clone());
        }
    }
    groups
}

/// Renders the toolchain home and warns on stderr about what the render could not do.
///
/// Never the exit code, or a read-only checkout or an unwalkable offline closure fails a pull that
/// already landed; stderr, since `--quiet` is a contract about the payload, not a warning.
async fn render_and_warn(
    context: &crate::app::Context,
    project: &crate::app::project_context::ProjectContext,
    groups: &[String],
    platform: &ocx_oci::Platform,
    dry_run: bool,
) {
    let rendered = async {
        let scope = context.toolchain_render_scope(&project.config_path).await?;
        let render = ocx_package_manager::ToolchainRender {
            scope: &scope,
            toolchain_root: context.toolchain_root(),
            platform,
        };
        let pinned = ocx_package_manager::pinned_for_project(None, &project.config);
        anyhow::Ok(
            context
                .manager()
                .render_home(&project.lock, pinned, &render, groups, dry_run)
                .await?,
        )
    }
    .await;

    match rendered {
        Ok(report) => {
            for line in ocx_package_manager::skipped_render_warnings(&report) {
                context.ui().warn(line);
            }
        }
        // Debug-only under `--offline`, where an unwarmed store failing to resolve is the ordinary state.
        Err(error) if context.manager().is_offline() => {
            log::debug!("The toolchain home was not rendered: {error:#}");
        }
        Err(error) => context
            .ui()
            .warn(format!("The toolchain home was not rendered: {error:#}")),
    }
}

/// One dry-run probe: cached / would-fetch, plus the store path when present.
type DryRunProbe = (api::data::pull_dry_run::PullStatus, Option<std::path::PathBuf>);

/// Reports cached / would-fetch per pinned id without writing the store; a resolution failure
/// reads as would-fetch. Resolves before `find_plain`, since the lock pins the image index and the
/// store keys by platform manifest, so a direct probe misses every multi-platform package.
async fn run_dry_run(
    context: &crate::app::Context,
    pinned: &[ocx_oci::PinnedPackageRef],
    platform: ocx_oci::Platform,
) -> anyhow::Result<ExitCode> {
    use api::data::pull_dry_run::{DryRunEntry, PullDryRun, PullStatus};

    // Fanned out: `resolve` hits the network on a cold cache, so a sequential loop chains n round trips.
    let mut join_set: tokio::task::JoinSet<(usize, anyhow::Result<DryRunProbe>)> = tokio::task::JoinSet::new();
    for (index, id) in pinned.iter().enumerate() {
        let manager = context.manager().clone();
        let identifier: ocx_oci::PackageRef = id.clone().into();
        let platform = platform.clone();
        join_set.spawn(async move {
            let result = async {
                let resolved = match manager.resolve(&identifier, platform).await {
                    Ok(chain) => Some(chain.pinned),
                    Err(_) => None,
                };
                match resolved {
                    Some(pinned) => match manager.find_plain(&pinned).await? {
                        Some(info) => Ok((PullStatus::Cached, Some(info.dir().root().to_path_buf()))),
                        None => Ok((PullStatus::WouldFetch, None)),
                    },
                    None => Ok((PullStatus::WouldFetch, None)),
                }
            }
            .await;
            (index, result)
        });
    }

    // Failures keep their input index, so the surfaced error and exit code are deterministic.
    let mut slots: Vec<Option<DryRunProbe>> = (0..pinned.len()).map(|_| None).collect();
    let mut failures: Vec<(usize, anyhow::Error)> = Vec::new();
    while let Some(joined) = join_set.join_next().await {
        match joined {
            Ok((index, Ok(value))) => slots[index] = Some(value),
            Ok((index, Err(e))) => failures.push((index, e)),
            Err(join_err) => {
                join_set.abort_all();
                std::panic::resume_unwind(join_err.into_panic());
            }
        }
    }
    if !failures.is_empty() {
        failures.sort_by_key(|(index, _)| *index);
        let (_, error) = failures.into_iter().next().expect("failures is non-empty");
        return Err(error);
    }

    let entries: Vec<DryRunEntry> = pinned
        .iter()
        .zip(slots)
        .map(|(id, slot)| {
            let (status, path) = slot.expect("all slots filled on success");
            DryRunEntry::new(id.clone(), status, path)
        })
        .collect();
    let report = PullDryRun::new(entries);
    context.api().report(&report)?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// `--platform` accepts a single value and coexists with `-g`.
    #[test]
    fn parses_platform_flag() {
        let pull = Pull::try_parse_from(["pull", "-g", "ci", "--platform", "linux/arm64"]).unwrap();
        assert_eq!(
            pull.platform.platform.map(|p| p.to_string()),
            Some("linux/arm64".to_owned())
        );
        assert_eq!(
            pull.groups,
            vec!["ci".to_owned()],
            "-g must still parse alongside --platform"
        );
    }

    /// A second `--platform` occurrence is a usage error
    /// (`adr_platform_model_unification.md`).
    #[test]
    fn rejects_repeated_platform_flag() {
        assert!(
            Pull::try_parse_from(["pull", "--platform", "linux/arm64", "-p", "linux/amd64"]).is_err(),
            "repeated --platform must be rejected"
        );
    }

    /// `-g all` parses (the `all` keyword is resolved at execute time).
    #[test]
    fn parses_all_group_keyword() {
        let pull = Pull::try_parse_from(["pull", "-g", "all"]).unwrap();
        assert_eq!(pull.groups, vec!["all".to_owned()]);
    }

    // ── `--dry-run` stays `ocx pull`-only ─────────────────────────────────

    /// The four mutation commands route through the shared
    /// `commit_and_render`, but none of them grows a `--dry-run` implicitly
    /// by coming through it: a dry run of a *mutation* would have to decide
    /// what "wrote nothing" means for `ocx.toml` and `ocx.lock` as well as
    /// for the tree, and no contract says.
    const MUTATION_COMMANDS: [(&[&str], &[&str]); 4] = [
        (&["add"], &["example.com/cmake:1.0.0"]),
        (&["remove"], &["cmake"]),
        (&["lock"], &[]),
        (&["update"], &[]),
    ];

    /// `--dry-run` is `ocx pull`'s alone.
    ///
    /// `UnknownArgument`, not merely "an error": that is what separates "there
    /// is no such flag here" from "this invocation was rejected for some other
    /// reason". The positive control on `ocx pull` is what stops the whole case
    /// passing because the flag was renamed out from under it.
    ///
    /// Reachable red by construction: flattening a `--dry-run` onto the shared
    /// mutation path — the tempting reading of "the four commands call that one
    /// function and nothing else" — turns each of these into a parse success.
    #[test]
    fn only_pull_declares_dry_run() {
        // Positive control first.
        let pull = Pull::try_parse_from(["pull", "--dry-run"]).expect("`ocx pull --dry-run` must parse");
        assert!(pull.dry_run, "the control must actually set the flag");

        for (path, operands) in MUTATION_COMMANDS {
            let base: Vec<String> = std::iter::once("ocx")
                .chain(path.iter().copied())
                .chain(operands.iter().copied())
                .map(str::to_string)
                .collect();
            crate::app::Cli::try_parse_from(&base)
                .unwrap_or_else(|error| panic!("`ocx {}` base invocation must parse: {error}", path.join(" ")));

            let with_flag: Vec<String> = std::iter::once("ocx")
                .chain(path.iter().copied())
                .chain(std::iter::once("--dry-run"))
                .chain(operands.iter().copied())
                .map(str::to_string)
                .collect();
            let error = crate::app::Cli::try_parse_from(&with_flag)
                .err()
                .unwrap_or_else(|| panic!("RUL-54 — `ocx {} --dry-run` must not parse", path.join(" ")));
            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::UnknownArgument,
                "RUL-54 — `ocx {}` must reject --dry-run as an unknown argument",
                path.join(" ")
            );
        }
    }
}
