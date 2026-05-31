// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Subcommand;

/// Shell-integration commands.
///
/// Generate static shell completion scripts. Login-profile activation is
/// handled by `$OCX_HOME/env.sh` (sourced from your shell profile), not here.
#[derive(Subcommand)]
pub enum Shell {
    /// Generate shell completion scripts.
    Completion(super::shell_completion::ShellCompletion),
}

impl Shell {
    pub async fn execute(&self, _context: crate::app::Context) -> anyhow::Result<ExitCode> {
        match self {
            Shell::Completion(completion) => completion.execute().await,
        }
    }
}
