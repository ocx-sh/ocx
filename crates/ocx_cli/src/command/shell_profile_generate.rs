// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use clap::Parser;
use ocx_lib::{
    ConfigLoader, log::*, package::install_info::InstallInfo, package::metadata::env::modifier::ModifierKind, shell,
};

/// Generate a shell init file with all profile exports.
///
/// File-generating variant of `ocx shell profile load` — emits the same
/// export lines but writes to a file the user sources once from their
/// shell rc, rather than re-running `eval` on every shell startup.
///
/// Default output path is `$OCX_HOME/init.<shell>` (e.g., `init.bash`).
/// Use `--output <PATH>` to override or `--output -` to write to stdout.
#[derive(Parser)]
pub struct ShellProfileGenerate {
    /// The shell to generate exports for. Auto-detected if not specified.
    #[clap(short = 's', long = "shell")]
    shell: Option<shell::Shell>,

    /// Output path. Defaults to `$OCX_HOME/init.<shell>`. Use `-` for stdout.
    #[clap(short = 'o', long = "output", value_name = "PATH")]
    output: Option<PathBuf>,
}

/// Where the generated init script lands. Distinct enum (vs. `Option<PathBuf>`)
/// so the caller's `-` sentinel is decoded once at flag-resolution time and
/// the executor can match on intent.
enum OutputTarget {
    Stdout,
    File(PathBuf),
}

impl ShellProfileGenerate {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // Fail loud — `generate` is interactive (unlike `load` which runs from
        // shell init via `eval` and must stay silent on detection failure).
        let detected_shell = match self.shell {
            Some(s) => s,
            None => shell::Shell::detect()
                .ok_or_else(|| anyhow::anyhow!("could not detect shell; pass --shell <bash|zsh|fish|nushell>"))?,
        };

        let manager = context.manager();
        let manifest = manager.profile().load()?;
        let (resolved, conflicts) = manager.resolve_profile_env(&manifest.packages).await;
        for conflict in &conflicts {
            warn!("{}", conflict);
        }

        let install_infos: Vec<_> = resolved.iter().map(InstallInfo::from).collect();
        let entries = manager.resolve_env(&install_infos).await?;

        let mut output = String::new();
        output.push_str(&detected_shell.comment("ocx profile (generated)"));
        output.push('\n');
        for entry in &entries {
            // `export_path` / `export_constant` return `None` when the
            // env-var key fails POSIX validation; skip the line and emit
            // a stderr note rather than abort the whole generation.
            let line = match entry.kind {
                ModifierKind::Path => detected_shell.export_path(&entry.key, &entry.value),
                ModifierKind::Constant => detected_shell.export_constant(&entry.key, &entry.value),
            };
            match line {
                Some(line) => {
                    output.push_str(&line);
                    output.push('\n');
                }
                None => warn!("ocx: skipping invalid env-var key {:?}", entry.key),
            }
        }

        match self.resolve_output_target(detected_shell)? {
            OutputTarget::Stdout => print!("{output}"),
            OutputTarget::File(path) => {
                // `tokio::fs::write` truncates + writes; overwrites are the
                // intended behavior (the file is regenerated on every
                // profile change). User-driven init files don't need the
                // tempfile + atomic-rename machinery `profile/manager.rs`
                // uses for concurrent writers.
                tokio::fs::write(&path, output)
                    .await
                    .with_context(|| format!("writing init file at {}", path.display()))?;
                eprintln!("wrote init file to {}", path.display());
            }
        }

        Ok(ExitCode::SUCCESS)
    }

    fn resolve_output_target(&self, shell: shell::Shell) -> anyhow::Result<OutputTarget> {
        match self.output.as_deref() {
            Some(p) if p == std::path::Path::new("-") => Ok(OutputTarget::Stdout),
            Some(p) => Ok(OutputTarget::File(p.to_path_buf())),
            None => {
                // Single sanctioned API for `$OCX_HOME/init.<shell>` — the
                // per-shell basename table lives on `Shell` itself, the
                // `$OCX_HOME` path math stays in `ConfigLoader`.
                let path = ConfigLoader::home_init_path(shell)
                    .ok_or_else(|| anyhow::anyhow!("could not determine $OCX_HOME"))?;
                Ok(OutputTarget::File(path))
            }
        }
    }
}
