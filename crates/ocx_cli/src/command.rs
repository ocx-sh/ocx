use std::process::ExitCode;

use clap::Subcommand;

pub mod index;
pub mod index_catalog;
pub mod index_list;
pub mod index_update;
pub mod info;
pub mod install;
pub mod package;
pub mod package_create;
pub mod package_push;
pub mod env;
pub mod exec;
pub mod version;
pub mod shell;
pub mod shell_env;
pub mod shell_completion;

#[derive(Subcommand)]
pub enum Command {
    /// Operations related to the package index
    #[command(subcommand)]
    Index(index::Index),
    /// Print ocx version and build information
    Info(info::Info),
    /// Install packages from a local or remote index.
    Install(install::Install),
    /// Runs installed packages.
    Exec(exec::Exec),
    /// Print resolved environment variables for one or more packages.
    Env(env::Env),
    /// Operations related to packages (e.g. bundling or deploying)
    #[command(subcommand)]
    Package(package::Package),
    #[command(subcommand)]
    Shell(shell::Shell),
    /// Print the version of ocx
    Version(version::Version),
}

impl Command {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        match self {
            Command::Index(index) => index.execute(context).await,
            Command::Info(info) => info.execute().await,
            Command::Install(install) => install.execute(context).await,
            Command::Exec(exec) => exec.execute(context).await,
            Command::Env(env) => env.execute(context).await,
            Command::Package(package) => package.execute(context).await,
            Command::Shell(shell) => shell.execute(context).await,
            Command::Version(version) => version.execute().await,
        }
    }
}
