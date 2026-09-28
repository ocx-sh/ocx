// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Toolchain-tier `ocx inspect`: `ocx package inspect` keyed by `ocx.toml` binding, plus the
//! project's composed `[env]`.
//!
//! Needs a current `ocx.lock` (78 missing, 65 drifted): a live-resolved moving tag would make the
//! report unstable.

use std::process::ExitCode;

use clap::Parser;
use ocx_package_manager::InspectOptions;
use ocx_project::{
    DEFAULT_GROUP, Origin, ProjectConfig, SelectedTool, ToolSource, check_duplicate_selection, expand_all_keyword,
    resolve_selected_tools, select_tool_set,
};

use crate::api::data::package_inspect::{InspectReport, PackageInspect};
use crate::app::project_context::{filter_by_names, load_project_with_lock};
use crate::{conventions, options};

/// The identifier `ocx.toml` declares for `tool`; the lock records only the bare repository, which
/// would drop the declared tag. The lock fallback is unreachable under a current lock.
fn declared_identifier(config: &ProjectConfig, tool: &SelectedTool) -> ocx_oci::PackageRef {
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
/// Reports the `ocx package inspect` view per selected binding, keyed by
/// binding name, then the project `[env]`, group envs and `--env` overrides in
/// application order. Read-only. By default lists the platform candidates
/// `ocx.lock` pins; `--resolve` selects this host's leaf, and `--closure` adds
/// the transitive dependency set and `PATH` surface, reporting collisions
/// between packages before either is installed (exit 65, still in full).
/// Needs a current `ocx.lock` (78 absent, 65 stale); `ocx status` shows the
/// declaration itself.
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
        // Malformed `-g`/`--env` fails before any I/O, as in `ocx exec`.
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

        // Filter before duplicate validation: a disagreeing binding fails only when it is in the narrowed set.
        let selected = select_tool_set(&ctx.config, Some(&ctx.lock), &expanded, &[])?;
        let filtered = filter_by_names(selected, &self.names)?;
        check_duplicate_selection(&filtered)?;

        let declared: Vec<ocx_oci::PackageRef> = filtered
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

        // In application order, as declared, not merged: package values are `${installPath}`-templated,
        // so the final value is `ocx env`'s to answer.
        let mut env = ocx_project::project_env_entries(&ctx.config, &ctx.config_path, &expanded);
        env.extend(env_overrides);

        let report = InspectReport::new(
            selects_platform.then_some(&platform),
            packages,
            conventions::env_entries(&env),
        );
        context.api().report(&report)?;

        Ok(conventions::inspect_exit_code(&report))
    }

    /// The `--resolve`/`--closure` path: each binding's declared identifier carrying the resolved leaf
    /// digest, so the report pins `registry/repo:tag@digest` as `ocx package inspect` does.
    async fn resolved_packages(
        &self,
        context: &crate::app::Context,
        selected: &[SelectedTool],
        declared: &[ocx_oci::PackageRef],
        platform: &ocx_oci::Platform,
    ) -> anyhow::Result<Vec<PackageInspect>> {
        let resolved = resolve_selected_tools(selected, platform)?;
        let identifiers: Vec<ocx_oci::PackageRef> = resolved
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

        // `inspect_all` preserves input order, so zipping back is sound. `identifier` is the declaration
        // without the digest, as `ocx package inspect` reports it.
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

/// The default path: each binding straight from `ocx.lock`, offline, choosing no platform.
fn locked_packages(selected: &[SelectedTool], declared: &[ocx_oci::PackageRef]) -> Vec<PackageInspect> {
    selected
        .iter()
        .zip(declared)
        .map(|(tool, declared)| match &tool.source {
            ToolSource::Locked(locked) => {
                PackageInspect::locked(tool.binding.clone(), declared.clone(), &locked.platforms)
            }
            // Unreachable: `inspect` passes no positionals, so every selection is lock-backed.
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
