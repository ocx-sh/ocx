// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::project::{
    DEFAULT_GROUP, ProjectConfig, ProjectLock, compose_tool_set, declaration_hash, parse_positional,
};
use ocx_lib::{cli, env, oci};

use crate::conventions::*;

/// Runs a command with a composed tool environment.
///
/// Composes the tool set from any selected `--group` entries (resolved via
/// the digest-pinned `ocx.lock`) and any explicit positional packages
/// (resolved via the index, tag-style). Right-most positional binding wins
/// over both group entries and earlier positionals.
#[derive(Parser)]
pub struct Exec {
    /// Run in interactive mode, which keeps the environment variables set after the command finishes.
    ///
    /// Useful for shells that support it (PowerShell, Elvish). For other shells, this flag is ignored.
    #[clap(short = 'i', long = "interactive", default_value_t = false)]
    interactive: bool,

    /// Start with a clean environment containing only the package variables, instead of inheriting the current shell environment.
    #[clap(long = "clean", default_value_t = false)]
    clean: bool,

    /// Target platforms to consider when resolving packages. If not specified, only supported platforms will be considered.
    #[clap(short = 'p', long = "platform", value_delimiter = ',', value_name = "PLATFORM", num_args = 0..)]
    platforms: Vec<oci::Platform>,

    /// Select tool group(s) from the project `ocx.lock`.
    ///
    /// Repeatable and comma-separated: `-g ci,lint -g release`. The reserved
    /// name `default` selects the top-level `[tools]` table. Requires an
    /// `ocx.toml` in scope.
    #[clap(short = 'g', long = "group", value_delimiter = ',', value_name = "NAME")]
    groups: Vec<String>,

    /// Positional package overrides of the form `[name=]identifier`.
    ///
    /// When `name=` is omitted, the binding name is inferred from the
    /// identifier's repository basename (`cmake:3.29` → `cmake`,
    /// `ghcr.io/acme/foo:1` → `foo`). Right-most wins over group entries
    /// sharing the same binding name.
    #[clap(value_terminator = "--", num_args = 0..)]
    packages: Vec<String>,

    /// Command to execute, with arguments. The command runs with the composed package environment.
    #[clap(allow_hyphen_values = true, num_args = 1..)]
    command: Vec<String>,
}

impl Exec {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // ── Phase 1: parse-time validation (rules in plan §4) ────────────

        // Rule 4: empty segments in `-g` lists (`-g ci,,lint`) → exit 64.
        for raw in &self.groups {
            if raw.is_empty() {
                eprintln!("empty group segment in --group value; check for stray commas");
                return Ok(cli::ExitCode::UsageError.into());
            }
        }

        // Rule 3: at least one identifier source must be provided. The
        // command vec is the post-`--` payload; an empty `packages` AND an
        // empty `groups` means no tools were requested.
        if self.groups.is_empty() && self.packages.is_empty() {
            eprintln!("no packages or groups specified");
            return Ok(cli::ExitCode::UsageError.into());
        }

        // ── Phase 2: project resolution (only consulted when groups used) ─
        //
        // When the user supplied only positionals, the command stays
        // hermetic — no `ocx.toml` walk, no lock load. Matches today's
        // exec behaviour for tag-style invocations (parity guarantee from
        // plan §4 acceptance tests).
        let cwd = env::current_dir()?;
        let project = if self.groups.is_empty() {
            None
        } else {
            let home = context.file_structure().root().to_path_buf();
            ProjectConfig::resolve(Some(&cwd), context.project_path(), Some(&home)).await?
        };

        // Rule 2: `--group` requires an ocx.toml. The pre-validation above
        // already rejected the no-ocx.toml case for groups via the resolver;
        // this branch surfaces the explicit error.
        let (config_path, lock_path) = match (&self.groups[..], project) {
            ([], _) => (None, None),
            (_groups, Some((cfg, lock))) => (Some(cfg), Some(lock)),
            (_groups, None) => {
                eprintln!("--group requires an ocx.toml in the working directory or its parents");
                return Ok(cli::ExitCode::UsageError.into());
            }
        };

        // Load the project config so we can validate group names. The
        // load also enforces the `[group.default]` reserved-name rule
        // (Phase 2), so a malformed project surfaces a parse error here
        // before any network work.
        let config = if let Some(p) = &config_path {
            Some(ProjectConfig::from_path(p).await?)
        } else {
            // Synthetic empty config for positional-only invocations. The
            // composer accepts `&ProjectConfig`; we don't want to make it
            // optional just for this code path.
            None
        };

        // Rule 1: every requested group must exist in the project (or be
        // the reserved `default`). Validated against the loaded config —
        // unknown names produce exit 64 with a clear message.
        if let Some(cfg) = &config {
            for raw in &self.groups {
                if raw == DEFAULT_GROUP {
                    continue;
                }
                if !cfg.groups.contains_key(raw) {
                    eprintln!("group '{raw}' not found in ocx.toml");
                    return Ok(cli::ExitCode::UsageError.into());
                }
            }
        }

        // ── Phase 3: lock load + staleness gate (only when groups used) ──

        let lock = if let Some(path) = &lock_path {
            // Open without holding an advisory lock — exec is read-only on
            // ocx.lock; only `ocx lock` writes it.
            match ProjectLock::from_path(path).await? {
                Some(l) => Some(l),
                None => {
                    // Plan §4 step 4: missing lock when groups are selected
                    // → ConfigError (exit 78). Message points at the fix.
                    eprintln!("ocx.lock not found at {}; run `ocx lock` to create it", path.display());
                    return Ok(cli::ExitCode::ConfigError.into());
                }
            }
        } else {
            None
        };

        // Staleness gate: declaration_hash on the lock must match the
        // current config. A mismatch means `ocx.toml` changed since the
        // lock was written → DataError (exit 65).
        if let (Some(cfg), Some(l)) = (&config, &lock) {
            let current = declaration_hash(cfg);
            if l.metadata.declaration_hash != current {
                eprintln!("ocx.lock is stale (ocx.toml changed since last `ocx lock`); run `ocx lock`");
                return Ok(cli::ExitCode::DataError.into());
            }
        }

        // ── Phase 4: parse positional packages ───────────────────────────

        let positionals: Vec<_> = self
            .packages
            .iter()
            .map(|raw| parse_positional(raw, context.default_registry()))
            .collect::<Result<Vec<_>, _>>()?;

        // ── Phase 5: compose the resolved tool set ───────────────────────
        //
        // The composer is pure — all I/O is finished by here. It applies
        // dedup-then-error-or-collapse for groups, then right-most-wins
        // for positionals.
        //
        // For the synthetic empty-config code path we still need a
        // `ProjectConfig` to satisfy the signature; build one on-the-fly
        // — it's never inspected by the composer in the positional-only
        // case (no groups → no group lookups).
        let synthetic_config;
        let cfg_ref: &ProjectConfig = match &config {
            Some(c) => c,
            None => {
                synthetic_config = ProjectConfig {
                    tools: Default::default(),
                    groups: Default::default(),
                };
                &synthetic_config
            }
        };
        let resolved = compose_tool_set(cfg_ref, lock.as_ref(), &self.groups, &positionals)?;

        // ── Phase 6: pull + env composition ──────────────────────────────

        let identifiers: Vec<oci::Identifier> = resolved.iter().map(|r| r.identifier.clone()).collect();
        let platforms = platforms_or_default(&self.platforms);
        let manager = context.manager();
        let info = manager.pull_all(&identifiers, platforms, context.concurrency()).await?;

        let entries = manager.resolve_env(&info).await?;
        let mut process_env = if self.clean { env::Env::clean() } else { env::Env::new() };
        process_env.apply_entries(&entries);

        // ── Phase 7: spawn the user command ──────────────────────────────

        use std::process::Stdio;
        use tokio::process::Command;

        let Some((command, args)) = self.command.split_first() else {
            return Err(anyhow::anyhow!("No command provided to execute."));
        };

        let resolved_cmd = process_env.resolve_command(command);

        let mut child_process = Command::new(&resolved_cmd)
            .args(args)
            .stdin(if self.interactive {
                Stdio::inherit()
            } else {
                Stdio::null()
            })
            .stderr(Stdio::inherit())
            .stdout(Stdio::inherit())
            .envs(process_env)
            .spawn()?;

        let status = child_process.wait().await?;
        if !status.success() {
            match status.code() {
                Some(code) => return Ok(ExitCode::from(code as u8)),
                None => return Ok(ExitCode::FAILURE),
            }
        }
        Ok(ExitCode::SUCCESS)
    }
}
