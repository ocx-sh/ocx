// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::{CommandFactory, Parser};
use ocx_shell::shell;

use crate::app::ContextOptions;
use crate::command::self_group::activate::load_shell_config;
use crate::options;

/// Generates shell completion scripts.
#[derive(Parser)]
pub struct ShellCompletion {
    /// Print nothing when completions are switched off for this session.
    ///
    /// Without this flag the script is always printed, which is what redirecting
    /// it into a file wants. With it, the same policy `ocx self activate`
    /// applies decides whether anything is printed at all: `OCX_NO_COMPLETION`,
    /// then `completions` under `[shell]` in config.toml, then whether the
    /// session is interactive.
    ///
    /// https://ocx.sh/docs/in-depth/shell-integration
    // The `env.sh` and `env.elv` shims pass it; renaming it breaks their completion injection.
    #[clap(long = "if-enabled")]
    if_enabled: bool,

    /// The shell to generate the completions for: bash, elvish, fish, powershell or zsh.
    ///
    /// Takes the same shell names as every other `--shell`; the shells
    /// without completion support are refused.
    #[clap(long, value_enum, hide_possible_values = true)]
    shell: Option<shell::Shell>,

    /// Session interactivity, as the calling shell measured it.
    ///
    /// Feeds the last rung of the policy `--if-enabled` consults, and nothing
    /// else. Ignored without `--if-enabled`.
    #[clap(flatten)]
    interactive: options::Interactive,
}

impl ShellCompletion {
    pub async fn execute(&self, options: &ContextOptions) -> anyhow::Result<ExitCode> {
        if self.if_enabled && !self.completions_enabled(options).await {
            log::debug!("completions are disabled for this session; emitting no script");
            return Ok(ExitCode::SUCCESS);
        }
        let mut cmd = crate::app::Cli::command();
        let cmd_name = cmd.get_name().to_string();
        let shell = match self.shell {
            Some(shell) => shell.try_into().map_err(|_| {
                crate::error::UsageError::new(format!(
                    "{shell} has no completion support; use --shell bash, elvish, fish, powershell or zsh"
                ))
            })?,
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

    /// Resolve the completions ladder through the implementation `ocx self activate` shares, so the two never disagree.
    async fn completions_enabled(&self, options: &ContextOptions) -> bool {
        let (shell_config, _tiers) = load_shell_config(options).await;
        options::Completion::default().enabled(
            self.interactive.resolve_probed(),
            shell_config.and_then(|config| config.completions),
        )
    }
}

/// Render the completion script for `shell`; shared with `ocx self activate`'s inline stream.
///
/// The zsh prefix self-loads `compinit`, or sourcing from `.zprofile` leaves `compdef` undefined.
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
