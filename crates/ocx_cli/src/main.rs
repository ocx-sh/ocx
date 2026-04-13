// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use ocx_lib::cli::classify_error;
use ocx_lib::log;

mod api;
mod app;
mod command;
mod conventions;
mod options;

#[tokio::main]
async fn main() -> ExitCode {
    match app::run().await {
        Ok(exit_code) => exit_code,
        Err(err) => {
            // {err:#} walks the anyhow source chain so causes (e.g. "permission denied") surface to users.
            // `log::error!` already categorizes the line, so we omit a redundant "Error: " prefix.
            log::error!("{err:#}");
            classify_error(err.as_ref()).into()
        }
    }
}
