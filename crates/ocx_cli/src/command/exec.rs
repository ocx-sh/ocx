use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{env, log, oci};

use crate::{conventions::*, options, task};

/// Runs installed packages.
#[derive(Parser)]
pub struct Exec {
    /// Run in interactive mode, which will keep the environment variables set after the command finishes.
    ///
    /// This is useful for shells that support it, such as PowerShell and Elvish. For other shells, this flag will be ignored.
    #[clap(short = 'i', long = "interactive", default_value_t = false)]
    interactive: bool,

    /// Target platforms to consider when resolving packages. If not specified, only supported platforms will be considered.
    #[clap(short = 'p', long = "platform", value_delimiter = ',', value_name = "PLATFORM", num_args = 0..)]
    platforms: Vec<oci::Platform>,

    /// Package identifiers to install.
    #[clap(required = true, num_args = 1.., value_terminator = "--")]
    packages: Vec<options::Identifier>,

    /// Command to execute, with arguments. The command will be executed with the environment with the packages.
    #[clap(allow_hyphen_values = true, allow_hyphen_values = true, num_args = 1..)]
    command: Vec<String>,
}

impl Exec {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let platforms = platforms_or_default(&self.platforms);
        let identifier =
            options::Identifier::transform_all(self.packages.clone().into_iter(), context.default_registry())?;

        let info = task::package::find_or_install::FindOrInstall {
            context: context.clone(),
            file_structure: context.file_structure().clone(),
            platforms,
        }
        .find_or_install_all(identifier)
        .await?;

        use std::process::Stdio;
        use tokio::process::Command;

        let mut process_env = env::Env::clean();
        for info in info {
            log::debug!("Setting environment variables for package: {}", info.identifier);
            if let Some(env) = info.metadata.env() {
                env.resolve_into_env(info.content, &mut process_env)?;
            }
        }

        let Some((command, args)) = self.command.split_first() else {
            return Err(anyhow::anyhow!("No command provided to execute."));
        };

        let mut child_process = Command::new(command)
            .args(args)
            .stdin(if self.interactive {
                Stdio::inherit()
            } else {
                Stdio::null()
            })
            .stderr(Stdio::inherit())
            .stdout(Stdio::inherit())
            .envs(process_env.into_iter())
            .spawn()?;

        let status = child_process.wait().await?;
        if !status.success() {
            match status.code() {
                Some(code) => return Ok(ExitCode::from(code as u8)),
                None => return Ok(ExitCode::FAILURE),
            }
        }
        Ok(ExitCode::SUCCESS)
    }
}
