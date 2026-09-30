// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::Path;
use std::process::ExitCode;

use clap::Parser;
use ocx_project::{ResolveLockOptions, resolve_lock, resolve_lock_touched};

use crate::api::data::lock::{LockEntry, LockReport};
use crate::app::project_context::{
    load_project_for_mutate, load_project_with_lock, materialize_lock, record_activation_consent,
};
use crate::conventions;
use crate::options;

/// Resolve package tags to digests and write `ocx.lock`.
///
/// A current lock is carried forward verbatim, never advancing a moving tag; a drifted one
/// re-resolves every tag. Transactional: every binding resolves or nothing is written.
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
        // No flock, no network: `load_project_with_lock` already enforces 65 and 78.
        if self.check {
            return run_check(&context).await;
        }

        let guard = load_project_for_mutate(&context).await?;

        // Lock-only: the candidate config is byte-identical, so `commit` skips the manifest write.
        let staged = guard.stage(|_cfg| Ok(()))?.lock_only();

        let new_lock = match guard.previous_lock().cloned() {
            // Clean: carry every pin forward; advancing a moved tag here would silently do `ocx update`'s job.
            Some(prev) if prev.is_current(staged.config()) => {
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
            // Dirty or no predecessor: re-resolve every declared tag.
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
        // `save` keeps `generated_at` when nothing else changed, so a clean reconcile is byte-identical.

        let config_path = guard.config_path().to_path_buf();
        // `--no-pull` promises no downloads and the render's closure walk is one, so render offline; a
        // cold store degrades quietly and `ocx pull` renders later.
        let eager = self.pull.enabled(true);
        let render_manager = if eager {
            context.manager().clone()
        } else {
            context.manager().offline_view(context.local_index().clone())
        };
        let scope = context.toolchain_render_scope(&config_path).await?;
        let platform = conventions::platform_or_default(self.platform.platform.clone());
        // Cloned before the commit consumes `staged`; the eager pull binds the new lock to it.
        let config = staged.config().clone();
        let commit = render_manager
            .commit_and_render(
                guard,
                staged,
                new_lock.clone(),
                ocx_package_manager::ToolchainRender {
                    scope: &scope,
                    toolchain_root: context.toolchain_root(),
                    platform: &platform,
                },
            )
            .await?
            .commit;

        // After the commit, so the stamp records the requested source set, not the one it replaced.
        record_activation_consent(&commit.config_path, &new_lock, None).await;

        // After the commit: a failed download leaves the lock committed.
        materialize_lock(&context, &new_lock, &config, eager, platform.clone()).await?;

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

/// `ocx lock --check`: the staleness (65) and missing-lock (78) gates `ocx exec` and `ocx pull`
/// enforce, with no network and no write.
async fn run_check(context: &crate::app::Context) -> anyhow::Result<ExitCode> {
    load_project_with_lock(context).await?;
    Ok(ExitCode::SUCCESS)
}

/// Whether `{project_dir}/.gitattributes` carries `ocx.lock merge=union`.
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

    /// A second `--platform` occurrence is a usage error, per the
    /// single-platform authoring decision in `adr_platform_model_unification.md`.
    #[test]
    fn rejects_repeated_platform_flag() {
        assert!(
            Lock::try_parse_from(["lock", "-p", "linux/arm64", "-p", "linux/amd64"]).is_err(),
            "repeated --platform must be rejected"
        );
    }
}
