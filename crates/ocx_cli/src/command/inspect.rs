// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Toolchain-tier `ocx inspect` command.
//!
//! The project-tier counterpart to `ocx package inspect`: same report, same
//! flags, same per-package shapes — keyed by `ocx.toml` binding instead of raw
//! identifier, and carrying the project's composed `[env]` alongside.
//!
//! Read-only. Nothing is installed, no symlink is created, neither project file
//! is written. `--offline` is honored; a cold `--closure` costs one config-blob
//! fetch per selected tool.
//!
//! # Why this needs a current lock
//!
//! `ocx status` describes the declaration and reports a missing or drifted lock
//! as payload. `inspect` describes what *would happen*, and without a pin there
//! is no stable answer — re-resolving declared tags live would make the report
//! depend on where a moving tag points this second. So the staleness gate
//! applies here (78 / 65) and `ocx lock` is the remedy.
//!
//! # Why default mode resolves nothing
//!
//! `--resolve` is what selects a platform, here exactly as in
//! `ocx package inspect`. Default mode lists each binding's *candidates* —
//! and `ocx.lock` already holds them, one leaf digest per platform. So the
//! default report is a pure projection of the two project files: no registry
//! read, no host-leaf selection, and `-p` inert, matching the OCI-tier command
//! flag for flag.

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{
    oci,
    package_manager::InspectOptions,
    project::{
        DEFAULT_GROUP, Origin, ProjectConfig, SelectedTool, ToolSource, check_duplicate_selection, expand_all_keyword,
        resolve_selected_tools, select_tool_set,
    },
};

use crate::api::data::package_inspect::{InspectReport, PackageInspect};
use crate::app::project_context::{filter_by_names, load_project_with_lock};
use crate::{conventions, options};

/// The identifier `ocx.toml` declares for `tool`.
///
/// The lock records the bare `registry/repository` shared by every platform
/// leaf, so a report built from lock entries alone would drop the declared tag
/// — the one part of the reference a reader recognizes. The declaration is the
/// authority for it.
///
/// Falls back to the lock's bare repository if the config has no such binding.
/// A current lock (which this command requires) rules that out, so the arm is
/// a total-match tail rather than a real state.
fn declared_identifier(config: &ProjectConfig, tool: &SelectedTool) -> oci::Identifier {
    let declared = match &tool.origin {
        Origin::Group(group) if group == DEFAULT_GROUP => config.tools.get(&tool.binding),
        Origin::Group(group) => config
            .groups
            .get(group)
            .and_then(|group| group.tools.get(&tool.binding)),
        Origin::Explicit => None,
    };
    match (declared, &tool.source) {
        (Some(identifier), _) => identifier.clone(),
        (None, ToolSource::Locked(locked)) => locked.repository.clone(),
        (None, ToolSource::Explicit(identifier)) => identifier.clone(),
    }
}

/// Inspect what the project toolchain resolves to, without installing.
///
/// Selects the bindings in the requested groups, narrows them to any NAMEs
/// given, and reports the same per-package view `ocx package inspect` emits -
/// keyed by binding name. The project's `[env]`, the selected groups'
/// `[group.*.env]` and any `--env` overrides follow in application order.
///
/// Read-only: nothing is installed and no symlink is created.
///
/// By default each binding lists the platform candidates `ocx.lock` pins for
/// it, which needs no registry at all. Use `--resolve` to select this host's
/// leaf and emit its metadata and OCI resolution chain, and `--closure` for the
/// transitive dependency set plus the binaries, entrypoints and env keys that
/// would land on `PATH`, and each side's declared integration namespaces.
/// Because `--closure` sees the whole selection at once, it also reports
/// collisions between two different tools before either is installed.
///
/// Needs a current `ocx.lock` (exit 78 when absent, 65 when it no longer
/// matches `ocx.toml`) - without a pin there is no stable answer. For the
/// declaration itself, including those two states, use `ocx status`.
///
/// Exits 65 when `--closure` finds a conflict that would make the surface
/// unrealizable; the conflict is still reported in full.
#[derive(Parser)]
pub struct Inspect {
    #[clap(flatten)]
    groups: options::GroupSelection,

    #[clap(flatten)]
    platform: options::PlatformOption,

    #[clap(flatten)]
    env: options::EnvOverride,

    /// Select this host's leaf and emit its metadata plus the OCI resolution
    /// chain.
    ///
    /// Without it, a binding lists the platform candidates the lock pins for it
    /// and nothing is selected. The lock already pins a platform manifest, so
    /// the chain starts at that manifest and has no `index` entry.
    #[clap(long)]
    resolve: bool,

    /// Compute each binding's dependency closure from metadata alone, without
    /// installing.
    ///
    /// Adds a `closure` object per binding holding its transitive dependencies
    /// and the `interface` / `private` surface projections - the binaries,
    /// entrypoints and env keys that would reach a consumer versus stay
    /// internal, plus each side's declared integration namespaces. Reading
    /// declared dependencies means selecting a leaf, so this implies the same
    /// platform selection `--resolve` performs.
    #[clap(long)]
    closure: bool,

    /// Binding names to inspect; defaults to every binding in the selected
    /// groups.
    ///
    /// Each name is an `ocx.toml` binding key. Only the named bindings are
    /// reported, so under `--resolve` an unrelated sibling that ships no leaf
    /// for this host does not block the report. An unknown name exits 64.
    #[arg(num_args = 0..)]
    names: Vec<String>,
}

impl Inspect {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // Reject malformed `-g` / `--env` before any filesystem or network
        // work, matching `ocx run`.
        crate::app::project_context::ensure_group_segments_nonempty(self.groups.names())?;
        let cwd = std::env::current_dir()
            .map_err(|error| anyhow::Error::from(error).context("failed to read the current directory"))?;
        let env_overrides = self.env.entries(&cwd)?;

        let ctx = load_project_with_lock(&context).await?;
        crate::app::project_context::ensure_groups_known(self.groups.names(), &ctx.config)?;

        let mut expanded = expand_all_keyword(self.groups.names(), &ctx.config);
        if expanded.is_empty() {
            expanded = vec![DEFAULT_GROUP.to_owned()];
        }

        // Selection → NAME filter → duplicate validation, in that order: a
        // binding two selected groups disagree about only fails the command
        // when it is actually in the narrowed set.
        let selected = select_tool_set(&ctx.config, Some(&ctx.lock), &expanded, &[])?;
        let filtered = filter_by_names(selected, &self.names)?;
        check_duplicate_selection(&filtered)?;

        let declared: Vec<oci::Identifier> = filtered
            .iter()
            .map(|tool| declared_identifier(&ctx.config, tool))
            .collect();

        // `-p` selects a platform only where a platform is selected at all.
        let selects_platform = self.resolve || self.closure;
        let platform = conventions::platform_or_default(self.platform.platform.clone());

        let packages = if selects_platform {
            self.resolved_packages(&context, &filtered, &declared, &platform)
                .await?
        } else {
            locked_packages(&filtered, &declared)
        };

        // Application order: `[env]`, then each selected group's env in `-g`
        // order, then `--env` last. Entries are kept as declared rather than
        // merged — `ocx env` is what answers "what is the final value", and it
        // materializes to do so because package values are `${installPath}`-
        // templated.
        let mut env = ocx_lib::project::project_env_entries(&ctx.config, &ctx.config_path, &expanded);
        env.extend(env_overrides);

        let report = InspectReport::new(
            selects_platform.then_some(&platform),
            packages,
            conventions::env_entries(&env),
        );
        context.api().report(&report)?;

        Ok(conventions::inspect_exit_code(&report))
    }

    /// Resolves each selected binding to its host leaf and inspects it —
    /// the `--resolve` / `--closure` path.
    ///
    /// The identifier handed to the manager is the *declared* one carrying the
    /// resolved leaf digest, so the report pins `registry/repo:tag@digest`
    /// exactly as `ocx package inspect` does for the same package. The digest
    /// is what the fetch keys on, so attaching the tag changes nothing but the
    /// report.
    async fn resolved_packages(
        &self,
        context: &crate::app::Context,
        selected: &[SelectedTool],
        declared: &[oci::Identifier],
        platform: &oci::Platform,
    ) -> anyhow::Result<Vec<PackageInspect>> {
        let resolved = resolve_selected_tools(selected, platform)?;
        let identifiers: Vec<oci::Identifier> = resolved
            .iter()
            .zip(declared)
            .map(|(tool, declared)| match tool.identifier.digest() {
                Some(digest) => declared.clone_with_digest(digest),
                None => tool.identifier.clone(),
            })
            .collect();

        let options = InspectOptions {
            resolve: self.resolve,
            closure: self.closure,
        };
        let results = context
            .manager()
            .inspect_all(identifiers, platform.clone(), options)
            .await?;

        // `inspect_all` preserves input order, so zipping back onto `resolved`
        // (and therefore onto the binding names) is sound. The reported
        // `identifier` is the declaration WITHOUT the digest — `ocx package
        // inspect` reports the requested reference there and lets
        // `pinned_identifier` carry the resolved digest.
        Ok(resolved
            .iter()
            .zip(declared)
            .zip(results)
            .map(|((tool, declared), result)| {
                PackageInspect::new(tool.binding.clone(), declared.clone(), platform.clone(), result)
            })
            .collect())
    }
}

/// Projects each selected binding straight from `ocx.lock` — the default path.
///
/// Offline by construction: the lock's platform-to-leaf map *is* the candidate
/// list, so nothing is fetched and no platform is chosen.
fn locked_packages(selected: &[SelectedTool], declared: &[oci::Identifier]) -> Vec<PackageInspect> {
    selected
        .iter()
        .zip(declared)
        .map(|(tool, declared)| match &tool.source {
            ToolSource::Locked(locked) => {
                PackageInspect::locked(tool.binding.clone(), declared.clone(), &locked.platforms)
            }
            // Unreachable in practice: `inspect` passes no positionals, so
            // every selection is lock-backed. An explicit identifier has no
            // lock entry, hence no candidates to project.
            ToolSource::Explicit(_) => PackageInspect::locked(
                tool.binding.clone(),
                declared.clone(),
                &std::collections::BTreeMap::new(),
            ),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_zero_or_more_names() {
        assert!(Inspect::try_parse_from(["inspect"]).is_ok());
        let scoped = Inspect::try_parse_from(["inspect", "-g", "ci", "go-task", "uv"]).expect("parses");
        assert_eq!(scoped.names, ["go-task", "uv"]);
        assert_eq!(scoped.groups.names(), ["ci"]);
    }

    /// `--resolve` and `--closure` are independent switches here, unlike the
    /// `-p`-gated pair on `ocx package inspect`: the lock pins the platform, so
    /// neither flag has to imply a selection step.
    #[test]
    fn resolve_and_closure_compose() {
        let both = Inspect::try_parse_from(["inspect", "--resolve", "--closure"]).expect("parses");
        assert!(both.resolve && both.closure);
    }
}
