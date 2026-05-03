// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Subcommand;

#[derive(Subcommand)]
pub enum Shell {
    /// Print shell export statements for one or more packages.
    Env(super::shell_env::ShellEnv),
    /// Generate shell completion scripts.
    Completion(super::shell_completion::ShellCompletion),
    /// Print stateful shell exports for the project toolchain (prompt-hook entry point).
    ///
    /// Reads the nearest project `ocx.toml`, loads `ocx.lock`, and emits shell
    /// export lines only when the resolved set has changed since the last invocation.
    /// The `_OCX_APPLIED` environment variable carries the fingerprint for the fast path.
    Hook(super::shell_hook::ShellHook),
    /// Print stateless shell exports for the project toolchain (direnv entry point).
    ///
    /// Reads the nearest project `ocx.toml`, loads `ocx.lock`, and emits a fresh
    /// export block on every invocation — suitable for `eval "$(ocx shell direnv)"` in
    /// `.envrc`. Unlike `shell hook`, this command does not consult or update `_OCX_APPLIED`.
    Direnv(super::shell_direnv::ShellDirenv),
    /// Print a shell-specific init snippet that wires `ocx shell hook` into the shell.
    Init(super::shell_init::ShellInit),
}

impl Shell {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        match self {
            Shell::Env(env) => env.execute(context).await,
            Shell::Completion(completion) => completion.execute().await,
            Shell::Hook(hook) => hook.execute(context).await,
            Shell::Direnv(direnv) => direnv.execute(context).await,
            Shell::Init(init) => init.execute(context).await,
        }
    }
}
