use std::process::ExitCode;

use clap::{CommandFactory, Parser};

use crate::command;

mod context;
pub use context::Context;

mod context_options;
pub use context_options::ContextOptions;

mod log_settings;
pub use log_settings::LogSettings;

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
        let cli = Cli::parse();
        let context = Context::try_init(&cli.context).await?;
        match &cli.command {
            Some(command) => command.execute(context).await,
            None => {
                Cli::command().print_help()?;
                Ok(ExitCode::SUCCESS)
            }
        }
    }
}

pub async fn run() -> anyhow::Result<ExitCode> {
    App::new().run().await
}
