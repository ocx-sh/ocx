// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx remove <identifier>` — drop a binding from `ocx.toml`, rewrite
//! `ocx.lock` for the affected group, and uninstall the tool.

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::cli;
use ocx_lib::project::{ResolveLockOptions, remove_binding_in_memory, resolve_lock};

use crate::api::data::lock::{LockEntry, LockReport};
use crate::app::project_context::{ProjectContextError, load_project_for_mutate};

/// Remove a tool binding from `ocx.toml`.
///
/// Searches the implicit default `[tools]` table and all named groups for
/// a binding whose key or identifier matches the given identifier, removes
/// it, rewrites `ocx.lock` for the affected group, and uninstalls the tool.
///
/// When the same binding name exists in multiple groups, use `--group` to
/// target a specific group. `--group default` targets the implicit
/// `[tools]` table; `--group <name>` targets a named `[group.<name>]` table.
///
/// Fails if no matching binding is found in the targeted group (or any
/// group when `--group` is absent).
#[derive(Parser, Clone)]
pub struct Remove {
    /// Identifier of the tool to remove (binding name or fully-qualified
    /// identifier, e.g. `cmake` or `ocx.sh/cmake:3.28`).
    #[arg(value_name = "IDENTIFIER")]
    pub identifier: String,

    /// Target a specific group. Use `default` to target the implicit
    /// `[tools]` table, or a named group (e.g. `ci`) to target
    /// `[group.ci]`. Without this flag, all groups are searched; if the
    /// binding appears in more than one group an error is returned.
    #[arg(long = "group", short = 'g', value_name = "NAME")]
    pub group: Option<String>,
}

impl Remove {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // Validate the group name early — before flock acquisition — so a
        // typo doesn't lock out other writers waiting on `ocx.toml`.
        if let Some(ref g) = self.group
            && g != "default"
        {
            let valid = !g.is_empty() && g.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_');
            if !valid {
                return Err(anyhow::anyhow!(
                    "invalid group name '{}': must be non-empty and contain only alphanumeric characters, '-', or '_'",
                    g
                ));
            }
        }

        // Acquire flock + load snapshot + predecessor.
        let guard = match load_project_for_mutate(&context).await {
            Ok(g) => g,
            Err(ProjectContextError::NoProject { cwd }) => {
                eprintln!(
                    "no ocx.toml found in {} or any parent; run `ocx init` to create one",
                    cwd.display()
                );
                return Ok(cli::ExitCode::UsageError.into());
            }
            Err(other) => return Err(other.into()),
        };

        // Derive the binding key + look up the live identifier from the
        // pre-mutation snapshot so we can uninstall it after the commit.
        let binding_key = {
            let raw = &self.identifier;
            if raw.contains('/') {
                match ocx_lib::oci::Identifier::parse_with_default_registry(raw, context.default_registry()) {
                    Ok(id) => ocx_lib::project::binding_key(&id),
                    Err(_) => raw.rsplit('/').next().unwrap_or(raw).to_owned(),
                }
            } else {
                raw.split_once(':')
                    .map(|(k, _)| k.to_owned())
                    .unwrap_or_else(|| raw.clone())
            }
        };

        let install_identifier = match self.group.as_deref() {
            Some("default") => guard.config().tools.get(&binding_key).cloned(),
            Some(g) => guard
                .config()
                .groups
                .get(g)
                .and_then(|grp| grp.get(&binding_key))
                .cloned(),
            None => guard
                .config()
                .tools
                .get(&binding_key)
                .or_else(|| guard.config().groups.values().find_map(|g| g.get(&binding_key)))
                .cloned(),
        };

        // Build the synthetic identifier the in-memory remover requires.
        // remove_binding_in_memory only consults the binding key (repo
        // basename), so either the live identifier or a parse of the
        // user-supplied string suffices.
        let dummy_id =
            ocx_lib::oci::Identifier::parse_with_default_registry(&self.identifier, context.default_registry());
        let remove_id = match &install_identifier {
            Some(id) => id.clone(),
            None => match dummy_id {
                Ok(id) => id,
                Err(_) => ocx_lib::oci::Identifier::new_registry(&binding_key, context.default_registry()),
            },
        };

        // Stage: in-memory remove on a clone of the snapshot.
        let config_path = guard.config_path().to_path_buf();
        let group = self.group.clone();
        let staged = guard.stage(move |cfg| {
            remove_binding_in_memory(cfg, &config_path, &remove_id, group.as_deref())?;
            Ok(())
        })?;

        // Full atomic rewrite of ocx.lock from the post-mutation config
        // (research §6.3: add/remove always write a complete fresh
        // lockfile).
        let new_lock = resolve_lock(
            staged.config(),
            context.default_index(),
            &[],
            ResolveLockOptions::default(),
        )
        .await?;

        let commit = guard.commit(staged, new_lock.clone()).await?;
        let _ = commit;

        // Best-effort uninstall after commit. The tool may not be
        // installed (lock-only workflow); errors here do not roll back.
        if let Some(ref ident) = install_identifier {
            let _ = context
                .manager()
                .uninstall_all(std::slice::from_ref(ident), false, false)
                .await;
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
