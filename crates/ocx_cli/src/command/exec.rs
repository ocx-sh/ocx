// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{env, oci};

use crate::{conventions::*, options};

/// Runs installed packages.
#[derive(Parser)]
pub struct Exec {
    /// Run in interactive mode, which will keep the environment variables set after the command finishes.
    ///
    /// This is useful for shells that support it, such as PowerShell and Elvish. For other shells, this flag will be ignored.
    #[clap(short = 'i', long = "interactive", default_value_t = false)]
    interactive: bool,

    /// Start with a clean environment containing only the package variables, instead of inheriting the current shell environment.
    #[clap(long = "clean", default_value_t = false)]
    clean: bool,

    /// Target platforms to consider when resolving packages. If not specified, only supported platforms will be considered.
    #[clap(short = 'p', long = "platform", value_delimiter = ',', value_name = "PLATFORM", num_args = 0..)]
    platforms: Vec<oci::Platform>,

    /// Package identifiers to install.
    #[clap(required = true, num_args = 1.., value_terminator = "--")]
    packages: Vec<options::Identifier>,

    /// Command to execute, with arguments. The command will be executed with the environment with the packages.
    #[clap(allow_hyphen_values = true, num_args = 1..)]
    command: Vec<String>,
}

impl Exec {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let platforms = platforms_or_default(&self.platforms);
        let identifier =
            options::Identifier::transform_all(self.packages.clone().into_iter(), context.default_registry())?;

        let manager = context.manager();
        let info = manager.find_or_install_all(identifier, platforms).await?;

        let entries = manager.resolve_env(&info).await?;
        let mut process_env = if self.clean { env::Env::clean() } else { env::Env::new() };
        process_env.apply_entries(&entries);

        use std::process::Stdio;
        use tokio::process::Command;

        let Some((command, args)) = self.command.split_first() else {
            return Err(anyhow::anyhow!("No command provided to execute."));
        };

        let resolved = process_env.resolve_command(command);

        let mut child_process = Command::new(&resolved)
            .args(args)
            .stdin(if self.interactive {
                Stdio::inherit()
            } else {
                Stdio::null()
            })
            .stderr(Stdio::inherit())
            .stdout(Stdio::inherit())
            .envs(process_env.into_iter())
            .spawn()?;

        let status = child_process.wait().await?;
        if !status.success() {
            match status.code() {
                Some(code) => return Ok(ExitCode::from(code as u8)),
                None => return Ok(ExitCode::FAILURE),
            }
        }
        Ok(ExitCode::SUCCESS)
    }
}
