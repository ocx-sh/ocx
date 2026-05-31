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
                        Err(err) => {
                            anyhow::bail!("detected shell ({shell}) not supported for completion generation: {err}")
                        }
                    }
                } else {
                    anyhow::bail!("could not detect the current shell; specify it using the --shell option");
                }
            }
        };
        log::debug!("Generating completions for shell: {}", shell);
        print!("{}", render_completion_script(&mut cmd, &cmd_name, shell));
        Ok(ExitCode::SUCCESS)
    }
}

/// Render the completion script for `shell`, adding the zsh `compinit` guard so
/// the output registers wherever it is sourced.
///
/// clap_complete's zsh script ends in `compdef _ocx ocx`, which requires
/// `compinit` to have run. The guard self-loads it, so the script is correct
/// even when sourced before the user's `.zshrc` runs `compinit` (e.g. from
/// `.zprofile`) — otherwise `compdef` is undefined and registration fails.
///
/// Shared by `ocx shell completion` (this command) and the inline completion
/// stream of `ocx self activate`, so both emit identical, self-sufficient
/// scripts.
pub(crate) fn render_completion_script(cmd: &mut clap::Command, cmd_name: &str, shell: clap_complete::Shell) -> String {
    let mut buf = Vec::new();
    clap_complete::generate(shell, cmd, cmd_name.to_string(), &mut buf);
    // clap_complete always writes valid UTF-8.
    let script = String::from_utf8_lossy(&buf).into_owned();
    if shell == clap_complete::Shell::Zsh {
        return format!("if (( ! $+functions[compdef] )); then\n  autoload -Uz compinit && compinit -C\nfi\n{script}");
    }
    script
}
