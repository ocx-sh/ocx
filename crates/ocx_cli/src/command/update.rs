// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::project::{
    DEFAULT_GROUP, ProjectConfig, ProjectLock, ResolveLockOptions, resolve_lock, resolve_lock_partial,
};

use crate::api::data::lock::{LockEntry, LockReport};

/// Re-resolve advisory tags for one or more tools and rewrite `ocx.lock`.
///
/// Opt-in mutation: never triggered automatically. With no arguments,
/// re-resolves every tool in `ocx.toml` (equivalent to `ocx lock` from
/// scratch). With positional binding names or `--group` filters,
/// re-resolves only the matching subset and preserves every other entry
/// already present in `ocx.lock`. Fully transactional — on any
/// resolution failure nothing is written.
#[derive(Parser, Clone)]
pub struct Update {
    /// Restrict re-resolution to the named group(s).
    ///
    /// Repeatable and comma-separated: `-g ci,lint -g release`. The
    /// reserved name `default` selects the top-level `[tools]` table.
    /// Combinable with positional binding names (intersection).
    #[arg(short = 'g', long = "group", value_delimiter = ',')]
    pub groups: Vec<String>,

    /// Binding names from `ocx.toml` to re-resolve.
    ///
    /// Each value is the local TOML key (e.g. `cmake`, not
    /// `ocx.sh/cmake:3.28`). Names not declared in `ocx.toml` produce
    /// `NotFound` (79) and no lock write.
    #[arg(value_name = "BINDING")]
    pub packages: Vec<String>,
}

impl Update {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // Pre-validate empty `--group` segments before any I/O. clap's
        // `value_delimiter = ','` splits `-g ci,,lint` into `["ci", "",
        // "lint"]`; an empty entry is a user-typing error.
        for raw in &self.groups {
            if raw.is_empty() {
                eprintln!("empty group segment in --group value; check for stray commas");
                return Ok(ocx_lib::cli::ExitCode::UsageError.into());
            }
        }

        // Resolve `ocx.toml` + sibling `ocx.lock` paths via the project
        // resolver (explicit flag > env > CWD walk > home fallback).
        let cwd = ocx_lib::env::current_dir()?;
        let home = context.file_structure().root().to_path_buf();
        let resolved = ProjectConfig::resolve(Some(&cwd), context.project_path(), Some(&home)).await?;
        let (config_path, lock_path) = match resolved {
            Some(pair) => pair,
            None => {
                eprintln!(
                    "no ocx.toml found in {} or any parent; run `ocx update` from a project directory",
                    cwd.display()
                );
                return Ok(ocx_lib::cli::ExitCode::UsageError.into());
            }
        };

        // Load the config so we can validate filters before any network
        // work and feed it into the resolver.
        let config = ProjectConfig::from_path(&config_path).await?;

        // Validate `--group` names against the loaded config (parity with
        // `ocx lock`).
        for raw in &self.groups {
            if raw == DEFAULT_GROUP {
                continue;
            }
            if !config.groups.contains_key(raw) {
                eprintln!("unknown group '{raw}' in --group filter");
                return Ok(ocx_lib::cli::ExitCode::UsageError.into());
            }
        }

        // Validate every positional binding exists in the config —
        // either in the top-level `[tools]` table or in some
        // `[group.*]` table. Unknown bindings exit with `NotFound` (79)
        // before any I/O, mirroring the spec bullet for `ocx update
        // <unknown>`.
        for name in &self.packages {
            let in_default = config.tools.contains_key(name);
            let in_groups = config.groups.values().any(|tools| tools.contains_key(name));
            if !in_default && !in_groups {
                eprintln!("tool '{name}' not declared in ocx.toml");
                return Ok(ocx_lib::cli::ExitCode::NotFound.into());
            }
        }

        // Acquire the exclusive sidecar lock before reading the existing
        // lock file. `_guard` lives until the end of the function so
        // concurrent writers cannot interleave with our save below.
        let (previous, _guard) = ProjectLock::load_exclusive(&lock_path).await?;

        // Two paths:
        //  - no bindings AND no groups → full re-resolution; same
        //    semantics as `ocx lock` from scratch (a missing
        //    `ocx.lock` is fine, the resolver writes a fresh one).
        //  - bindings or groups present → partial update; merge with
        //    the existing lock. A missing `ocx.lock` is a usage error
        //    here — there is nothing to merge into, so point the user
        //    at `ocx lock` to bootstrap.
        let lock = if self.packages.is_empty() && self.groups.is_empty() {
            resolve_lock(&config, context.default_index(), &[], ResolveLockOptions::default()).await?
        } else {
            let prev = match previous.as_ref() {
                Some(p) => p,
                None => {
                    eprintln!(
                        "ocx.lock not found at {}; run `ocx lock` to create it before updating a subset",
                        lock_path.display()
                    );
                    return Ok(ocx_lib::cli::ExitCode::ConfigError.into());
                }
            };
            resolve_lock_partial(
                &config,
                prev,
                context.default_index(),
                &self.packages,
                &self.groups,
                ResolveLockOptions::default(),
            )
            .await?
        };

        // Atomic save with `previous` as the eq_content baseline so
        // `generated_at` is preserved when the resolved content matches
        // the prior lock byte-for-byte (same invariant `ocx lock`
        // honors).
        lock.save(&lock_path, previous.as_ref()).await?;

        // Build the success report from the actual saved lock — the
        // user wants to see the full resulting tool set, not just the
        // entries that were re-resolved.
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
