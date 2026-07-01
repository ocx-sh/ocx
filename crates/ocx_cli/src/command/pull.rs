// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::cli;
use ocx_lib::oci;
use ocx_lib::project::{ALL_GROUP, DEFAULT_GROUP, expand_all_keyword};

use crate::api;
use crate::app::project_context::load_project_with_lock;
use crate::conventions::platforms_or_default;
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
    pub platforms: options::Platforms,
}

impl Pull {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // ── Phase 1: parse-time validation ───────────────────────────────

        // Reject empty comma segments (`-g ci,,lint`) BEFORE any filesystem
        // or network work. `clap`'s `value_delimiter = ','` splits the value
        // into `["ci", "", "lint"]`; an empty string is a user-typing error.
        for raw in &self.groups {
            if raw.is_empty() {
                return Err(
                    cli::UsageError::new("empty group segment in --group value; check for stray commas").into(),
                );
            }
        }

        // ── Phase 2–3: project resolution, lock load, staleness gate ─────
        // Errors propagate to the `main.rs` boundary: logged once via
        // `log::error!` and classified by `app::classify_error` from
        // `ProjectContextError`'s `ClassifyExitCode` impl.
        let ctx = load_project_with_lock(&context).await?;

        // Validate requested groups against the loaded config. Unknown
        // groups produce exit 64. `default` is always valid since it
        // names the top-level `[tools]` table.
        for raw in &self.groups {
            if raw == DEFAULT_GROUP || raw == ALL_GROUP {
                continue;
            }
            if !ctx.config.groups.contains_key(raw) {
                return Err(cli::UsageError::new(format!("unknown group '{raw}' in --group filter")).into());
            }
        }

        // ── Phase 4: select tool set from the lock ───────────────────────

        // No positional packages and no compose step: the lock is already
        // authoritative. Walk it directly, preserving lock order (sorted
        // by `(group, name)` at write time, so the result is deterministic).
        //
        // V2 ([`LockedResolution::PerPlatform`]): the pull id is the requested
        // platform's leaf (`repository.clone_with_digest(leaf)`, host key →
        // `"any"` fallback); an unshipped platform → clean pre-network
        // `NoHostLeaf` (exit 78). V1 ([`LockedResolution::LegacyIndex`]): the
        // legacy index-digest path (identical for every platform, deduped
        // below; the requested platforms drive `Index::select` in `pull_all`).
        //
        // `--platform` omitted → the host native platform (unchanged). One or
        // more `--platform` values → warm each requested platform's leaf, so an
        // amd64 host can pre-warm an arm64 leaf.
        let selection: Vec<oci::Platform> = if self.platforms.as_slice().is_empty() {
            vec![oci::Platform::current().unwrap_or_else(oci::Platform::any)]
        } else {
            self.platforms.as_slice().to_vec()
        };
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
        // A V1 legacy tool resolves the same index id for every platform, so a
        // multi-platform request would silently materialize only the first (see
        // `reject_multi_platform_on_legacy`). Fail loud before any store write.
        let has_legacy = selected
            .iter()
            .any(|t| matches!(t.resolution, ocx_lib::project::LockedResolution::LegacyIndex(_)));
        crate::app::project_context::reject_multi_platform_on_legacy(has_legacy, self.platforms.as_slice())?;
        let mut pinned: Vec<oci::PinnedIdentifier> = Vec::new();
        for tool in &selected {
            for platform in &selection {
                let id = host_pull_pinned(tool, platform)?;
                // ponytail: O(n²) dedup over tools×platforms — both tiny.
                if !pinned.contains(&id) {
                    pinned.push(id);
                }
            }
        }

        // ── Phase 4b: dry-run preview ────────────────────────────────────

        // Dry-run runs after the staleness gate so a stale lock still
        // exits 65 before any preview prints. No network, no store writes.
        if self.dry_run {
            return run_dry_run(&context, &pinned, self.platforms.as_slice()).await;
        }

        let identifiers: Vec<oci::Identifier> = pinned.iter().cloned().map(Into::into).collect();

        // ── Phase 5: pull + report ───────────────────────────────────────

        // `pull_all` short-circuits on an empty slice (returns `Ok(vec![])`),
        // so an unmatched group filter or an empty lock both exit 0 with
        // an empty report — there is nothing to pre-warm, that is not a
        // failure.
        let info = context
            .manager()
            .pull_all(
                &identifiers,
                platforms_or_default(self.platforms.as_slice()),
                context.concurrency(),
            )
            .await?;

        // Re-save the lock with same bytes to advance its mtime, so direnv
        // re-fires after a successful pull. The `tools_content_equal` guard
        // inside `ProjectLock::save` freezes `generated_at` when content is
        // unchanged — atomic rename still advances mtime. Skipped under
        // `--dry-run` because dry-run must not cause any side effects.
        //
        // A committed V1 (legacy) lock is NEVER rewritten on a read path: the
        // writer only emits V2, and the ADR forbids a read-path forced upgrade
        // (the migration is an explicit `ocx lock --upgrade` / write command).
        // The mtime bump is purely a direnv-refresh nicety, so skipping it for
        // a V1 lock is harmless — direnv still re-fires on the next write.
        if !lock_has_legacy_entry(&ctx.lock) {
            ctx.lock
                .save(
                    &ctx.lock_path,
                    Some(&ctx.lock),
                    context.file_structure().root(),
                    &ctx.config_path,
                )
                .await?;
        }

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

/// Return `true` when any tool in `lock` is a V1 legacy
/// ([`ocx_lib::project::LockedResolution::LegacyIndex`]) entry. Such a lock
/// must never be rewritten on a read path — the writer only emits V2 and the
/// migration to V2 is an explicit `ocx lock --upgrade` / write command.
fn lock_has_legacy_entry(lock: &ocx_lib::project::ProjectLock) -> bool {
    lock.tools
        .iter()
        .any(|t| matches!(t.resolution, ocx_lib::project::LockedResolution::LegacyIndex(_)))
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
async fn run_dry_run(
    context: &crate::app::Context,
    pinned: &[oci::PinnedIdentifier],
    platforms: &[oci::Platform],
) -> anyhow::Result<ExitCode> {
    let manager = context.manager();
    let platforms = platforms_or_default(platforms);
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
                Some(info) => (
                    api::data::pull_dry_run::PullStatus::Cached,
                    Some(info.dir().root().to_path_buf()),
                ),
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// `--platform` is repeatable (and comma-delimited) and coexists with `-g`.
    #[test]
    fn parses_repeatable_platform_flag() {
        let pull =
            Pull::try_parse_from(["pull", "-g", "ci", "--platform", "linux/arm64", "-p", "linux/amd64"]).unwrap();
        assert_eq!(
            pull.platforms.as_slice().len(),
            2,
            "two --platform values must parse into two entries"
        );
        assert_eq!(
            pull.groups,
            vec!["ci".to_owned()],
            "-g must still parse alongside --platform"
        );
    }

    /// `-g all` parses (the `all` keyword is resolved at execute time).
    #[test]
    fn parses_all_group_keyword() {
        let pull = Pull::try_parse_from(["pull", "-g", "all"]).unwrap();
        assert_eq!(pull.groups, vec!["all".to_owned()]);
    }
}
