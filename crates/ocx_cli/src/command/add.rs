// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx add [--group <name>] <identifier>...` — append one or more
//! bindings to `ocx.toml`, atomically rewrite `ocx.lock` for impacted
//! tools, and install.

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::project::{ResolveLockOptions, add_binding_in_memory, resolve_lock, resolve_lock_touched};

use crate::api::data::lock::{LockEntry, LockReport};
use crate::app::project_context::{ensure_global_project_initialized, load_project_for_mutate, materialize_lock};
use crate::options;

/// Add one or more tool bindings to `ocx.toml`.
///
/// Appends the given identifiers to the implicit default `[tools]` table,
/// or to a named `[group.<name>]` table when `--group` is supplied.
/// Resolves only the new bindings, carries every existing lock entry
/// forward unchanged, and installs the tools (default eager behavior).
///
/// All identifiers are validated and staged before anything is written:
/// a duplicate identifier (already bound, or repeated in the same batch)
/// aborts the whole command with `ocx.toml` left untouched.
///
/// Fails with exit 65 when `ocx.toml` drifted from `ocx.lock` before
/// this add (run `ocx lock` to reconcile), or exit 78 when a carried
/// legacy entry can no longer be migrated exactly (run `ocx upgrade`).
///
/// Pass `--no-pull` to write only the manifest and lock without
/// downloading; materialization is then deferred to `ocx pull` or the
/// first `ocx run` / direnv hit.
///
/// `--pull` is the affirmative form of the default (redundant but
/// accepted). Both flags use POSIX last-wins semantics (`overrides_with`):
/// `--no-pull --pull` resolves to pull; `--pull --no-pull` resolves to
/// no-pull.
///
/// Fails if the binding name already exists in any group.
#[derive(Parser, Clone)]
pub struct Add {
    /// Named group to add the bindings to. Defaults to the implicit
    /// `[tools]` table when omitted.
    #[arg(long = "group", short = 'g', value_name = "GROUP")]
    pub group: Option<String>,

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

    /// Fully-qualified tool identifiers to add (e.g. `ocx.sh/cmake:3.28`).
    #[arg(required = true, num_args = 1.., value_name = "IDENTIFIER")]
    pub identifiers: Vec<String>,
}

impl Add {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // F7: a `--global` mutator on an absent global file auto-creates
        // it (mirrors project `add` on a fresh project; the global tier is
        // the one sanctioned auto-scaffold site). No-op when not `--global`
        // or the file already exists.
        ensure_global_project_initialized(&context).await?;

        // Parse every identifier up front — before the flock — applying the
        // default registry if unqualified and the `:latest` default for bare
        // identifiers (no tag, no digest). Parsing all of them first means a
        // malformed identifier fails fast without touching the flock or
        // `ocx.toml`. The `:latest` default is intentionally NOT a duplicate
        // of the config-parse-layer default in `ProjectConfig::from_toml_str`.
        let identifiers: Vec<ocx_lib::oci::Identifier> = self
            .identifiers
            .iter()
            .map(|raw| {
                let id = ocx_lib::oci::Identifier::parse_with_default_registry(raw, context.default_registry())?;
                let id = if id.tag().is_none() && id.digest().is_none() {
                    id.clone_with_tag("latest")
                } else {
                    id
                };
                Ok::<_, anyhow::Error>(id)
            })
            .collect::<Result<_, _>>()?;

        // Resolve project, acquire flock, load snapshot + predecessor lock.
        // Errors propagate to the `main.rs` boundary: `log::error!` logs the
        // message once and `app::classify_error` derives the exit code from
        // `ProjectContextError`'s `ClassifyExitCode` impl.
        let guard = load_project_for_mutate(&context).await?;

        // Stage: in-memory add of every identifier against a clone of the
        // snapshot. A duplicate identifier — already bound, or repeated within
        // this batch — surfaces as `BindingAlreadyExists` inside the closure,
        // which aborts before any disk write. Atomic: all bindings land or
        // none do; `ocx.toml` is never left half-edited.
        let config_path = guard.config_path().to_path_buf();
        let identifiers_for_stage = identifiers.clone();
        let group = self.group.clone();
        let staged = guard.stage(move |cfg| {
            for identifier in &identifiers_for_stage {
                add_binding_in_memory(cfg, &config_path, identifier, group.as_deref())?;
            }
            Ok(())
        })?;

        // Whole-file model (spec §4.3): re-resolve ONLY the new bindings and
        // carry every pre-existing lock entry forward verbatim (V2
        // byte-identical; V1 via exact-only pinned-index transcribe). The
        // freshness gate inside `resolve_lock_touched` anchors on the
        // pre-mutation snapshot (`guard.config()`) — the inserted bindings make
        // the candidate hash differ, so anchoring on the candidate would fail
        // every clean add — and stamps the candidate hash into the produced
        // lock. Drift on the pre-mutation snapshot surfaces as
        // `StaleLockOnPartial` (65, run `ocx lock`); a carried V1 entry whose
        // index is gone surfaces as `LockUpgradeRequired` (78, run
        // `ocx upgrade`). Both propagate to the `main.rs` boundary.
        // `resolve_lock_touched` dedups the touched set internally.
        let group = self
            .group
            .clone()
            .unwrap_or_else(|| ocx_lib::project::DEFAULT_GROUP.to_string());
        let touched: Vec<(String, String)> = identifiers
            .iter()
            .map(|identifier| (group.clone(), ocx_lib::project::binding_key(identifier)))
            .collect();
        let new_lock = match guard.previous_lock().cloned() {
            Some(prev) => {
                resolve_lock_touched(
                    staged.config(), // candidate (post-mutation)
                    guard.config(),  // pre-mutation snapshot — freshness anchor
                    &prev,
                    context.default_index(),
                    &touched,
                    ResolveLockOptions::default(),
                )
                .await?
            }
            // Bootstrap: no predecessor to preserve, nothing to launder — a
            // direct resolve that must never fail closed.
            None => {
                resolve_lock(
                    staged.config(),
                    context.default_index(),
                    &[],
                    ResolveLockOptions::default(),
                )
                .await?
            }
        };

        // Commit: lock-first, manifest-second, both atomic.
        let commit = guard.commit(staged, new_lock.clone()).await?;
        let _ = commit;

        // Best-effort pull AFTER the commit lands. A failure here does
        // not roll back the manifest/lock — the binding is declaratively
        // present even if the pull needs a retry.
        //
        // Symbol-free by design: `materialize_lock` warms the object
        // store via `pull_all`, never `install_all`. The signed
        // handshake §1 contract — "global IS the project toolchain, only
        // difference is the load site" — combined with ADR D5
        // (amended 2026-05-19, env = lock-pinned digest, current symlink
        // demoted to IDE-anchor abstraction not consulted by env) means
        // neither tier needs a candidate or `current` symlink to make
        // the added tool resolvable. Users that want a per-repo stable
        // anchor invoke `ocx package install` / `ocx package select`
        // explicitly.
        //
        // `--no-pull` opts out: lock write happens regardless; only the
        // object-store materialization is deferred.
        let eager = !self.no_pull;
        materialize_lock(&context, &new_lock, eager, self.platforms.as_slice()).await?;

        // Report the full resulting lock to the user, keyed on the requested
        // platform when `--platform` was given (else the host).
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    // ── helpers ──────────────────────────────────────────────────────────────

    fn parse(args: &[&str]) -> Add {
        // Always supply the required IDENTIFIER positional argument.
        Add::try_parse_from(args).unwrap()
    }

    fn eager(add: &Add) -> bool {
        !add.no_pull
    }

    // ── cases ─────────────────────────────────────────────────────────────────

    /// Neither `--pull` nor `--no-pull` → both fields false; default is eager.
    #[test]
    fn parse_no_flags_defaults_to_eager() {
        let add = parse(&["add", "tool:1"]);
        assert!(!add.pull, "pull must be false when neither flag is given");
        assert!(!add.no_pull, "no_pull must be false when neither flag is given");
        assert!(eager(&add), "default must be eager (eager = !no_pull)");
    }

    /// `--pull` alone → pull=true, no_pull=false; still eager.
    #[test]
    fn parse_only_pull_is_eager() {
        let add = parse(&["add", "--pull", "tool:1"]);
        assert!(add.pull, "pull must be true with --pull");
        assert!(!add.no_pull, "no_pull must be false with --pull only");
        assert!(eager(&add), "eager must be true when --pull is the last flag");
    }

    /// `--no-pull` alone → pull=false, no_pull=true; lazy.
    #[test]
    fn parse_only_no_pull_is_lazy() {
        let add = parse(&["add", "--no-pull", "tool:1"]);
        assert!(!add.pull, "pull must be false with --no-pull only");
        assert!(add.no_pull, "no_pull must be true with --no-pull");
        assert!(!eager(&add), "eager must be false when --no-pull is set");
    }

    /// `--pull --no-pull` → POSIX last-wins: no_pull wins, pull=false; lazy.
    #[test]
    fn parse_pull_then_no_pull_no_pull_wins() {
        let add = parse(&["add", "--pull", "--no-pull", "tool:1"]);
        assert!(add.no_pull, "no_pull must be true when --no-pull follows --pull");
        assert!(!add.pull, "pull must be false when --no-pull overrides it");
        assert!(!eager(&add), "eager must be false when --no-pull wins");
    }

    /// `--no-pull --pull` → POSIX last-wins: pull wins, no_pull=false; eager.
    #[test]
    fn parse_no_pull_then_pull_pull_wins() {
        let add = parse(&["add", "--no-pull", "--pull", "tool:1"]);
        assert!(add.pull, "pull must be true when --pull follows --no-pull");
        assert!(!add.no_pull, "no_pull must be false when --pull overrides it");
        assert!(eager(&add), "eager must be true when --pull wins");
    }

    /// A single positional still parses (back-compat with the pre-plural form).
    #[test]
    fn parse_single_identifier() {
        let add = parse(&["add", "tool:1"]);
        assert_eq!(add.identifiers, vec!["tool:1".to_string()]);
    }

    /// Multiple positionals are all captured, in order.
    #[test]
    fn parse_multiple_identifiers() {
        let add = parse(&["add", "a:1", "b:2", "c:3"]);
        assert_eq!(
            add.identifiers,
            vec!["a:1".to_string(), "b:2".to_string(), "c:3".to_string()]
        );
    }

    /// `num_args=1..` rejects zero positionals.
    #[test]
    fn parse_zero_identifiers_is_error() {
        assert!(
            Add::try_parse_from(["add"]).is_err(),
            "add with no identifier must fail"
        );
    }

    /// `--platform` is repeatable and parses alongside the positional identifier.
    #[test]
    fn parses_repeatable_platform_flag() {
        let add = Add::try_parse_from(["add", "--platform", "linux/arm64", "-p", "linux/amd64", "cmake:3.28"]).unwrap();
        assert_eq!(
            add.platforms.as_slice().len(),
            2,
            "two --platform values must parse into two entries"
        );
        assert_eq!(
            add.identifiers,
            vec!["cmake:3.28".to_owned()],
            "--platform must not swallow the identifier"
        );
    }
}
