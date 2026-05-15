// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// [direnv](https://direnv.net) integration for the project toolchain.
///
/// Bare `ocx direnv` is shorthand for `ocx direnv init` — the once-per-project
/// setup that writes a `.envrc`. The generated `.envrc` evaluates
/// `ocx direnv export` on every directory entry.
#[derive(Parser)]
pub struct Direnv {
    #[command(subcommand)]
    command: Option<DirenvCommand>,
}

#[derive(Subcommand)]
enum DirenvCommand {
    /// Write a `.envrc` wiring `ocx direnv export` into direnv.
    Init(super::direnv_init::DirenvInit),
    /// Print stateless shell exports for the project toolchain (direnv entry point).
    Export(super::direnv_export::DirenvExport),
}

impl Direnv {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        match &self.command {
            Some(DirenvCommand::Init(init)) => init.execute(context).await,
            Some(DirenvCommand::Export(export)) => export.execute(context).await,
            // Bare `ocx direnv` defaults to the setup action.
            None => super::direnv_init::DirenvInit::default().execute(context).await,
        }
    }
}
