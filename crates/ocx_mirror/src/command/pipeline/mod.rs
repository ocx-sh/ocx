// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx-mirror pipeline` subcommand group.
//!
//! Five subcommands implement the pre-publish multi-runner test pipeline:
//!
//! | Subcommand | GHA job | Purpose |
//! |---|---|---|
//! | `generate ci` | (local) | Render workflow + support scripts |
//! | `plan` | `discover` | Compute versions needing work |
//! | `prepare` | `prepare` | Download, verify, bundle one version |
//! | `push` | `push` | Aggregate JUNIT results, publish passing (V, P) pairs |
//! | `notify` | `notify` | Post Discord webhook summary |
//! | `describe` | `describe` | Publish catalog metadata (README + logo) to the registry |

pub mod describe;
pub mod generate;
pub mod notify;
pub mod plan;
pub mod prepare;
pub mod push;

use ocx_lib::cli::DataInterface;

use crate::error::MirrorError;

/// Dispatcher for `ocx-mirror pipeline <subcommand>`.
#[derive(clap::Subcommand)]
pub enum PipelineCommand {
    /// Generate CI workflow files from a mirror spec.
    #[command(subcommand)]
    Generate(generate::GenerateCommand),

    /// Compute which versions need work (side-effect-free, used by GHA `discover` job).
    Plan(plan::PlanCmd),

    /// Download, verify, and bundle one version across all declared platforms.
    Prepare(prepare::Prepare),

    /// Aggregate JUNIT results and publish passing platform packages.
    Push(push::Push),

    /// Post a Discord webhook notification from `run-summary.json`.
    Notify(notify::Notify),

    /// Publish catalog metadata (README + logo) to the registry.
    Describe(describe::Describe),
}

impl PipelineCommand {
    pub async fn execute(&self, printer: &DataInterface) -> Result<(), MirrorError> {
        match self {
            Self::Generate(cmd) => match cmd {
                generate::GenerateCommand::Ci(ci) => ci.execute(printer).await,
            },
            Self::Plan(cmd) => cmd.execute(printer).await,
            Self::Prepare(cmd) => cmd.execute(printer).await,
            Self::Push(cmd) => cmd.execute(printer).await,
            Self::Notify(cmd) => cmd.execute(printer).await,
            Self::Describe(cmd) => cmd.execute(printer).await,
        }
    }
}
