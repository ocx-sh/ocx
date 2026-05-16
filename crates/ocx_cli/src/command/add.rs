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
use crate::app::project_context::{ensure_global_project_initialized, load_project_for_mutate};
use crate::conventions::platforms_or_default;

/// Add a tool binding to `ocx.toml`.
///
/// Appends the given identifier to the implicit default `[tools]` table,
/// or to a named `[group.<name>]` table when `--group` is supplied.
/// After updating `ocx.toml`, rewrites `ocx.lock` for the affected
/// group and installs the tool.
///
/// Fails if the binding name already exists in any group.
#[derive(Parser, Clone)]
pub struct Add {
    /// Named group to add the binding to. Defaults to the implicit
    /// `[tools]` table when omitted.
    #[arg(long = "group", short = 'g', value_name = "GROUP")]
    pub group: Option<String>,

    /// Operate on the global toolchain (`$OCX_HOME/ocx.toml`) instead of
    /// a discovered project. Auto-creates the global file when absent
    /// (mirrors project `add` on a fresh project). Mutually exclusive
    /// with `--project`.
    #[arg(long)]
    pub global: bool,

    /// Fully-qualified tool identifier to add (e.g. `ocx.sh/cmake:3.28`).
    #[arg(value_name = "IDENTIFIER")]
    pub identifier: String,
}

impl Add {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // Fold a post-subcommand `--global` into the resolution view so
        // the project-tier prologue selects `$OCX_HOME/ocx.toml`.
        let context = context.with_command_global(self.global)?;

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

        // Best-effort install AFTER the commit lands. A failure here
        // does not roll back the manifest/lock — the binding is
        // declaratively present even if the install retry is needed.
        //
        // In the **global tier only**, also set the `current` selection
        // for the added tool. The offline login exporter
        // (`ocx env --global`, ADR `adr_global_toolchain_tier.md` D5)
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
        context
            .manager()
            .install_all(
                vec![identifier],
                platforms_or_default(&[]),
                true,
                self.global,
                context.concurrency(),
            )
            .await?;

        let _ = commit;

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
