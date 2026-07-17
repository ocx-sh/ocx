// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::Path;
use std::process::ExitCode;

use clap::Parser;
use ocx_lib::project::{ResolveLockOptions, resolve_lock, resolve_lock_touched};

use crate::api::data::lock::{LockEntry, LockReport};
use crate::app::project_context::{load_project_for_mutate, load_project_with_lock, materialize_lock};
use crate::conventions;
use crate::options;

/// Resolve tool tags to digests and write `ocx.lock`.
///
/// Walks the nearest `ocx.toml` and reconciles the whole `ocx.lock` with
/// it. When the lock is already current (its `declaration_hash` matches the
/// config) the existing pins are carried forward verbatim — a byte-identical,
/// idempotent no-op that never advances a moving tag. When the config drifted,
/// every declared tag is re-resolved; a moving tag may advance to wherever it
/// points today. Fully transactional — either every tool resolves or nothing
/// is written. Use `ocx update` to force a re-resolve of every tag regardless
/// of drift.
///
/// Migrates a legacy lock to the current per-platform format automatically
/// on any write — no separate migration step is required.
///
/// Materializes packages by default after writing the lock (matching
/// `ocx add`). Pass `--no-pull` to write the lock without downloading.
///
/// `--pull` is the affirmative form of the default (redundant but
/// accepted). Both flags use POSIX last-wins semantics (`overrides_with`):
/// `--no-pull --pull` resolves to pull; `--pull --no-pull` resolves to
/// no-pull.
#[derive(Parser, Clone)]
pub struct Lock {
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

    #[clap(flatten)]
    pub pull: options::Pull,

    #[clap(flatten)]
    pub platform: options::PlatformOption,
}

impl Lock {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
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

        // Stage as a lock-only mutation — the candidate config is
        // byte-identical to the snapshot. `commit` will skip the
        // manifest write.
        let staged = guard.stage(|_cfg| Ok(()))?.lock_only();

        // Whole-file reconcile (spec §4.6) branched on predecessor freshness so
        // a CLEAN lock is preserved verbatim — never silently bumped:
        //
        // - No predecessor → create the lock from scratch (`resolve_lock`).
        // - Predecessor present AND clean (config hash == the lock's stored
        //   `declaration_hash`) → carry every pin forward verbatim with NO live
        //   resolve. `resolve_lock_touched` with an empty `touched` set resolves
        //   nothing: V2 entries pass through byte-identical, V1 entries are
        //   migrated by exact pinned-index transcription, and a gone V1 index
        //   fails `LockUpgradeRequired` (78, "run `ocx update`"). A moved
        //   upstream tag must NOT change the pin on a clean lock — that is the
        //   lock-vs-update distinction.
        // - Predecessor present AND dirty (config changed since the last lock)
        //   → full whole-file re-resolve of every declared tag; advancing a
        //   moved tag is the intended explicit whole-file reconcile behaviour.
        //
        // `save` preserves `generated_at` via `tools_content_equal`, so a clean
        // reconcile is a byte-identical no-op. V1 → V2 migration falls out of
        // the resolver writing V2 only.
        let new_lock = match guard.previous_lock().cloned() {
            Some(prev) if prev.metadata.declaration_hash == staged.config().declaration_hash_cached() => {
                resolve_lock_touched(
                    staged.config(), // candidate
                    staged.config(), // pre-mutation snapshot (lock-only: identical to candidate)
                    &prev,
                    context.default_index(),
                    &[], // empty touched ⇒ resolve nothing; carry every pin forward
                    ResolveLockOptions::default(),
                )
                .await?
            }
            _ => {
                resolve_lock(
                    staged.config(),
                    context.default_index(),
                    &[],
                    ResolveLockOptions::default(),
                )
                .await?
            }
        };

        let config_path = guard.config_path().to_path_buf();
        let commit = guard.commit(staged, new_lock.clone()).await?;
        let _ = commit;

        // Best-effort materialization AFTER the commit lands. A failure here
        // does not roll back the lock — the declaration is committed; only
        // the object-store population is deferred. Matches `add` semantics.
        // `--no-pull` opts out: defers to `ocx pull` or the first direnv hit.
        let eager = self.pull.enabled(true);
        let platform = conventions::platform_or_default(self.platform.platform.clone());
        materialize_lock(&context, &new_lock, eager, platform.clone()).await?;

        // Non-fatal advisory note when `.gitattributes` lacks
        // `ocx.lock merge=union`.
        let project_dir = config_path.parent().unwrap_or_else(|| Path::new("."));
        if !gitattributes_has_merge_union(project_dir).await {
            context
                .ui()
                .warn("add `ocx.lock merge=union` to .gitattributes to avoid merge conflicts");
        }

        let report_platform = platform;
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

    // ── cases ─────────────────────────────────────────────────────────────────

    /// `--pull`/`--no-pull` wire through the shared `options::Pull` flatten;
    /// `lock` defaults to eager. The full flag matrix is tested on the
    /// flatten struct itself (`options/pull.rs`).
    #[test]
    fn pull_flags_flatten_with_eager_default() {
        assert!(parse(&["lock"]).pull.enabled(true), "default must be eager");
        assert!(
            !parse(&["lock", "--no-pull"]).pull.enabled(true),
            "--no-pull must defer"
        );
    }

    /// `--platform` accepts a single value.
    #[test]
    fn parses_platform_flag() {
        let lock = Lock::try_parse_from(["lock", "-p", "linux/arm64"]).unwrap();
        assert_eq!(
            lock.platform.platform.map(|p| p.to_string()),
            Some("linux/arm64".to_owned())
        );
    }

    /// A second `--platform` occurrence is a usage error (D4 of
    /// `adr_platform_model_unification.md`).
    #[test]
    fn rejects_repeated_platform_flag() {
        assert!(
            Lock::try_parse_from(["lock", "-p", "linux/arm64", "-p", "linux/amd64"]).is_err(),
            "repeated --platform must be rejected"
        );
    }
}
