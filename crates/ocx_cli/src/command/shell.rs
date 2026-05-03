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
    /// Manage the shell profile — packages loaded at shell startup.
    #[command(subcommand)]
    Profile(super::shell_profile::ShellProfile),
}

impl Shell {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        match self {
            Shell::Env(env) => env.execute(context).await,
            Shell::Completion(completion) => completion.execute().await,
            Shell::Profile(profile) => profile.execute(context).await,
        }
    }
}
