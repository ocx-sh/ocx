// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::{CommandFactory, FromArgMatches, Parser};
use ocx_lib::cli::{self, ColorMode, LogLevel, LogSettings, Printer, indicatif::ProgressStyle};

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
    let matches = Cli::command()
        .color(color_mode.into())
        .styles(styles)
        .get_matches();
    let cli = match Cli::from_arg_matches(&matches) {
        Ok(cli) => cli,
        Err(e) => e.exit(),
    };

    let level = cli.log_level.or(Some(LogLevel::Info));
    let style = ProgressStyle::with_template("{spinner:.blue} {msg}").expect("valid indicatif template");
    if let Err(e) = LogSettings::default()
        .with_console_level(level)
        .with_stderr_color(color_config.stderr)
        .init_with_indicatif(style)
    {
        eprintln!("Failed to initialize logging: {e}");
        return ExitCode::FAILURE;
    }

    let printer = Printer::new(color_config.stdout);
    match cli.command.execute(&printer).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            ocx_lib::log::error!("{err:#}");
            err.exit_code()
        }
    }
}
