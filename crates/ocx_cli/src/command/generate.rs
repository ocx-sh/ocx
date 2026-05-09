// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Subcommand;

#[derive(Subcommand)]
pub enum Generate {
    /// Generate a `.envrc` for direnv integration in the current directory.
    Direnv(super::generate_direnv::GenerateDirenv),
}

impl Generate {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        match self {
            Generate::Direnv(direnv) => direnv.execute(context).await,
        }
    }
}
