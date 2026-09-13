// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use ocx_lib::cli;

use crate::api::data::package_receipt::PackageReceipt;
use crate::app::CommandError;

#[derive(Parser)]
pub struct PackageReceiptCommand {
    /// Path to the bundle `ocx package create -o` wrote.
    #[arg(value_name = "BUNDLE")]
    bundle: PathBuf,
}

impl PackageReceiptCommand {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // The single-bundle twin of `push`/`test`'s layer-list resolution: one
        // path, so an unparseable one is a usage error rather than "no receipt".
        let path = crate::conventions::infer_receipt_file(&self.bundle)?;
        match crate::build_receipt::read(&path).await? {
            Some(receipt) => {
                context.api().report(&PackageReceipt::from(&receipt))?;
                Ok(ExitCode::SUCCESS)
            }
            None => Err(CommandError::new(
                format!(
                    "no build receipt at {}; `ocx package create` writes one beside the bundle",
                    path.display()
                ),
                cli::ExitCode::NotFound,
            )
            .into()),
        }
    }
}
