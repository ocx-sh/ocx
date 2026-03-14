// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Subcommand;

/// CI-specific commands for exporting package environments to CI systems.
#[derive(Subcommand)]
pub enum Ci {
    /// Export package environment variables to a CI system (e.g. GitHub Actions).
    Export(super::ci_export::CiExport),
}

impl Ci {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        match self {
            Ci::Export(export) => export.execute(context).await,
        }
    }
}
