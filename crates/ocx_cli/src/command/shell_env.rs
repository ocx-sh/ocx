use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{log, oci, shell};

use crate::{conventions::*, options, task};

/// Prints the environment configuration for a given package.
#[derive(Parser)]
pub struct ShellEnv {
    /// Platforms to consider when looking for the package. If not specified, it will use the current supported platform.
    #[clap(short = 'p', long = "platform", value_delimiter = ',', value_name = "PLATFORM")]
    platforms: Vec<oci::Platform>,

    /// The shell to generate the environment configuration for. If not specified, it will be auto-detected.
    #[clap(short = 's', long = "shell")]
    shell: Option<shell::Shell>,

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
        let identifier =
            options::Identifier::transform_all(self.packages.clone().into_iter(), context.default_registry())?;
        let package_info = task::package::find::Find {
            context: context.clone(),
            platforms: platforms.clone(),
            file_structure: context.file_structure().clone(),
        }
        .find_all(identifier)
        .await?;

        for package_info in package_info {
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
