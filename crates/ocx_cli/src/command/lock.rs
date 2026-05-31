// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::Path;
use std::process::ExitCode;

use clap::Parser;
use ocx_lib::cli;
use ocx_lib::project::{DEFAULT_GROUP, ResolveLockOptions, resolve_lock};

use crate::api::data::lock::{LockEntry, LockReport};
use crate::app::project_context::{load_project_for_mutate, load_project_with_lock, materialize_lock};

/// Resolve tool tags to digests and write `ocx.lock`.
///
/// Walks the nearest `ocx.toml`, resolves each tool's advisory tag to a
/// pinned OCI index-manifest digest, and writes a deterministic
/// `ocx.lock` next to it. Fully transactional — either every tool
/// resolves or nothing is written.
///
/// **Behavior change:** this command now materializes packages by default
/// after writing the lock (matching `ocx add`). Pass `--no-pull` to
/// restore the prior lock-only behavior.
///
/// `--pull` is the affirmative form of the default (redundant but
/// accepted). Both flags use POSIX last-wins semantics (`overrides_with`):
/// `--no-pull --pull` resolves to pull; `--pull --no-pull` resolves to
/// no-pull.
#[derive(Parser, Clone)]
pub struct Lock {
    /// Restrict resolution to the named group(s).
    ///
    /// Repeatable and comma-separated: `-g ci,lint -g release`. The
    /// reserved name `default` selects the top-level `[tools]` table.
    /// When omitted, every `[tools]` and `[group.*]` entry is resolved.
    #[arg(short = 'g', long = "group", value_delimiter = ',')]
    pub groups: Vec<String>,

    /// Verify `ocx.lock` is current relative to `ocx.toml` and exit.
    ///
    /// Reads `ocx.toml` and `ocx.lock` from disk, compares the lock's
    /// stored `declaration_hash` against the current config's hash,
    /// and exits 0 if they match (lock is current) or 65 if they
    /// drift (lock is stale). No re-resolution, no writes, no network
    /// calls - strictly a CI primitive for "is the lock committed and
    /// current?" verification. When the lock file is absent, exits 78
    /// (the canonical "lock missing" code shared with `ocx pull`).
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
}

impl Lock {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // Pre-validate empty comma segments before any I/O.
        for raw in &self.groups {
            if raw.is_empty() {
                return Err(
                    cli::UsageError::new("empty group segment in --group value; check for stray commas").into(),
                );
            }
        }

        // ── --check fast path ────────────────────────────────────────────
        //
        // `--check` is a no-op verification: read ocx.toml + ocx.lock,
        // compare hashes, exit 0/65/78 without ever acquiring the project
        // flock or touching the network. Routes through
        // `load_project_with_lock` which already enforces the staleness
        // gate via `StaleLock` (exit 65) and surfaces `LockMissing`
        // (exit 78) for the "no lock at all" case.
        if self.check {
            return run_check(&context).await;
        }

        // Acquire flock + load snapshot. `ocx lock` mutates the lock file
        // only; the staging closure is identity, and `lock_only()`
        // suppresses the manifest rewrite at commit time.
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

        // Stage as a lock-only mutation — the candidate config is
        // byte-identical to the snapshot. `commit` will skip the
        // manifest write.
        let staged = guard.stage(|_cfg| Ok(()))?.lock_only();

        let new_lock = resolve_lock(
            staged.config(),
            context.default_index(),
            &self.groups,
            ResolveLockOptions::default(),
        )
        .await?;

        let config_path = guard.config_path().to_path_buf();
        let commit = guard.commit(staged, new_lock.clone()).await?;
        let _ = commit;

        // Best-effort materialization AFTER the commit lands. A failure here
        // does not roll back the lock — the declaration is committed; only
        // the object-store population is deferred. Matches `add` semantics.
        // `--no-pull` opts out: defers to `ocx pull` or the first direnv hit.
        let eager = !self.no_pull;
        materialize_lock(&context, &new_lock, eager).await?;

        // Non-fatal advisory note when `.gitattributes` lacks
        // `ocx.lock merge=union`.
        let project_dir = config_path.parent().unwrap_or_else(|| Path::new("."));
        if !gitattributes_has_merge_union(project_dir).await {
            context
                .ui()
                .warn("add `ocx.lock merge=union` to .gitattributes to avoid merge conflicts");
        }

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

/// CI-primitive verification path for `ocx lock --check`.
///
/// Reads `ocx.toml` and `ocx.lock` from disk and exits without touching
/// the network or the lock file. Reuses the existing project-context
/// prologue so the staleness gate (`StaleLock` → exit 65) and missing-
/// lock gate (`LockMissing` → exit 78) are byte-identical to what
/// `ocx run` and `ocx pull` already enforce. Success returns exit 0.
async fn run_check(context: &crate::app::Context) -> anyhow::Result<ExitCode> {
    // All `ProjectContextError` variants classify at the `main.rs` boundary
    // (NoProject→64, LockMissing→78, StaleLock→65); propagate and let the
    // boundary log + map the exit code.
    load_project_with_lock(context).await?;
    Ok(ExitCode::SUCCESS)
}

/// Probe whether `{project_dir}/.gitattributes` contains the
/// `ocx.lock merge=union` attribute line.
async fn gitattributes_has_merge_union(project_dir: &Path) -> bool {
    let path = project_dir.join(".gitattributes");
    let Ok(contents) = tokio::fs::read_to_string(&path).await else {
        return false;
    };
    contents.lines().any(|line| {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            return false;
        }
        let mut tokens = trimmed.split_whitespace();
        let Some(pattern) = tokens.next() else {
            return false;
        };
        if pattern != "ocx.lock" {
            return false;
        }
        tokens.any(|t| t == "merge=union")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    // ── helpers ──────────────────────────────────────────────────────────────

    fn parse(args: &[&str]) -> Lock {
        Lock::try_parse_from(args).unwrap()
    }

    fn eager(lock: &Lock) -> bool {
        !lock.no_pull
    }

    // ── cases ─────────────────────────────────────────────────────────────────

    /// Neither `--pull` nor `--no-pull` → both fields false; default is eager.
    #[test]
    fn parse_no_flags_defaults_to_eager() {
        let lock = parse(&["lock"]);
        assert!(!lock.pull, "pull must be false when neither flag is given");
        assert!(!lock.no_pull, "no_pull must be false when neither flag is given");
        assert!(eager(&lock), "default must be eager (eager = !no_pull)");
    }

    /// `--pull` alone → pull=true, no_pull=false; still eager.
    #[test]
    fn parse_only_pull_is_eager() {
        let lock = parse(&["lock", "--pull"]);
        assert!(lock.pull, "pull must be true with --pull");
        assert!(!lock.no_pull, "no_pull must be false with --pull only");
        assert!(eager(&lock), "eager must be true when --pull is the last flag");
    }

    /// `--no-pull` alone → pull=false, no_pull=true; lazy.
    #[test]
    fn parse_only_no_pull_is_lazy() {
        let lock = parse(&["lock", "--no-pull"]);
        assert!(!lock.pull, "pull must be false with --no-pull only");
        assert!(lock.no_pull, "no_pull must be true with --no-pull");
        assert!(!eager(&lock), "eager must be false when --no-pull is set");
    }

    /// `--pull --no-pull` → POSIX last-wins: no_pull wins, pull=false; lazy.
    #[test]
    fn parse_pull_then_no_pull_no_pull_wins() {
        let lock = parse(&["lock", "--pull", "--no-pull"]);
        assert!(lock.no_pull, "no_pull must be true when --no-pull follows --pull");
        assert!(!lock.pull, "pull must be false when --no-pull overrides it");
        assert!(!eager(&lock), "eager must be false when --no-pull wins");
    }

    /// `--no-pull --pull` → POSIX last-wins: pull wins, no_pull=false; eager.
    #[test]
    fn parse_no_pull_then_pull_pull_wins() {
        let lock = parse(&["lock", "--no-pull", "--pull"]);
        assert!(lock.pull, "pull must be true when --pull follows --no-pull");
        assert!(!lock.no_pull, "no_pull must be false when --pull overrides it");
        assert!(eager(&lock), "eager must be true when --pull wins");
    }
}
