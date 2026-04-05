// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{log::*, package::install_info::InstallInfo, package::metadata::env::modifier::ModifierKind, shell};

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
                    debug!("Could not detect shell for profile load");
                    return Ok(ExitCode::SUCCESS);
                }
            }
        };

        let manager = context.manager();
        let manifest = manager.profile().load()?;

        let (resolved, conflicts) = manager.resolve_profile_env(&manifest.packages).await;

        for conflict in &conflicts {
            warn!("{}", conflict);
        }

        // Convert resolved profile entries to InstallInfo for dependency expansion.
        let install_infos: Vec<_> = resolved.iter().map(InstallInfo::from).collect();

        println!("{}", detected_shell.comment("ocx profile"));
        let entries = manager.resolve_env(&install_infos).await?;
        for entry in &entries {
            match entry.kind {
                ModifierKind::Path => println!("{}", detected_shell.export_path(&entry.key, &entry.value)),
                ModifierKind::Constant => println!("{}", detected_shell.export_constant(&entry.key, &entry.value)),
            }
        }

        Ok(ExitCode::SUCCESS)
    }
}
