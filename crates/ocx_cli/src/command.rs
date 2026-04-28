// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Subcommand;

pub mod ci;
pub mod ci_export;
pub mod clean;
pub mod deps;
pub mod deselect;
pub mod env;
pub mod exec;
pub mod find;
pub mod generate;
pub mod generate_direnv;
pub mod hook_env;
pub mod index;
pub mod index_catalog;
pub mod index_list;
pub mod index_update;
pub mod info;
pub mod install;
pub mod lock;
pub mod package;
pub mod package_create;
pub mod package_describe;
pub mod package_info;
pub mod package_pull;
pub mod package_push;
pub mod pull;
pub mod select;
pub mod shell;
pub mod shell_completion;
pub mod shell_env;
pub mod shell_hook;
pub mod shell_init;
pub mod shell_profile;
pub mod shell_profile_add;
pub mod shell_profile_generate;
pub mod shell_profile_list;
pub mod shell_profile_load;
pub mod shell_profile_remove;
pub mod uninstall;
pub mod update;
pub mod version;

#[derive(Subcommand)]
pub enum Command {
    /// CI-specific commands (e.g. exporting environment variables to CI systems).
    #[command(subcommand)]
    Ci(ci::Ci),
    /// Remove unreferenced objects from the local object store.
    Clean(clean::Clean),
    /// Show the dependency tree for one or more packages.
    Deps(deps::Deps),
    /// Remove the current-version symlink for one or more packages.
    Deselect(deselect::Deselect),
    /// Resolve packages and print their content directory paths.
    Find(find::Find),
    /// Generate scaffolding files for project integration (e.g. direnv).
    #[command(subcommand)]
    Generate(generate::Generate),
    /// Operations related to the package index
    #[command(subcommand)]
    Index(index::Index),
    /// Print ocx version and build information
    Info(info::Info),
    /// Install packages from a local or remote index.
    Install(install::Install),
    /// Resolve tool tags to digests and write ocx.lock.
    Lock(lock::Lock),
    /// Remove an installed candidate for one or more packages.
    Uninstall(uninstall::Uninstall),
    /// Re-resolve advisory tags and rewrite ocx.lock for one or more tools.
    Update(update::Update),
    /// Runs installed packages.
    Exec(exec::Exec),
    /// Print resolved environment variables for one or more packages.
    Env(env::Env),
    /// Print stateful shell exports for the project toolchain (prompt-hook entry point).
    HookEnv(hook_env::HookEnv),
    /// Operations related to packages (e.g. bundling or deploying)
    #[command(subcommand)]
    Package(package::Package),
    /// Pre-warm the object store from the project ocx.lock without creating symlinks.
    Pull(pull::Pull),
    /// Set the current version of one or more packages.
    Select(select::Select),
    #[command(subcommand)]
    Shell(shell::Shell),
    /// Print stateless shell exports for the project toolchain (direnv entry point).
    ShellHook(shell_hook::ShellHook),
    /// Print the version of ocx
    Version(version::Version),
}

impl Command {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        match self {
            Command::Ci(ci) => ci.execute(context).await,
            Command::Clean(clean) => clean.execute(context).await,
            Command::Deps(deps) => deps.execute(context).await,
            Command::Deselect(deselect) => deselect.execute(context).await,
            Command::Find(find) => find.execute(context).await,
            Command::Generate(generate) => generate.execute(context).await,
            Command::Index(index) => index.execute(context).await,
            Command::Info(info) => info.execute(context).await,
            Command::Install(install) => install.execute(context).await,
            Command::Lock(lock) => lock.execute(context).await,
            Command::Uninstall(uninstall) => uninstall.execute(context).await,
            Command::Update(update) => update.execute(context).await,
            Command::Exec(exec) => exec.execute(context).await,
            Command::Env(env) => env.execute(context).await,
            Command::HookEnv(hook_env) => hook_env.execute(context).await,
            Command::Package(package) => package.execute(context).await,
            Command::Pull(pull) => pull.execute(context).await,
            Command::Select(select) => select.execute(context).await,
            Command::Shell(shell) => shell.execute(context).await,
            Command::ShellHook(shell_hook) => shell_hook.execute(context).await,
            Command::Version(version) => version.execute().await,
        }
    }
}
