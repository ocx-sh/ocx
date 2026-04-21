// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::project::{ProjectConfig, ProjectLock, declaration_hash};
use ocx_lib::{cli, env, oci};

use crate::api;
use crate::conventions::platforms_or_default;

/// CLI-layer sentinel for the reserved default group name. The library
/// keeps its copy private in `project::internal`; duplicating a single
/// literal across the project-tier CLI commands is cheaper than punching
/// a hole in that encapsulation. Mirrors the constant in `command/lock.rs`
/// and `command/exec.rs`.
const DEFAULT_GROUP: &str = "default";

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

        // ── Phase 2: project resolution ──────────────────────────────────

        // Resolve `ocx.toml` + sibling `ocx.lock` paths with the full
        // precedence chain: explicit flag > env > CWD walk > home fallback.
        let cwd = env::current_dir()?;
        let home = context.file_structure().root().to_path_buf();
        let resolved = ProjectConfig::resolve(Some(&cwd), context.project_path(), Some(&home)).await?;

        let (config_path, lock_path) = match resolved {
            Some(pair) => pair,
            None => {
                // No `ocx.toml` anywhere in the precedence chain — usage
                // error. The message mentions `ocx.toml` so the Python
                // assertion finds it in stderr.
                eprintln!(
                    "no ocx.toml found in {} or any parent; create one, or run from a directory that contains it",
                    cwd.display()
                );
                return Ok(cli::ExitCode::UsageError.into());
            }
        };

        // Load the config so we can validate `--group` names against real
        // group keys before touching the lock.
        let config = ProjectConfig::from_path(&config_path).await?;

        // Validate requested groups against the loaded config. Unknown
        // groups produce exit 64. `default` is always valid since it
        // names the top-level `[tools]` table.
        for raw in &self.groups {
            if raw == DEFAULT_GROUP {
                continue;
            }
            if !config.groups.contains_key(raw) {
                eprintln!("unknown group '{raw}' in --group filter");
                return Ok(cli::ExitCode::UsageError.into());
            }
        }

        // ── Phase 3: lock load + staleness gate ──────────────────────────

        // Open without holding an advisory lock — pull is read-only on
        // ocx.lock; only `ocx lock` and `ocx update` write it.
        let lock = match ProjectLock::from_path(&lock_path).await? {
            Some(l) => l,
            None => {
                // Missing lock when a project config is present is a
                // ConfigError (exit 78). Message points at the fix.
                eprintln!(
                    "ocx.lock not found at {}; run `ocx lock` to create it",
                    lock_path.display()
                );
                return Ok(cli::ExitCode::ConfigError.into());
            }
        };

        // Staleness gate: the lock's stored declaration_hash must match
        // the current config. A mismatch means `ocx.toml` changed since
        // the lock was written → DataError (exit 65).
        let current_hash = declaration_hash(&config);
        if lock.metadata.declaration_hash != current_hash {
            eprintln!("ocx.lock is stale (ocx.toml changed since last `ocx lock`); run `ocx lock`");
            return Ok(cli::ExitCode::DataError.into());
        }

        // ── Phase 4: select tool set from the lock ───────────────────────

        // No positional packages and no compose step: the lock is already
        // authoritative. Walk it directly, preserving lock order (sorted
        // by `(group, name)` at write time, so the result is deterministic).
        let pinned: Vec<oci::PinnedIdentifier> = if self.groups.is_empty() {
            lock.tools.iter().map(|t| t.pinned.clone()).collect()
        } else {
            lock.tools
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
                path: info.content.clone(),
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
                Some(info) => (api::data::pull_dry_run::PullStatus::Cached, Some(info.content.clone())),
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
