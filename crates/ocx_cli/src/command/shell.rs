// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Subcommand;

/// Shell-integration commands.
///
/// `ocx shell hook`, `ocx shell init`, and `ocx shell env` are deleted
/// (handshake section 7).  The login-profile activation model (`$OCX_HOME/env.sh`
/// + `ocx --global env --shell=sh`) replaces them.  Only `completion` survives.
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
