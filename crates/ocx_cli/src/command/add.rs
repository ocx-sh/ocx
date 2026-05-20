// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx add [--group <name>] <identifier>` — append a binding to
//! `ocx.toml`, atomically rewrite `ocx.lock` for impacted tools, and
//! install.

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::project::error::ProjectErrorKind;
use ocx_lib::project::{ResolveLockOptions, add_binding_in_memory, resolve_lock, resolve_lock_partial};

use crate::api::data::lock::{LockEntry, LockReport};
use crate::app::project_context::{ensure_global_project_initialized, load_project_for_mutate, materialize_lock};

/// Add a tool binding to `ocx.toml`.
///
/// Appends the given identifier to the implicit default `[tools]` table,
/// or to a named `[group.<name>]` table when `--group` is supplied.
/// After updating `ocx.toml`, rewrites `ocx.lock` for the affected
/// group and installs the tool (default eager behavior).
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
    /// Named group to add the binding to. Defaults to the implicit
    /// `[tools]` table when omitted.
    #[arg(long = "group", short = 'g', value_name = "GROUP")]
    pub group: Option<String>,

    /// Materialize packages into the object store after writing the lock (default).
    ///
    /// `--pull` is the affirmative form of the default behavior; `--no-pull`
    /// opts out. Both flags use POSIX last-wins semantics (`overrides_with`):
    /// `--pull --no-pull` resolves to no-pull; `--no-pull --pull` resolves
    /// to pull. Combining the flags is not an error — git `--[no-]verify`
    /// idiom.
    #[arg(long, overrides_with = "no_pull")]
    pub pull: bool,

    /// Write the lock without downloading. Materialization is deferred to
    /// `ocx pull` or first `ocx run` / direnv hit. Useful for CI flows that
    /// batch lock changes and materialize separately.
    #[arg(long = "no-pull", overrides_with = "pull")]
    pub no_pull: bool,

    /// Fully-qualified tool identifier to add (e.g. `ocx.sh/cmake:3.28`).
    #[arg(value_name = "IDENTIFIER")]
    pub identifier: String,
}

impl Add {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // F7: a `--global` mutator on an absent global file auto-creates
        // it (mirrors project `add` on a fresh project; the global tier is
        // the one sanctioned auto-scaffold site). No-op when not `--global`
        // or the file already exists.
        ensure_global_project_initialized(&context).await?;

        // Parse the identifier, applying the default registry if unqualified.
        let identifier =
            ocx_lib::oci::Identifier::parse_with_default_registry(&self.identifier, context.default_registry())?;

        // Apply :latest default for bare identifiers (no tag, no digest).
        // See history of this file for the design rationale (intentional NOT a
        // duplicate of the config-parse-layer default in
        // `ProjectConfig::from_toml_str`).
        let identifier = if identifier.tag().is_none() && identifier.digest().is_none() {
            identifier.clone_with_tag("latest")
        } else {
            identifier
        };

        // Resolve project, acquire flock, load snapshot + predecessor lock.
        // Errors propagate to the `main.rs` boundary: `log::error!` logs the
        // message once and `app::classify_error` derives the exit code from
        // `ProjectContextError`'s `ClassifyExitCode` impl.
        let guard = load_project_for_mutate(&context).await?;

        // Stage: in-memory add against a clone of the snapshot.
        let config_path = guard.config_path().to_path_buf();
        let identifier_for_stage = identifier.clone();
        let group = self.group.clone();
        let staged = guard.stage(move |cfg| {
            add_binding_in_memory(cfg, &config_path, &identifier_for_stage, group.as_deref())?;
            Ok(())
        })?;

        // Resolve the new lock against the candidate config. When a
        // predecessor exists, try the partial resolve first (cheaper —
        // touches only the new binding). The candidate's
        // `declaration_hash` necessarily differs from the predecessor's
        // (we just inserted a binding), so `resolve_lock_partial` will
        // refuse with [`ProjectErrorKind::StaleLockOnPartial`]. Catch
        // that signal and fall back to a full `resolve_lock` per the
        // Cluster A plan: the partial path's hash gate prevents
        // laundering, so on mismatch we rebuild from scratch.
        let binding_name = ocx_lib::project::binding_key(&identifier);
        let new_lock = if let Some(prev) = guard.previous_lock().cloned() {
            match resolve_lock_partial(
                staged.config(),
                &prev,
                context.default_index(),
                &[binding_name],
                &[],
                ResolveLockOptions::default(),
            )
            .await
            {
                Ok(lock) => lock,
                Err(ocx_lib::project::Error::Project(pe))
                    if matches!(pe.kind, ProjectErrorKind::StaleLockOnPartial { .. }) =>
                {
                    // Hash drifted — predecessor was computed before our
                    // in-memory mutation. Full re-resolve rebuilds the
                    // lock from the candidate config without laundering.
                    resolve_lock(
                        staged.config(),
                        context.default_index(),
                        &[],
                        ResolveLockOptions::default(),
                    )
                    .await?
                }
                Err(other) => return Err(other.into()),
            }
        } else {
            resolve_lock(
                staged.config(),
                context.default_index(),
                &[],
                ResolveLockOptions::default(),
            )
            .await?
        };

        // Commit: lock-first, manifest-second, both atomic.
        let commit = guard.commit(staged, new_lock.clone()).await?;
        let _ = commit;

        // Best-effort install AFTER the commit lands. A failure here
        // does not roll back the manifest/lock — the binding is
        // declaratively present even if the install retry is needed.
        //
        // In the **global tier only**, also set the `current` selection
        // for the added tool. The offline login exporter
        // (`ocx --global env`, ADR `adr_global_toolchain_tier.md` D5)
        // resolves the global toolchain through `find_symlink(Current)`,
        // so without this the tool stays invisible until a manual
        // `ocx package select`. The signed handshake §1 contract is
        // "global IS the project toolchain — the only difference is the
        // load site"; project-tier `add` needs no separate select, so
        // global-tier `add` must not either. This reuses the exact
        // `wire_selection` path `ocx package select` uses (the `select`
        // flag on `install_all`) — no hand-rolled symlink writes.
        // Project tier (no `--global`) keeps `select=false`: project
        // resolution goes through the lock, never `current`.
        //
        // `--no-pull` opts out: lock write happens regardless; only the
        // object-store materialization is deferred.
        let eager = !self.no_pull;
        materialize_lock(&context, &new_lock, eager).await?;

        // Report the full resulting lock to the user.
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
}
