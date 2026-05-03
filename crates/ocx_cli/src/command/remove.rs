// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx remove <identifier>` — drop a binding from `ocx.toml`, rewrite
//! `ocx.lock` for the affected group, and uninstall the tool.

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::project::{ProjectConfig, ProjectLock, ResolveLockOptions, resolve_lock};

use crate::api::data::lock::{LockEntry, LockReport};

/// Remove a tool binding from `ocx.toml`.
///
/// Searches the implicit default `[tools]` table and all named groups for
/// a binding whose key or identifier matches the given identifier, removes
/// it, rewrites `ocx.lock` for the affected group, and uninstalls the tool.
///
/// Fails if no matching binding is found in any group.
#[derive(Parser, Clone)]
pub struct Remove {
    /// Identifier of the tool to remove (binding name or fully-qualified
    /// identifier, e.g. `cmake` or `ocx.sh/cmake:3.28`).
    #[arg(value_name = "IDENTIFIER")]
    pub identifier: String,
}

impl Remove {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // Resolve `ocx.toml` + `ocx.lock` paths via the project resolver.
        let cwd = ocx_lib::env::current_dir()?;
        let home = context.file_structure().root().to_path_buf();
        let resolved = ProjectConfig::resolve(Some(&cwd), context.project_path(), Some(&home)).await?;
        let (config_path, lock_path) = match resolved {
            Some(pair) => pair,
            None => {
                eprintln!(
                    "no ocx.toml found in {} or any parent; run `ocx init` to create one",
                    cwd.display()
                );
                return Ok(ocx_lib::cli::ExitCode::UsageError.into());
            }
        };

        let project_root = config_path.parent().unwrap_or(&cwd).to_path_buf();

        // Load current config to find the full identifier for the binding
        // (needed for uninstall).  The identifier arg may be a bare binding
        // name (e.g. `cmake`) or a fully-qualified string — derive the
        // lookup key (repo basename) either way.
        let config = ProjectConfig::from_path(&config_path).await?;

        // Derive the binding key from the user-supplied string.
        // When the user provides a fully-qualified identifier, parse it and
        // delegate to the canonical mutate::binding_key derivation.  When the
        // user supplies a bare binding name (no registry, no tag), treat the
        // whole string as the key after stripping any trailing `:tag` suffix.
        let binding_key = {
            let raw = &self.identifier;
            if raw.contains('/') {
                // Looks like a qualified identifier — parse and extract basename
                // via the same function add_binding uses, so both sides agree.
                match ocx_lib::oci::Identifier::parse_with_default_registry(raw, context.default_registry()) {
                    Ok(id) => ocx_lib::project::binding_key(&id),
                    // Unparseable: fall back to raw last-segment extraction.
                    Err(_) => raw.rsplit('/').next().unwrap_or(raw).to_owned(),
                }
            } else {
                // Bare name (possibly with `:tag` suffix) — strip the tag.
                raw.split_once(':')
                    .map(|(k, _)| k.to_owned())
                    .unwrap_or_else(|| raw.clone())
            }
        };

        // Look up the full identifier so we can uninstall it after removing
        // the config entry.
        let install_identifier = config
            .tools
            .get(&binding_key)
            .or_else(|| config.groups.values().find_map(|g| g.get(&binding_key)))
            .cloned();

        // Build a synthetic identifier for remove_binding; the binding key
        // is repo-basename based, so construct one with repo = key and a
        // placeholder registry.
        let dummy_id =
            ocx_lib::oci::Identifier::parse_with_default_registry(&self.identifier, context.default_registry());

        // Use the actual identifier from the config when available; fall back
        // to parsing the user's string.  remove_binding only needs the key
        // (repo basename), so either approach is correct.
        let remove_id = match &install_identifier {
            Some(id) => id.clone(),
            None => match dummy_id {
                Ok(id) => id,
                Err(_) => {
                    // Not a parseable identifier — treat as a bare binding key
                    // by constructing a minimal identifier.
                    ocx_lib::oci::Identifier::new_registry(&binding_key, context.default_registry())
                }
            },
        };

        // Mutate ocx.toml: drop the binding.  Errors on BindingNotFound.
        ocx_lib::project::remove_binding(&project_root, &remove_id)?;

        // Re-load the config without the removed entry.
        let config_after = ProjectConfig::from_path(&config_path).await?;

        // Acquire exclusive lock and load existing ocx.lock.
        let (previous, _guard) = ProjectLock::load_exclusive(&lock_path).await?;

        // Full atomic rewrite of ocx.lock from the updated config (research §6.3:
        // add/remove always write a complete fresh lockfile).
        let lock = resolve_lock(
            &config_after,
            context.default_index(),
            &[],
            ResolveLockOptions::default(),
        )
        .await?;

        lock.save(&lock_path, previous.as_ref(), &home).await?;

        // Uninstall the package if we found its installed identifier.
        if let Some(ref ident) = install_identifier {
            // Ignore errors from uninstall — the tool may not be installed
            // (e.g. lock-only workflow), and removing it from config + lock
            // is the primary contract.
            let _ = context
                .manager()
                .uninstall_all(std::slice::from_ref(ident), false, false)
                .await;
        }

        // Report the resulting lock (tools that remain after remove).
        let entries: Vec<LockEntry> = lock
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
