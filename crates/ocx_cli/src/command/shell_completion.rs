// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::{CommandFactory, Parser};
use ocx_lib::{log, shell};

/// Generates shell completion scripts.
#[derive(Parser)]
pub struct ShellCompletion {
    /// The shell to generate the completions for
    #[clap(long, value_enum)]
    shell: Option<clap_complete::Shell>,
}

impl ShellCompletion {
    pub async fn execute(&self) -> anyhow::Result<ExitCode> {
        let mut cmd = crate::app::Cli::command();
        let cmd_name = cmd.get_name().to_string();
        let shell = match self.shell {
            Some(shell) => shell,
            None => {
                if let Some(shell) = shell::Shell::detect() {
                    match shell.try_into() {
                        Ok(clap_shell) => clap_shell,
                        Err(err) => anyhow::bail!(
                            "The detected shell ({shell}) is not supported for completion generation: {err}"
                        ),
                    }
                } else {
                    anyhow::bail!("Could not detect the current shell. Please specify it using the --shell option.");
                }
            }
        };
        log::info!("Generating completions for shell: {}", shell);
        clap_complete::generate(shell, &mut cmd, cmd_name, &mut std::io::stdout());
        Ok(ExitCode::SUCCESS)
    }
}
