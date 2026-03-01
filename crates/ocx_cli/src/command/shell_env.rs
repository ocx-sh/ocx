use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{log, oci, shell};

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
        let shell = match self.shell {
            Some(shell) => shell,
            None => {
                if let Some(shell) = shell::Shell::detect() {
                    shell
                } else {
                    anyhow::bail!("Could not detect the current shell. Please specify it using the --shell option.");
                }
            }
        };
        log::debug!("Using shell: {}", shell);
        let platforms = platforms_or_default(&self.platforms);
        let identifiers =
            options::Identifier::transform_all(self.packages.clone().into_iter(), context.default_registry())?;

        let manager = context.manager();

        let package_infos = if let Some(kind) = self.content_path.symlink_kind() {
            manager.find_symlink_all(identifiers, kind).await?
        } else {
            manager.find_all(identifiers, platforms).await?
        };

        for package_info in package_infos {
            let mut profile_builder = shell.profile_builder(package_info.content);
            if let Some(env) = package_info.metadata.env() {
                for var in env {
                    profile_builder.add(var.clone());
                }
            }
            print!("{}", profile_builder.take());
        }

        Ok(ExitCode::SUCCESS)
    }
}
