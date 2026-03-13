// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::cli::{LogLevel, LogSettings, indicatif::ProgressStyle};

mod annotations;
mod command;
mod error;
mod filter;
mod normalizer;
mod pipeline;
mod resolver;
mod source;
mod spec;
mod version_platform_map;

use command::Command;

#[derive(Parser)]
#[command(name = "ocx-mirror", about = "Mirror upstream binary releases into OCI registries")]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// The log level to use
    #[arg(short, long, value_enum, global = true)]
    log_level: Option<LogLevel>,
}

#[tokio::main]
async fn main() -> ExitCode {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("Failed to install default crypto provider");

    let cli = Cli::parse();

    let level = cli.log_level.or(Some(LogLevel::Info));
    let style = ProgressStyle::with_template("{spinner:.blue} {msg}").expect("valid indicatif template");
    if let Err(e) = LogSettings::default()
        .with_console_level(level)
        .init_with_indicatif(style)
    {
        eprintln!("Failed to initialize logging: {e}");
        return ExitCode::FAILURE;
    }

    match cli.command.execute().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            ocx_lib::log::error!("{err:#}");
            err.exit_code()
        }
    }
}
