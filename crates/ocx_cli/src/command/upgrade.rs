// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::cli;
use ocx_lib::project::{LockedTool, ProjectLock, ResolveLockOptions, resolutions_content_equal, resolve_lock};

use crate::api::data::lock::{LockEntry, LockReport};
use crate::app::CommandError;
use crate::app::project_context::{load_project_for_mutate, materialize_lock};
use crate::options;

/// Re-resolve every advisory tag in `ocx.toml` and rewrite `ocx.lock`.
///
/// The whole-file bump verb: every declared tag is re-resolved against the
/// registry, even when the lock is already current. A moving tag (`:latest`,
/// `:3`) advances to wherever it points today; an unchanged result rewrites
/// the lock byte-identically. Use `ocx lock` for an idempotent reconcile that
/// only re-resolves when `ocx.toml` drifted. Fully transactional — on any
/// resolution failure nothing is written. Offline or frozen plus an uncached
/// tag exits 81.
///
/// Materializes packages by default after writing the lock (matching
/// `ocx add`). Pass `--no-pull` to write the lock without downloading. The
/// `--check` path is read-only and never materializes.
///
/// `--pull` is the affirmative form of the default (redundant but
/// accepted). Both flags use POSIX last-wins semantics (`overrides_with`):
/// `--no-pull --pull` resolves to pull; `--pull --no-pull` resolves to
/// no-pull.
#[derive(Parser, Clone)]
pub struct Upgrade {
    /// Verify the candidate lock would match the predecessor and exit.
    ///
    /// Re-resolves every declared tag, compares the candidate to the
    /// predecessor, and exits 0 (matches) or 65 (`DataError`, a pin would
    /// change). No writes, no commit. When the predecessor lock is absent,
    /// exits 78 (`ConfigError`).
    #[arg(long = "check", default_value_t = false)]
    pub check: bool,

    /// Materialize packages into the object store after writing the lock (default).
    ///
    /// `--pull` is the affirmative form of the default behavior; `--no-pull`
    /// opts out. Both flags use POSIX last-wins semantics (`overrides_with`):
    /// `--pull --no-pull` resolves to no-pull; `--no-pull --pull` resolves
    /// to pull. Combining the flags is not an error - git `--[no-]verify`
    /// idiom.
    #[arg(long, overrides_with = "no_pull")]
    pub pull: bool,

    /// Write the lock without downloading. Materialization is deferred to
    /// `ocx pull` or first `ocx run` / direnv hit. Useful for CI flows that
    /// batch lock changes and materialize separately.
    #[arg(long = "no-pull", overrides_with = "pull")]
    pub no_pull: bool,

    #[clap(flatten)]
    pub platforms: options::Platforms,
}

impl Upgrade {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // Errors propagate to the `main.rs` boundary (logged + classified).
        let guard = load_project_for_mutate(&context).await?;

        // Stage as a lock-only mutation; the candidate config is
        // byte-identical to the snapshot.
        let staged = guard.stage(|_cfg| Ok(()))?.lock_only();

        let resolve_index = context.default_index();

        // ── --check missing-predecessor gate (spec §4.5) ───────────────
        //
        // For `--check`, verify a predecessor lock exists BEFORE the
        // whole-file re-resolve. With no lock, the re-resolve would still
        // hit the registry — a registry/auth/policy failure would then mask
        // the intended exit 78 (and waste a network round-trip). Checking
        // first makes "no lock to verify against" return ConfigError (78)
        // deterministically, with no network attempted.
        if self.check && guard.previous_lock().is_none() {
            return Err(CommandError::new(
                format!(
                    "ocx.lock not found at {}; run `ocx lock` to create it",
                    guard.lock_path().display()
                ),
                cli::ExitCode::ConfigError,
            )
            .into());
        }

        // Whole-file bump (spec §4.5): bare `resolve_lock(&[])` re-resolves
        // every declared tag. There is no subset, no carry-forward, nothing
        // untouched — laundering and drift are impossible by construction.
        let new_lock = resolve_lock(staged.config(), resolve_index, &[], ResolveLockOptions::default()).await?;

        // ── --check verify-only path ───────────────────────────────────
        //
        // `--check` performs the whole-file re-resolve above and exits
        // without writing: 0 when the candidate matches the predecessor,
        // 65 when any pinned content would change (an advisory tag moved
        // upstream). The missing-predecessor case (78) was already handled
        // above, before the re-resolve.
        if self.check {
            // The missing-predecessor gate above guarantees a predecessor here.
            let prev = guard
                .previous_lock()
                .expect("missing-predecessor gate guarantees a predecessor lock for --check");
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

        // Best-effort materialization AFTER the commit lands. A failure here
        // does not roll back the lock — the declaration is committed; only
        // the object-store population is deferred. Matches `add` semantics.
        // The `--check` early-return above ensures this line is never reached
        // on the verify-only path. `--no-pull` opts out.
        let eager = !self.no_pull;
        materialize_lock(&context, &new_lock, eager, self.platforms.as_slice()).await?;

        let report_platform = crate::app::project_context::primary_platform(self.platforms.as_slice());
        let entries: Vec<LockEntry> = new_lock
            .tools
            .iter()
            .map(|t| LockEntry::from_tool(t, &report_platform))
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
/// resolves to a different digest for any entry must surface as
/// `DataError` (exit 65) without writing.
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
        .all(|(x, y)| x.name == y.name && x.group == y.group && resolutions_content_equal(&x.resolution, &y.resolution))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    // ── helpers ──────────────────────────────────────────────────────────────

    fn parse(args: &[&str]) -> Upgrade {
        Upgrade::try_parse_from(args).unwrap()
    }

    fn eager(upgrade: &Upgrade) -> bool {
        !upgrade.no_pull
    }

    // ── cases ─────────────────────────────────────────────────────────────────

    /// Neither `--pull` nor `--no-pull` → both fields false; default is eager.
    #[test]
    fn parse_no_flags_defaults_to_eager() {
        let upgrade = parse(&["upgrade"]);
        assert!(!upgrade.pull, "pull must be false when neither flag is given");
        assert!(!upgrade.no_pull, "no_pull must be false when neither flag is given");
        assert!(eager(&upgrade), "default must be eager (eager = !no_pull)");
    }

    /// `--pull` alone → pull=true, no_pull=false; still eager.
    #[test]
    fn parse_only_pull_is_eager() {
        let upgrade = parse(&["upgrade", "--pull"]);
        assert!(upgrade.pull, "pull must be true with --pull");
        assert!(!upgrade.no_pull, "no_pull must be false with --pull only");
        assert!(eager(&upgrade), "eager must be true when --pull is the last flag");
    }

    /// `--no-pull` alone → pull=false, no_pull=true; lazy.
    #[test]
    fn parse_only_no_pull_is_lazy() {
        let upgrade = parse(&["upgrade", "--no-pull"]);
        assert!(!upgrade.pull, "pull must be false with --no-pull only");
        assert!(upgrade.no_pull, "no_pull must be true with --no-pull");
        assert!(!eager(&upgrade), "eager must be false when --no-pull is set");
    }

    /// `--pull --no-pull` → POSIX last-wins: no_pull wins, pull=false; lazy.
    #[test]
    fn parse_pull_then_no_pull_no_pull_wins() {
        let upgrade = parse(&["upgrade", "--pull", "--no-pull"]);
        assert!(upgrade.no_pull, "no_pull must be true when --no-pull follows --pull");
        assert!(!upgrade.pull, "pull must be false when --no-pull overrides it");
        assert!(!eager(&upgrade), "eager must be false when --no-pull wins");
    }

    /// `--no-pull --pull` → POSIX last-wins: pull wins, no_pull=false; eager.
    #[test]
    fn parse_no_pull_then_pull_pull_wins() {
        let upgrade = parse(&["upgrade", "--no-pull", "--pull"]);
        assert!(upgrade.pull, "pull must be true when --pull follows --no-pull");
        assert!(!upgrade.no_pull, "no_pull must be false when --pull overrides it");
        assert!(eager(&upgrade), "eager must be true when --pull wins");
    }

    // ── Whole-file model: the subset surface is gone (spec §4.5, §8.1) ──

    /// `ocx upgrade --group ci` is rejected by clap — the `--group`/`-g` flag
    /// no longer exists. `upgrade` is the whole-file bump verb; groups are a
    /// composition concern only, never an upgrade scope. clap's unknown-arg
    /// error maps to EX_USAGE (64) at the `main.rs` boundary.
    #[test]
    fn rejects_group_flag() {
        assert!(
            Upgrade::try_parse_from(["upgrade", "--group", "ci"]).is_err(),
            "`ocx upgrade --group ci` must be rejected: the subset surface is gone"
        );
        assert!(
            Upgrade::try_parse_from(["upgrade", "-g", "ci"]).is_err(),
            "`ocx upgrade -g ci` must be rejected: the subset surface is gone"
        );
    }

    /// `ocx upgrade <binding>` is rejected by clap — positional binding names
    /// no longer exist. `upgrade` always re-resolves the whole file.
    #[test]
    fn rejects_positional_args() {
        assert!(
            Upgrade::try_parse_from(["upgrade", "cmake"]).is_err(),
            "`ocx upgrade cmake` must be rejected: positional scoping is gone"
        );
    }

    /// `--platform` is repeatable and parses into the flattened `Platforms`.
    #[test]
    fn parses_repeatable_platform_flag() {
        let upgrade = parse(&["upgrade", "--platform", "linux/arm64", "-p", "linux/amd64"]);
        assert_eq!(
            upgrade.platforms.as_slice().len(),
            2,
            "two --platform values must parse into two entries"
        );
    }
}
