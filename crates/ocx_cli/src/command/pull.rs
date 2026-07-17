// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::oci;
use ocx_lib::project::expand_all_keyword;

use crate::api;
use crate::app::project_context::load_project_with_lock;
use crate::conventions;
use crate::options;

/// Pre-warm the object store from the project `ocx.lock` without creating symlinks.
///
/// Loads the nearest `ocx.toml` together with its sibling `ocx.lock`, collects
/// every digest-pinned tool entry across the requested groups, and pulls each
/// one into the local object store. Distinct from `ocx package pull`: this
/// command is project-tier (driven by the lock file) and never touches the
/// candidate or current symlink namespace.
///
/// After a successful pull, `ocx.lock` is re-saved with byte-identical content
/// so its mtime advances. This re-fires `direnv watch_file ocx.lock`, ensuring
/// direnv refreshes the environment after the object store catches up to the
/// declared lock. Skipped under `--dry-run`.
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
    /// reserved name `default` selects the top-level `[tools]` table; the
    /// reserved name `all` expands to `default` + every declared `[group.*]`.
    /// When omitted, every `[tools]` and `[group.*]` entry from the lock
    /// is pulled.
    #[arg(short = 'g', long = "group", value_delimiter = ',')]
    pub groups: Vec<String>,

    #[clap(flatten)]
    pub platform: options::PlatformOption,
}

impl Pull {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // ── Phase 1: parse-time validation ───────────────────────────────
        // Reject empty comma segments (`-g ci,,lint`) before any filesystem
        // or network work.
        crate::app::project_context::ensure_group_segments_nonempty(&self.groups)?;

        // ── Phase 2–3: project resolution, lock load, staleness gate ─────
        // Errors propagate to the `main.rs` boundary: logged once via
        // `log::error!` and classified by `app::classify_error` from
        // `ProjectContextError`'s `ClassifyExitCode` impl.
        let ctx = load_project_with_lock(&context).await?;

        // Validate requested groups against the loaded config (unknown → 64).
        crate::app::project_context::ensure_groups_known(&self.groups, &ctx.config)?;

        // ── Phase 4: select tool set from the lock ───────────────────────

        // No positional packages and no compose step: the lock is already
        // authoritative. Walk it directly, preserving lock order (sorted
        // by `(group, name)` at write time, so the result is deterministic).
        //
        // The pull id is the requested platform's leaf
        // (`repository.clone_with_digest(leaf)`, host key → `Any`-offer
        // fallback); an unshipped platform → clean pre-network `NoHostLeaf`
        // (exit 78).
        //
        // `--platform` omitted → the host native platform.
        let platform = conventions::platform_or_default(self.platform.platform.clone());
        let selected: Vec<&ocx_lib::project::LockedTool> = if self.groups.is_empty() {
            ctx.lock.tools.iter().collect()
        } else {
            // Expand `all` → default + every declared `[group.*]` before the
            // filter, so `-g all` warms every group (matches `run`/`env`).
            let expanded = expand_all_keyword(&self.groups, &ctx.config);
            ctx.lock
                .tools
                .iter()
                .filter(|t| expanded.iter().any(|g| g == &t.group))
                .collect()
        };
        let mut pinned: Vec<oci::PinnedIdentifier> = Vec::new();
        for tool in &selected {
            let id = host_pull_pinned(tool, &platform)?;
            // ponytail: O(n) dedup over tools — tiny.
            if !pinned.contains(&id) {
                pinned.push(id);
            }
        }

        // ── Phase 4b: dry-run preview ────────────────────────────────────

        // Dry-run runs after the staleness gate so a stale lock still
        // exits 65 before any preview prints. No network, no store writes.
        if self.dry_run {
            return run_dry_run(&context, &pinned, platform).await;
        }

        let identifiers: Vec<oci::Identifier> = pinned.iter().cloned().map(Into::into).collect();

        // ── Phase 5: pull + report ───────────────────────────────────────

        // `pull_all` short-circuits on an empty slice (returns `Ok(vec![])`),
        // so an unmatched group filter or an empty lock both exit 0 with
        // an empty report — there is nothing to pre-warm, that is not a
        // failure.
        let info = context
            .manager()
            .pull_all(&identifiers, platform, context.concurrency())
            .await?;

        // Re-save the lock with same bytes to advance its mtime, so direnv
        // re-fires after a successful pull. The `tools_content_equal` guard
        // inside `ProjectLock::save` freezes `generated_at` when content is
        // unchanged — atomic rename still advances mtime. Skipped under
        // `--dry-run` because dry-run must not cause any side effects.
        ctx.lock
            .save(
                &ctx.lock_path,
                Some(&ctx.lock),
                context.file_structure().root(),
                &ctx.config_path,
            )
            .await?;

        let entries: Vec<api::data::paths::PathEntry> = identifiers
            .iter()
            .zip(info.iter())
            .map(|(id, info)| api::data::paths::PathEntry {
                package: id.to_string(),
                path: info.dir().root().to_path_buf(),
            })
            .collect();
        let paths = api::data::paths::Paths::new(entries);
        context.api().report(&paths)?;

        Ok(ExitCode::SUCCESS)
    }
}

/// Resolve a locked tool to its host-platform pull [`oci::PinnedIdentifier`].
///
/// Delegates the V1/V2 host-leaf resolution to
/// [`ocx_lib::project::host_leaf_identifier`] — the single source of the
/// absent-host-leaf error ([`ProjectErrorKind::NoHostLeaf`], exit 78) — then
/// asserts the resolved identifier is digest-pinned via `try_into`. The
/// `ProjectError` is converted to `anyhow::Error` so the chain still classifies
/// at the `main.rs` boundary.
///
/// [`ProjectErrorKind::NoHostLeaf`]: ocx_lib::project::error::ProjectErrorKind::NoHostLeaf
fn host_pull_pinned(
    tool: &ocx_lib::project::LockedTool,
    host: &ocx_lib::oci::Platform,
) -> anyhow::Result<oci::PinnedIdentifier> {
    let id = ocx_lib::project::host_leaf_identifier(tool, host).map_err(anyhow::Error::from)?;
    oci::PinnedIdentifier::try_from(id).map_err(|e| {
        anyhow::anyhow!(
            "locked leaf for tool '{}' is not a valid pinned identifier: {e}",
            tool.name
        )
    })
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
/// One dry-run probe result: the cached / would-fetch status plus the resolved
/// object-store path when the package is already present.
type DryRunProbe = (api::data::pull_dry_run::PullStatus, Option<std::path::PathBuf>);

async fn run_dry_run(
    context: &crate::app::Context,
    pinned: &[oci::PinnedIdentifier],
    platform: oci::Platform,
) -> anyhow::Result<ExitCode> {
    use api::data::pull_dry_run::{DryRunEntry, PullDryRun, PullStatus};

    // Fan out one resolve + probe per pinned id, tagged with its input index.
    // `resolve` hits the index (network on a cold cache), so a sequential loop
    // is an O(n) round-trip chain — the real pull already fans out via
    // `pull_all`, so the preview must too. Same index-tagged JoinSet shape as
    // `ocx package info` and `index update`.
    let mut join_set: tokio::task::JoinSet<(usize, anyhow::Result<DryRunProbe>)> = tokio::task::JoinSet::new();
    for (index, id) in pinned.iter().enumerate() {
        let manager = context.manager().clone();
        let identifier: oci::Identifier = id.clone().into();
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

    // Place successes by index; collect failures with their index so the
    // input-order-first error is the one surfaced (deterministic exit code).
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
            DryRunEntry::new(id.to_string(), status, path)
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

    /// A second `--platform` occurrence is a usage error (D4 of
    /// `adr_platform_model_unification.md`).
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
}
