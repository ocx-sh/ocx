// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::io::IsTerminal;
use std::process::ExitCode;

use crate::{api, conventions::*, options};
use clap::Parser;
use ocx_lib::shell::Shell;

/// Print the resolved environment variables for one or more installed packages.
///
/// Plain format: aligned table with Key, Value, and Type columns where Type is `constant` or `path`.
/// JSON format:  `{"entries": [{"key": "...", "value": "...", "type": "constant"|"path"}, ...]}`.
///
/// This allows external tools (Python scripts, Bazel rules, CI steps) to correctly
/// configure child process environments without going through `ocx exec`.
///
/// By default, env values are rooted in the content-addressed object store and
/// may change when a package is updated.  Use `--candidate` or `--current` to
/// root them in a stable symlink path instead — suitable for embedding in editor
/// or IDE configuration files that should not change on every package update.
/// See the path resolution modes documentation for details.
#[derive(Parser)]
pub struct Env {
    /// Expose the package's full env, including private (self-only) entries.
    /// See `ocx exec --help` for full view semantics.
    ///
    /// Generated launchers embed `--self`; avoid passing it directly unless
    /// building a launcher equivalent.
    #[clap(long = "self", default_value_t = false)]
    self_view: bool,

    #[clap(flatten)]
    platforms: options::Platforms,

    #[clap(flatten)]
    content_path: options::ContentPath,

    /// Package identifiers to resolve the environment for.
    #[clap(required = true, num_args = 1.., value_name = "PACKAGE")]
    packages: Vec<options::Identifier>,

    /// Target shell for eval-safe export lines.
    ///
    /// Must be supplied with `=` (`--shell=bash`).  Bare `--shell` (no `=`)
    /// triggers autodetection from `$SHELL`/parent process; exit 64 if
    /// undetectable.
    ///
    /// When absent, output uses the context-level format (root `--format` flag;
    /// default plain). Use `ocx --format json package env` for JSON.
    /// `--shell=sh` is an alias for `--shell=dash` (POSIX strict).
    #[arg(
        long,
        value_enum,
        value_name = "SHELL",
        num_args = 0..=1,
        require_equals = true
    )]
    shell: Option<Option<Shell>>,
}

impl Env {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let platforms = platforms_or_default(self.platforms.as_slice());
        let identifiers = options::Identifier::transform_all(self.packages.clone(), context.default_registry())?;

        let manager = context.manager();

        let info = if let Some(kind) = self.content_path.symlink_kind() {
            manager.find_symlink_all(identifiers, kind).await?
        } else {
            manager
                .find_or_install_all(identifiers, platforms, context.concurrency())
                .await?
        };

        let info: Vec<std::sync::Arc<ocx_lib::package::install_info::InstallInfo>> =
            info.into_iter().map(std::sync::Arc::new).collect();
        let entries = manager.resolve_env(&info, self.self_view).await?;
        // `--shell[=NAME]` → eval-safe emit path (handshake §3, C5).
        // Shared bare-shell autodetect + identical UsageError (conventions).
        // Branch BEFORE consuming `entries` via `into_iter()`.
        if let Some(shell) = resolve_shell_arg(self.shell)? {
            emit_lines(shell, &entries);
            return Ok(ExitCode::SUCCESS);
        }

        let all_entries: Vec<api::data::env::EnvEntry> = entries
            .into_iter()
            .map(|e| api::data::env::EnvEntry {
                key: e.key,
                value: e.value,
                kind: e.kind,
            })
            .collect();

        // No synthetic PATHEXT entry: the Windows launcher is now a native
        // `<name>.exe` shim, and `.EXE` is unconditionally in the default
        // Windows PATHEXT — nothing to inject for bare-name resolution.

        // Backend channel is stdout; if a human is watching a TTY, hint that
        // the default report output is not eval-safe (stderr only — stdout
        // stays a pure machine channel).
        if std::io::stdout().is_terminal() {
            context
                .ui()
                .warn("default output is not eval-safe; use --shell=bash to activate");
        }

        // Structured report. Format is a context-level concern (root
        // `--format`); this command does not override it.
        context.api().report(&api::data::env::EnvVars::new(all_entries))?;

        Ok(ExitCode::SUCCESS)
    }
}
