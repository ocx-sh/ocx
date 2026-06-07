// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::{CommandFactory, FromArgMatches, Parser};
use ocx_lib::cli::progress::ProgressManager;
use ocx_lib::cli::{self, ColorMode, DataInterface, LogLevel, LogSettings, Printer, ProgressMode};

mod annotations;
mod command;
mod discord;
mod error;
mod filter;
mod junit;
mod normalizer;
mod pipeline;
mod resolver;
mod run_summary;
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

    // Parsed early in main() via ColorMode::from_args(); this field exists
    // so clap recognizes --color and shows it in --help.
    /// When to use ANSI colors in output.
    #[arg(long, value_enum, value_name = "WHEN", default_value_t = Default::default(), global = true)]
    color: ColorMode,
}

#[tokio::main]
async fn main() -> ExitCode {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("Failed to install default crypto provider");

    let color_mode = cli::ColorMode::from_args();
    let color_config = color_mode.config();
    color_config.apply();

    let styles = cli::clap_styles(color_config.stdout);
    let matches = Cli::command().color(color_mode.into()).styles(styles).get_matches();
    let cli = match Cli::from_arg_matches(&matches) {
        Ok(cli) => cli,
        Err(e) => e.exit(),
    };

    let level = cli.log_level.or(Some(LogLevel::Info));
    // Span-free progress manager (ADR adr_progress_architecture), created
    // before the subscriber so its MultiProgress backs the fmt log writer.
    let progress = if ProgressMode::detect().stderr {
        ProgressManager::stderr()
    } else {
        ProgressManager::disabled()
    };
    if let Err(e) = LogSettings::default()
        .with_console_level(level)
        .with_stderr_color(color_config.stderr)
        .init_with_progress(&progress)
    {
        eprintln!("Failed to initialize logging: {e}");
        return ExitCode::FAILURE;
    }

    let printer = DataInterface::new(Printer::new(color_config.stdout, color_config.stderr));
    match cli.command.execute(&printer, &progress).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            ocx_lib::log::error!("{err:#}");
            err.kind_exit_code().into()
        }
    }
}
