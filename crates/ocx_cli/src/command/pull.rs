// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::cli;
use ocx_lib::oci;
use ocx_lib::project::DEFAULT_GROUP;

use crate::api;
use crate::app::project_context::{ProjectContextError, load_project_with_lock};
use crate::conventions::platforms_or_default;

/// Pre-warm the object store from the project `ocx.lock` without creating symlinks.
///
/// Loads the nearest `ocx.toml` together with its sibling `ocx.lock`, collects
/// every digest-pinned tool entry across the requested groups, and pulls each
/// one into the local object store. Distinct from `ocx package pull`: this
/// command is project-tier (driven by the lock file) and never touches the
/// candidate or current symlink namespace.
#[derive(Parser, Clone)]
pub struct Pull {
    /// Preview which locked tools are cached vs. would be fetched.
    ///
    /// Walks `ocx.lock`, resolves each entry through the local index
    /// (cache-first, like `pull_all` does), and probes `find_plain` on
    /// the resolved digest. No store writes; the only network surface is
    /// the cache-miss path of resolve, which lock has typically already
    /// populated. Combine with `--offline` to forbid any network probe.
    /// Honors `--format json` and `--quiet`. The staleness gate still
    /// fires: a stale lock exits 65 before the dry-run preview prints.
    #[arg(long = "dry-run")]
    pub dry_run: bool,

    /// Restrict the pull to the named group(s).
    ///
    /// Repeatable and comma-separated: `-g ci,lint -g release`. The
    /// reserved name `default` selects the top-level `[tools]` table.
    /// When omitted, every `[tools]` and `[group.*]` entry from the lock
    /// is pulled.
    #[arg(short = 'g', long = "group", value_delimiter = ',')]
    pub groups: Vec<String>,
}

impl Pull {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // ── Phase 1: parse-time validation ───────────────────────────────

        // Reject empty comma segments (`-g ci,,lint`) BEFORE any filesystem
        // or network work. `clap`'s `value_delimiter = ','` splits the value
        // into `["ci", "", "lint"]`; an empty string is a user-typing error.
        for raw in &self.groups {
            if raw.is_empty() {
                eprintln!("empty group segment in --group value; check for stray commas");
                return Ok(cli::ExitCode::UsageError.into());
            }
        }

        // ── Phase 2–3: project resolution, lock load, staleness gate ─────

        let ctx = match load_project_with_lock(&context).await {
            Ok(ctx) => ctx,
            Err(ProjectContextError::NoProject { cwd }) => {
                eprintln!(
                    "no ocx.toml found in {} or any parent; create one, or run from a directory that contains it",
                    cwd.display()
                );
                return Ok(cli::ExitCode::UsageError.into());
            }
            Err(ProjectContextError::LockMissing { path }) => {
                eprintln!("ocx.lock not found at {}; run `ocx lock` to create it", path.display());
                return Ok(cli::ExitCode::ConfigError.into());
            }
            Err(ProjectContextError::StaleLock { .. }) => {
                eprintln!("ocx.lock is stale (ocx.toml changed since last `ocx lock`); run `ocx lock`");
                return Ok(cli::ExitCode::DataError.into());
            }
            Err(other) => return Err(other.into()),
        };

        // Validate requested groups against the loaded config. Unknown
        // groups produce exit 64. `default` is always valid since it
        // names the top-level `[tools]` table.
        for raw in &self.groups {
            if raw == DEFAULT_GROUP {
                continue;
            }
            if !ctx.config.groups.contains_key(raw) {
                eprintln!("unknown group '{raw}' in --group filter");
                return Ok(cli::ExitCode::UsageError.into());
            }
        }

        // ── Phase 4: select tool set from the lock ───────────────────────

        // No positional packages and no compose step: the lock is already
        // authoritative. Walk it directly, preserving lock order (sorted
        // by `(group, name)` at write time, so the result is deterministic).
        let pinned: Vec<oci::PinnedIdentifier> = if self.groups.is_empty() {
            ctx.lock.tools.iter().map(|t| t.pinned.clone()).collect()
        } else {
            ctx.lock
                .tools
                .iter()
                .filter(|t| self.groups.iter().any(|g| g == &t.group))
                .map(|t| t.pinned.clone())
                .collect()
        };

        // ── Phase 4b: dry-run preview ────────────────────────────────────

        // Dry-run runs after the staleness gate so a stale lock still
        // exits 65 before any preview prints. No network, no store writes.
        if self.dry_run {
            return run_dry_run(&context, &pinned).await;
        }

        let identifiers: Vec<oci::Identifier> = pinned.iter().cloned().map(Into::into).collect();

        // ── Phase 5: pull + report ───────────────────────────────────────

        // `pull_all` short-circuits on an empty slice (returns `Ok(vec![])`),
        // so an unmatched group filter or an empty lock both exit 0 with
        // an empty report — there is nothing to pre-warm, that is not a
        // failure.
        let info = context
            .manager()
            .pull_all(&identifiers, platforms_or_default(&[]), context.concurrency())
            .await?;

        let entries: Vec<api::data::paths::PathEntry> = identifiers
            .iter()
            .zip(info.iter())
            .map(|(id, info)| api::data::paths::PathEntry {
                package: id.to_string(),
                path: info.dir().content(),
            })
            .collect();
        let paths = api::data::paths::Paths::new(entries);
        context.api().report(&paths)?;

        Ok(ExitCode::SUCCESS)
    }
}

/// Walks `pinned` and reports cached / would-fetch status without
/// modifying the store. Resolution mirrors `pull_all`'s first steps:
/// resolve each identifier through the index chain (local-first, with
/// the configured remote behind it), then probe `find_plain` on the
/// resolved digest. The lock holds the image-index digest; the store
/// keys by platform-manifest digest, so a direct `find_plain(lock.pinned)`
/// would miss every multi-platform package — descending through `resolve`
/// keeps dry-run aligned with what the real pull would short-circuit on.
///
/// Resolution failures (network errors when the local index is cold)
/// surface as `would-fetch` rather than aborting the preview, so a stale
/// or partial cache still produces a useful report.
async fn run_dry_run(context: &crate::app::Context, pinned: &[oci::PinnedIdentifier]) -> anyhow::Result<ExitCode> {
    let manager = context.manager();
    let platforms = platforms_or_default(&[]);
    let mut entries = Vec::with_capacity(pinned.len());
    for id in pinned {
        let display = id.to_string();
        let identifier: oci::Identifier = id.clone().into();
        let resolved = match manager.resolve(&identifier, platforms.clone()).await {
            Ok(chain) => Some(chain.pinned),
            Err(_) => None,
        };
        let (status, path) = match resolved {
            Some(pinned) => match manager.find_plain(&pinned).await? {
                Some(info) => (api::data::pull_dry_run::PullStatus::Cached, Some(info.dir().content())),
                None => (api::data::pull_dry_run::PullStatus::WouldFetch, None),
            },
            None => (api::data::pull_dry_run::PullStatus::WouldFetch, None),
        };
        entries.push(api::data::pull_dry_run::DryRunEntry::new(display, status, path));
    }
    let report = api::data::pull_dry_run::PullDryRun::new(entries);
    context.api().report(&report)?;
    Ok(ExitCode::SUCCESS)
}
