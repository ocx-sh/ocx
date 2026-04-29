// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{
    log::*, package::install_info::InstallInfo, package::metadata::env::modifier::ModifierKind, shell, symlink,
};

use crate::conventions::warn_if_pathext_missing_launcher;

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
        warn_if_pathext_missing_launcher();
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
        let install_infos: Vec<std::sync::Arc<InstallInfo>> = resolved
            .iter()
            .map(InstallInfo::from)
            .map(std::sync::Arc::new)
            .collect();

        println!("{}", detected_shell.comment("ocx profile"));
        let entries = manager.resolve_env(&install_infos).await?;
        for entry in &entries {
            match entry.kind {
                ModifierKind::Path => println!("{}", detected_shell.export_path(&entry.key, &entry.value)),
                ModifierKind::Constant => println!("{}", detected_shell.export_constant(&entry.key, &entry.value)),
            }
        }

        // Emit `<current>/entrypoints` PATH entries for packages that declare entrypoints.
        // For each resolved profile entry whose metadata has non-empty bundle_entrypoints,
        // prepend `$OCX_HOME/symlinks/{registry}/{repo}/current/entrypoints` to PATH so
        // launchers are findable without an explicit `ocx exec` wrapper.
        // Only emit when the `current` symlink actually exists on disk: the
        // default profile mode is `candidate` (no `current` symlink created),
        // so unconditional emission would add non-existent directories to PATH
        // on every shell startup.
        let symlinks = &manager.file_structure().symlinks;
        for resolved_entry in &resolved {
            if resolved_entry
                .metadata
                .bundle_entrypoints()
                .is_none_or(|e| e.is_empty())
            {
                continue;
            }
            let current = symlinks.current(&resolved_entry.identifier);
            if !symlink::is_link(&current) {
                continue;
            }
            let entrypoints_path = current.join("entrypoints");
            println!(
                "{}",
                detected_shell.export_path("PATH", entrypoints_path.to_string_lossy().as_ref())
            );
        }

        Ok(ExitCode::SUCCESS)
    }
}
