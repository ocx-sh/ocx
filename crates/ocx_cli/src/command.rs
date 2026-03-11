// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Subcommand;

pub mod clean;
pub mod deselect;
pub mod env;
pub mod exec;
pub mod find;
pub mod index;
pub mod index_catalog;
pub mod index_list;
pub mod index_update;
pub mod info;
pub mod install;
pub mod package;
pub mod package_create;
pub mod package_push;
pub mod select;
pub mod shell;
pub mod shell_completion;
pub mod shell_env;
pub mod uninstall;
pub mod version;

#[derive(Subcommand)]
pub enum Command {
    /// Remove unreferenced objects from the local object store.
    Clean(clean::Clean),
    /// Remove the current-version symlink for one or more packages.
    Deselect(deselect::Deselect),
    /// Resolve packages and print their content directory paths.
    Find(find::Find),
    /// Operations related to the package index
    #[command(subcommand)]
    Index(index::Index),
    /// Print ocx version and build information
    Info(info::Info),
    /// Install packages from a local or remote index.
    Install(install::Install),
    /// Remove an installed candidate for one or more packages.
    Uninstall(uninstall::Uninstall),
    /// Runs installed packages.
    Exec(exec::Exec),
    /// Print resolved environment variables for one or more packages.
    Env(env::Env),
    /// Operations related to packages (e.g. bundling or deploying)
    #[command(subcommand)]
    Package(package::Package),
    /// Set the current version of one or more packages.
    Select(select::Select),
    #[command(subcommand)]
    Shell(shell::Shell),
    /// Print the version of ocx
    Version(version::Version),
}

impl Command {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        match self {
            Command::Clean(clean) => clean.execute(context).await,
            Command::Deselect(deselect) => deselect.execute(context).await,
            Command::Find(find) => find.execute(context).await,
            Command::Index(index) => index.execute(context).await,
            Command::Info(info) => info.execute().await,
            Command::Install(install) => install.execute(context).await,
            Command::Uninstall(uninstall) => uninstall.execute(context).await,
            Command::Exec(exec) => exec.execute(context).await,
            Command::Env(env) => env.execute(context).await,
            Command::Package(package) => package.execute(context).await,
            Command::Select(select) => select.execute(context).await,
            Command::Shell(shell) => shell.execute(context).await,
            Command::Version(version) => version.execute().await,
        }
    }
}
