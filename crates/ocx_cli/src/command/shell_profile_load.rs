// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{log, shell};

/// Output shell export statements for all profiled packages.
///
/// Reads `$OCX_HOME/profile.json` and emits shell-specific export lines for
/// each package's declared environment variables. Intended to be called from
/// the shell env file via `eval`.
///
/// Broken entries (missing symlinks or metadata) emit a warning to stderr
/// and are skipped.
#[derive(Parser)]
pub struct ShellProfileLoad {
    /// The shell to generate exports for. Auto-detected if not specified.
    #[clap(short = 's', long = "shell")]
    shell: Option<shell::Shell>,
}

impl ShellProfileLoad {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let detected_shell = match self.shell {
            Some(s) => s,
            None => {
                if let Some(s) = shell::Shell::detect() {
                    s
                } else {
                    // Silent failure — this runs from eval in shell init
                    log::debug!("Could not detect shell for profile load");
                    return Ok(ExitCode::SUCCESS);
                }
            }
        };

        let manifest = context.manager().profile().load()?;

        let (resolved, conflicts) = context.manager().resolve_profile_env(&manifest.packages).await;

        for conflict in &conflicts {
            log::warn!(
                "Env var conflict: `{}` is set by both `{}` ({}) and `{}` ({})",
                conflict.key,
                conflict.previous_package,
                conflict.previous_value,
                conflict.current_package,
                conflict.current_value
            );
        }

        for entry in &resolved {
            if let Some(env) = entry.metadata.env() {
                let mut profile_builder = detected_shell.profile_builder(&entry.content_path);
                for var in env {
                    profile_builder.add(var.clone());
                }
                print!("{}", profile_builder.take());
            }
        }

        Ok(ExitCode::SUCCESS)
    }
}
