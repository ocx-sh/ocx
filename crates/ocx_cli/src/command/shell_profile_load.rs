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
        // No deprecation note here: `load` is consumed by
        // `eval "$(ocx --offline shell profile load)"` in shell init —
        // a stderr note would spam every new terminal. The note still
        // emits from the interactive subcommands (`add`, `remove`,
        // `list`).
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
            // `export_path` / `export_constant` return `None` when the
            // env-var key fails POSIX validation; skip the line and emit
            // a stderr note rather than abort the whole profile load.
            let line = match entry.kind {
                ModifierKind::Path => detected_shell.export_path(&entry.key, &entry.value),
                ModifierKind::Constant => detected_shell.export_constant(&entry.key, &entry.value),
            };
            match line {
                Some(line) => println!("{line}"),
                None => eprintln!("# ocx: skipping invalid env-var key {:?}", entry.key),
            }
        }

        Ok(ExitCode::SUCCESS)
    }
}
