// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

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
            // TODO: Custom error type that can provide more structured information incl. error codes.
            log::error!("Error: {err}");
            ExitCode::FAILURE
        }
    }
}
