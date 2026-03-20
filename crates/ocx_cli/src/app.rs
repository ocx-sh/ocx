// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::{CommandFactory, FromArgMatches, Parser};
use ocx_lib::cli;

use crate::command;

mod context;
pub use context::Context;

mod context_options;
pub use context_options::ContextOptions;

mod update_check;

mod version;
pub use version::version;

#[derive(Parser)]
#[command(name = "ocx", about, long_about = None)]
#[command(about = "A simple package manager for pre-built binaries.", long_about = None)]
pub struct Cli {
    #[command(flatten)]
    pub context: ContextOptions,

    #[command(subcommand)]
    pub command: Option<command::Command>,
}

pub struct App {}

impl App {
    pub fn new() -> Self {
        Self {}
    }

    pub async fn run(self) -> anyhow::Result<ExitCode> {
        // Pre-parse --color before clap so help/error output respects it.
        let color_mode = cli::ColorMode::from_args();
        let color_config = color_mode.config();
        color_config.apply();

        let styles = cli::clap_styles(color_config.stdout);
        let matches = Cli::command()
            .color(color_mode.into())
            .styles(styles.clone())
            .get_matches();
        let cli = Cli::from_arg_matches(&matches)?;

        let context = Context::try_init(&cli.context, color_config).await?;
        if should_check_for_update(&cli.command) {
            update_check::check_for_update(&context).await;
        }
        match &cli.command {
            Some(command) => command.execute(context).await,
            None => {
                Cli::command().color(color_mode.into()).styles(styles).print_help()?;
                Ok(ExitCode::SUCCESS)
            }
        }
    }
}

/// Skip the update check for commands that only print static info.
fn should_check_for_update(command: &Option<command::Command>) -> bool {
    !matches!(
        command,
        Some(
            command::Command::Version(_)
                | command::Command::Info(_)
                | command::Command::Shell(command::shell::Shell::Completion(_))
        )
    )
}

pub async fn run() -> anyhow::Result<ExitCode> {
    App::new().run().await
}
