// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{log, oci, package::metadata::env::modifier::ModifierKind, shell};

use crate::{conventions::*, options};

/// Prints shell export statements for the environment variables declared by one or more packages.
///
/// Output is intended to be evaluated by the shell, e.g.:
///   `eval "$(ocx shell env mypackage)"`
///
/// This command does not auto-install packages.  If a package is not already
/// available locally it will fail with an error.
///
/// Use `--candidate` or `--current` to emit paths rooted in a stable symlink
/// rather than the content-addressed object store — useful for shell profiles
/// that should not change on every package update.
/// See the path resolution modes documentation for details.
#[derive(Parser)]
pub struct ShellEnv {
    /// Platforms to consider when looking for the package. If not specified, it will use the current supported platform.
    #[clap(short = 'p', long = "platform", value_delimiter = ',', value_name = "PLATFORM")]
    platforms: Vec<oci::Platform>,

    /// The shell to generate the environment configuration for. If not specified, it will be auto-detected.
    #[clap(short = 's', long = "shell")]
    shell: Option<shell::Shell>,

    #[clap(flatten)]
    content_path: options::ContentPath,

    /// Package identifiers to print the environment for.
    #[arg(required = true, num_args = 1.., value_name = "PACKAGE")]
    packages: Vec<options::Identifier>,
}

impl ShellEnv {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        warn_if_pathext_missing_launcher();
        let shell = match self.shell {
            Some(shell) => shell,
            None => {
                if let Some(shell) = shell::Shell::detect() {
                    shell
                } else {
                    anyhow::bail!("could not detect the current shell; specify it using the --shell option");
                }
            }
        };
        log::debug!("Using shell: {}", shell);
        let manager = context.manager();
        let package_infos = resolve_packages(
            self.packages.clone(),
            &self.platforms,
            &self.content_path,
            manager,
            context.default_registry(),
        )
        .await?;

        println!("{}", shell.comment("ocx env"));
        let entries = manager.resolve_env(&package_infos).await?;
        for entry in &entries {
            match entry.kind {
                ModifierKind::Path => println!("{}", shell.export_path(&entry.key, &entry.value)),
                ModifierKind::Constant => println!("{}", shell.export_constant(&entry.key, &entry.value)),
            }
        }

        Ok(ExitCode::SUCCESS)
    }
}
