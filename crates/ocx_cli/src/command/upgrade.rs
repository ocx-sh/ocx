// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::cli;
use ocx_lib::project::error::ProjectErrorKind;
use ocx_lib::project::{
    DEFAULT_GROUP, LockedTool, ProjectLock, ResolveLockOptions, resolve_lock, resolve_lock_partial,
};

use crate::api::data::lock::{LockEntry, LockReport};
use crate::app::CommandError;
use crate::app::project_context::load_project_for_mutate;

/// Re-resolve advisory tags for one or more tools and rewrite `ocx.lock`.
///
/// Opt-in mutation: never triggered automatically. With no arguments,
/// re-resolves every tool in `ocx.toml` (equivalent to `ocx lock` from
/// scratch). With positional binding names or `--group` filters,
/// re-resolves only the matching subset and preserves every other entry
/// already present in `ocx.lock`. Fully transactional — on any
/// resolution failure nothing is written.
#[derive(Parser, Clone)]
pub struct Upgrade {
    /// Restrict re-resolution to the named group(s).
    ///
    /// Repeatable and comma-separated: `-g ci,lint -g release`. The
    /// reserved name `default` selects the top-level `[tools]` table.
    /// Combinable with positional binding names (intersection).
    #[arg(short = 'g', long = "group", value_delimiter = ',')]
    pub groups: Vec<String>,

    /// Binding names from `ocx.toml` to re-resolve.
    ///
    /// Each value is the local TOML key (e.g. `cmake`, not
    /// `ocx.sh/cmake:3.28`). Names not declared in `ocx.toml` produce
    /// `NotFound` (79) and no lock write.
    #[arg(value_name = "BINDING")]
    pub packages: Vec<String>,

    /// Verify the candidate lock would match the predecessor and exit.
    ///
    /// Mirrors `ocx lock --check`: re-resolves the requested subset
    /// (or the full toolchain when no positional names and no `-g` are
    /// supplied), compares the candidate to the predecessor, and exits
    /// 0 (matches) or 65 (`DataError`, candidate would change). No
    /// writes, no commit. When the predecessor lock is absent, exits
    /// 78 (`ConfigError`).
    #[arg(long = "check", default_value_t = false)]
    pub check: bool,
}

impl Upgrade {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // Pre-validate empty `--group` segments before any I/O.
        for raw in &self.groups {
            if raw.is_empty() {
                return Err(
                    cli::UsageError::new("empty group segment in --group value; check for stray commas").into(),
                );
            }
        }

        // Errors propagate to the `main.rs` boundary (logged + classified).
        let guard = load_project_for_mutate(&context).await?;

        // Validate group filter against the loaded snapshot.
        for raw in &self.groups {
            if raw == DEFAULT_GROUP {
                continue;
            }
            if !guard.config().groups.contains_key(raw) {
                return Err(cli::UsageError::new(format!("unknown group '{raw}' in --group filter")).into());
            }
        }

        // Validate every positional binding exists in the snapshot.
        for name in &self.packages {
            let in_default = guard.config().tools.contains_key(name);
            let in_groups = guard.config().groups.values().any(|tools| tools.contains_key(name));
            if !in_default && !in_groups {
                return Err(CommandError::new(
                    format!("tool '{name}' not declared in ocx.toml"),
                    cli::ExitCode::NotFound,
                )
                .into());
            }
        }

        // Stage as a lock-only mutation; the candidate config is
        // byte-identical to the snapshot.
        let staged = guard.stage(|_cfg| Ok(()))?.lock_only();

        let resolve_index = context.default_index();

        let new_lock = if self.packages.is_empty() && self.groups.is_empty() {
            resolve_lock(staged.config(), resolve_index, &[], ResolveLockOptions::default()).await?
        } else {
            let prev = match guard.previous_lock() {
                Some(p) => p.clone(),
                None => {
                    return Err(CommandError::new(
                        format!(
                            "ocx.lock not found at {}; run `ocx lock` to create it before upgrading a subset",
                            guard.lock_path().display()
                        ),
                        cli::ExitCode::ConfigError,
                    )
                    .into());
                }
            };
            // Try the partial resolve first. When the predecessor's
            // declaration_hash drifts from the current config (the user
            // hand-edited `ocx.toml` between lock and update — common
            // workflow), partial-resolve refuses with `StaleLockOnPartial`.
            // Fall back to a full `resolve_lock` per the Cluster A plan
            // ("Caller orchestrates fallback to `resolve_lock` on
            // mismatch"): the gate prevents laundering, so on mismatch
            // we rebuild from scratch.
            match resolve_lock_partial(
                staged.config(),
                &prev,
                resolve_index,
                &self.packages,
                &self.groups,
                ResolveLockOptions::default(),
            )
            .await
            {
                Ok(lock) => lock,
                Err(ocx_lib::project::Error::Project(pe))
                    if matches!(pe.kind, ProjectErrorKind::StaleLockOnPartial { .. }) =>
                {
                    resolve_lock(staged.config(), resolve_index, &[], ResolveLockOptions::default()).await?
                }
                Err(other) => return Err(other.into()),
            }
        };

        // ── --check verify-only path ───────────────────────────────────
        //
        // `--check` performs the partial-resolve dry-run above and exits
        // without writing: 0 when the candidate matches the predecessor,
        // 65 when any pinned content would change (an advisory tag moved
        // upstream for a selected or preserved entry), 78 when there is
        // no predecessor lock to compare against.
        if self.check {
            let Some(prev) = guard.previous_lock() else {
                return Err(CommandError::new(
                    format!(
                        "ocx.lock not found at {}; run `ocx lock` to create it",
                        guard.lock_path().display()
                    ),
                    cli::ExitCode::ConfigError,
                )
                .into());
            };
            if !lock_content_matches(&new_lock, prev) {
                return Err(CommandError::new(
                    "ocx.lock candidate would change pinned content; \
                     re-run `ocx upgrade` (without --check) to refresh the lock.",
                    cli::ExitCode::DataError,
                )
                .into());
            }
            return Ok(ExitCode::SUCCESS);
        }

        let commit = guard.commit(staged, new_lock.clone()).await?;
        let _ = commit;

        let entries: Vec<LockEntry> = new_lock
            .tools
            .iter()
            .map(|t| LockEntry {
                binding: t.name.clone(),
                group: t.group.clone(),
                digest: t.pinned.strip_advisory().to_string(),
            })
            .collect();
        let report = LockReport::new(entries);
        context.api().report(&report)?;

        Ok(ExitCode::SUCCESS)
    }
}

/// Resolved-content equality: two locks match when they share the same
/// `(group, name, pinned content)` tuples and the same load-bearing
/// metadata (declaration_hash, lock_version, declaration_hash_version).
/// Advisory metadata (`generated_at`, `generated_by`) is ignored.
///
/// Used by the `upgrade --check` verify-only path: a candidate that
/// resolves to a different digest for any selected or preserved entry
/// must surface as `DataError` (exit 65) without writing.
fn lock_content_matches(candidate: &ProjectLock, prev: &ProjectLock) -> bool {
    if candidate.metadata.declaration_hash != prev.metadata.declaration_hash
        || candidate.metadata.declaration_hash_version != prev.metadata.declaration_hash_version
        || candidate.metadata.lock_version != prev.metadata.lock_version
    {
        return false;
    }
    if candidate.tools.len() != prev.tools.len() {
        return false;
    }
    let mut a: Vec<&LockedTool> = candidate.tools.iter().collect();
    let mut b: Vec<&LockedTool> = prev.tools.iter().collect();
    a.sort_by(|x, y| (x.group.as_str(), x.name.as_str()).cmp(&(y.group.as_str(), y.name.as_str())));
    b.sort_by(|x, y| (x.group.as_str(), x.name.as_str()).cmp(&(y.group.as_str(), y.name.as_str())));
    a.iter()
        .zip(b.iter())
        .all(|(x, y)| x.name == y.name && x.group == y.group && x.pinned.eq_content(&y.pinned))
}
