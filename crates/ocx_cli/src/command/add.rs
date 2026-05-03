// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx add [--group <name>] <identifier>` — append a binding to
//! `ocx.toml`, atomically rewrite `ocx.lock` for impacted tools, and
//! install.

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::project::{ProjectConfig, ProjectLock, ResolveLockOptions, resolve_lock, resolve_lock_partial};

use crate::api::data::lock::{LockEntry, LockReport};
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

    /// Fully-qualified tool identifier to add (e.g. `ocx.sh/cmake:3.28`).
    #[arg(value_name = "IDENTIFIER")]
    pub identifier: String,
}

impl Add {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // Parse the identifier, applying the default registry if unqualified.
        let identifier =
            ocx_lib::oci::Identifier::parse_with_default_registry(&self.identifier, context.default_registry())?;

        // Apply :latest default for bare identifiers (no tag, no digest).
        //
        // This injection is intentional and NOT a duplicate of the config-parse-
        // layer default in config.rs (`parse_tool_map`). The config layer injects
        // `:latest` at *read* time as a convenience; `ocx add` must inject at
        // *write* time so the TOML stored on disk is explicit and human-readable
        // without relying on round-trip normalization. A file containing
        // `cmake = "ocx.sh/cmake"` would read as `:latest` today but could
        // confuse readers or tools that inspect the raw TOML. Writing
        // `cmake = "ocx.sh/cmake:latest"` makes the intent unambiguous.
        //
        // Reference: Unit 3 commit 7b8d7f2a and design rationale in
        // `crates/ocx_lib/src/project/config.rs::parse_tool_map`.
        let identifier = if identifier.tag().is_none() && identifier.digest().is_none() {
            identifier.clone_with_tag("latest")
        } else {
            identifier
        };

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

        // Mutate ocx.toml: append the new binding.
        ocx_lib::project::add_binding(&project_root, &identifier, self.group.as_deref())?;

        // Re-load the config after mutation so the resolver sees the new entry.
        let config = ProjectConfig::from_path(&config_path).await?;

        // Binding key = repo basename — must match what add_binding wrote.
        // Reuse the canonical derivation from mutate::binding_key (promoted to
        // pub) rather than duplicating the rsplit logic here.
        let binding_name = ocx_lib::project::binding_key(&identifier);

        // Acquire exclusive lock and load existing ocx.lock (may not exist yet).
        let (previous, _guard) = ProjectLock::load_exclusive(&lock_path).await?;

        // Full re-resolve of all tools (atomic full-lockfile rewrite per
        // research §3 + §6.3).  When a previous lock exists, use
        // resolve_lock_partial so existing entries are preserved without a
        // full network round-trip; when no lock exists yet, do a full resolve
        // to bootstrap.
        let lock = if let Some(prev) = previous.as_ref() {
            resolve_lock_partial(
                &config,
                prev,
                context.default_index(),
                &[binding_name],
                &[],
                ResolveLockOptions::default(),
            )
            .await?
        } else {
            resolve_lock(&config, context.default_index(), &[], ResolveLockOptions::default()).await?
        };

        lock.save(&lock_path, previous.as_ref(), &home).await?;

        // Install the newly added tool.
        context
            .manager()
            .install_all(
                vec![identifier],
                platforms_or_default(&[]),
                true,
                false,
                context.concurrency(),
            )
            .await?;

        // Report the full resulting lock to the user.
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
