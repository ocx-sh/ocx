// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::{CommandFactory, Parser};
use ocx_lib::{log, shell};

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
    /// applies decides whether anything is printed at all: `OCX_NO_COMPLETIONS`,
    /// then `completions` under `[shell]` in config.toml, then whether the
    /// session is interactive. The `$OCX_HOME/env.sh` and `env.elv` shims pass
    /// it, because they inject completions through this command rather than
    /// through the activation stream.
    ///
    /// https://ocx.sh/docs/in-depth/shell-integration
    #[clap(long = "if-enabled")]
    if_enabled: bool,

    /// The shell to generate the completions for
    #[clap(long, value_enum)]
    shell: Option<clap_complete::Shell>,

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

    /// Resolve the C-039 completions ladder for this invocation.
    ///
    /// Rungs 1 and 2 are unreachable here by construction, so
    /// [`options::Completion::default`] is the right input: `--if-enabled` is
    /// itself the caller's way of asking for a decision, and its absence is the
    /// unconditional answer this command has always given. What is left —
    /// `OCX_NO_COMPLETIONS`, `[shell] completions`, session interactivity — is
    /// evaluated by the one shared implementation, so a shim's separate
    /// completion injection and `ocx self activate`'s inline one can never
    /// disagree about whether completions are wanted.
    async fn completions_enabled(&self, options: &ContextOptions) -> bool {
        let (shell_config, _tiers) = load_shell_config(options).await;
        options::Completion::default().enabled(
            self.interactive.resolve_probed(),
            shell_config.and_then(|config| config.completions),
        )
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
