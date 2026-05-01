// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::env;
use ocx_lib::env::OcxConfigView;
use ocx_lib::package::metadata::env::entry::Entry as EnvEntry;
use ocx_lib::package_manager::launcher;
use ocx_lib::utility::child_process;

use crate::{conventions::*, options};

/// Runs installed packages.
///
/// Each positional accepts an OCI identifier (e.g. `node:20`).
/// Packages are resolved through the index and auto-installed when missing.
#[derive(Parser)]
pub struct Exec {
    /// Start with a clean environment containing only the package variables, instead of inheriting the current shell environment.
    #[clap(long = "clean", default_value_t = false)]
    clean: bool,

    /// Expose the package's full env, including its private (self-only)
    /// entries. Off by default: only public + interface entries are loaded
    /// (the consumer view). Generated launchers use `ocx launcher exec` which
    /// enables self-view internally.
    ///
    /// See https://ocx.sh/docs/in-depth/environments#visibility-views for the full view semantics.
    #[clap(long = "self", default_value_t = false)]
    self_view: bool,

    #[clap(flatten)]
    platforms: options::Platforms,

    /// Package identifiers to layer environment from.
    ///
    /// Each value is a bare OCI identifier (e.g. `node:20`); identifiers are
    /// resolved through the index and auto-installed when missing.
    #[clap(required = true, num_args = 1.., value_terminator = "--")]
    packages: Vec<options::Identifier>,

    /// Command to execute, with arguments. The command will be executed with the environment with the packages.
    ///
    /// `required = true` + `num_args = 1..` means clap rejects the invocation
    /// before [`Self::execute`] runs when the slice would be empty, so the
    /// `.split_first().expect(...)` below is sound: clap is the single source
    /// of truth for non-emptiness, and we depend on its guarantee rather than
    /// duplicating the check.
    #[clap(allow_hyphen_values = true, required = true, num_args = 1..)]
    command: Vec<String>,
}

impl Exec {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let manager = context.manager();
        let platforms = platforms_or_default(self.platforms.as_slice());

        let identifiers = options::Identifier::transform_all(self.packages.clone(), context.default_registry())?;
        let infos = manager.find_or_install_all(identifiers, platforms).await?;
        let install_infos: Vec<std::sync::Arc<ocx_lib::package::install_info::InstallInfo>> =
            infos.into_iter().map(std::sync::Arc::new).collect();
        let entries = manager.resolve_env(&install_infos, self.self_view).await?;
        self.run_with_env(entries, context.config_view()).await
    }

    /// Run the configured command with the given resolved environment.
    ///
    /// `child_process::exec` diverges on success on every platform —
    /// Unix `execvp(2)`s, Windows spawns + waits + `process::exit`s — so
    /// this function only returns when start-up itself fails. The
    /// `anyhow::Result<ExitCode>` shape is kept for symmetry with sibling
    /// commands; the `Ok` arm is unreachable.
    async fn run_with_env(&self, entries: Vec<EnvEntry>, config_view: &OcxConfigView) -> anyhow::Result<ExitCode> {
        let mut process_env = if self.clean { env::Env::clean() } else { env::Env::new() };
        process_env.apply_entries(&entries);
        // Forward the running ocx's resolution-affecting config (binary path,
        // offline/remote, config file, index) to any child ocx (e.g. through
        // a generated entrypoint launcher). Runs after `Env::clean()` /
        // `Env::new()` so the outer ocx's parsed state is the sole authority
        // for `OCX_*` keys on the child env — no ambient parent-shell export
        // can override it.
        process_env.apply_ocx_config(config_view);
        // Ensure the child PATHEXT lists the OCX launcher extension so generated
        // `.cmd` shims are resolvable. No-op on non-Windows.
        launcher::emplace_pathext(&mut process_env);

        // clap enforces `required = true, num_args = 1..` on the `command`
        // field — `self.command` is always non-empty at this point.
        let (command, args) = self
            .command
            .split_first()
            .expect("clap required=true guarantees at least one command element");

        let resolved = process_env.resolve_command(command);

        // Replace this process with the child on Unix (PID inherited via
        // `execvp(2)`); on Windows spawn+wait then `process::exit`, since
        // `CreateProcess` has no exec equivalent. Either way the helper
        // diverges on success — only start-up failures fall through to
        // the error-wrapping path below.
        let err = child_process::exec(&resolved, args, process_env);
        Err(anyhow::Error::from(err).context(format!("failed to run '{}'", resolved.display())))
    }
}
