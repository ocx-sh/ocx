// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Subcommand;

#[derive(Subcommand)]
pub enum Shell {
    Env(super::shell_env::ShellEnv),
    Completion(super::shell_completion::ShellCompletion),
    /// Print a shell-specific init snippet that wires `ocx hook-env` into the shell.
    Init(super::shell_init::ShellInit),
    /// Manage the shell profile — packages loaded at shell startup.
    #[command(subcommand)]
    Profile(super::shell_profile::ShellProfile),
}

impl Shell {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        match self {
            Shell::Env(env) => env.execute(context).await,
            Shell::Completion(completion) => completion.execute().await,
            Shell::Init(init) => init.execute(context).await,
            Shell::Profile(profile) => profile.execute(context).await,
        }
    }
}
