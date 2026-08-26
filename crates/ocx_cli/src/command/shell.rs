// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Subcommand;

/// Shell-integration commands.
///
/// Generate static shell completion scripts, and report the state of the
/// per-prompt environment integration. Login-profile activation is handled by
/// `$OCX_HOME/env.sh` (sourced from your shell profile), not here.
#[derive(Subcommand)]
pub enum Shell {
    /// Generate shell completion scripts.
    Completion(super::shell_completion::ShellCompletion),
    /// Report the shell integration's state, and why it is inert when it is.
    State(super::shell_state::ShellState),
}

impl Shell {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        match self {
            Shell::Completion(completion) => completion.execute().await,
            Shell::State(state) => state.execute(context).await,
        }
    }
}
