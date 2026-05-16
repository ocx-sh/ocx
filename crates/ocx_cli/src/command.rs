// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Subcommand;

pub mod about;
pub mod add;
pub mod clean;
pub mod deps;
pub mod deselect;
pub mod direnv;
pub mod direnv_export;
pub mod direnv_init;
pub mod env;
pub mod exec;
pub mod index;
pub mod index_catalog;
pub mod index_list;
pub mod index_update;
pub mod init;
pub mod install;
pub mod launcher;
pub mod lock;
pub mod login;
pub mod logout;
pub mod package;
pub mod package_create;
pub mod package_describe;
pub mod package_info;
pub mod package_inspect;
pub mod package_pull;
pub mod package_push;
pub mod package_test;
pub mod pull;
pub mod remove;
pub mod run;
pub mod select;
pub mod shell;
pub mod shell_completion;
pub mod toolchain_env;
pub mod uninstall;
pub mod upgrade;
pub mod version;
pub mod which;

// ci.rs and ci_export.rs are deleted (C4 — handshake §7 / §6).
// shell_hook.rs, shell_init.rs, shell_env.rs are deleted (C4); their command
// bodies are gone and `resolve_global_current_env` is relocated into
// `command/toolchain_env.rs` (Phase 2 relocate, now wired into `ocx env`).
// install.rs global field + execute_global are deleted (C4).
// Root variants Install, Uninstall, Select, Exec, Deselect are moved to
// `Package` group (C1 — handshake §2). Deselect is a MOVE (body preserved).

#[derive(Subcommand)]
pub enum Command {
    /// Emit the composed environment for the in-scope toolchain.
    ///
    /// Reads `ocx.toml` + `ocx.lock` (project tier) or `$OCX_HOME/ocx.toml`
    /// (when `--global` is set).  Default output: JSON (backend-first, §3).
    /// Use `--format plain` for human inspection (NOT sourceable).
    /// Use `--shell[=NAME]` for the ONLY eval-safe form.
    Env(toolchain_env::ToolchainEnv),
    /// Add a tool binding to ocx.toml.
    Add(add::Add),
    /// Remove unreferenced objects from the local object store.
    Clean(clean::Clean),
    /// Show the dependency tree for one or more packages.
    Deps(deps::Deps),
    /// Resolve packages and print their content directory paths.
    Which(which::Which),
    /// direnv integration (init writes .envrc; export emits the env block).
    Direnv(direnv::Direnv),
    /// Operations related to the package index
    #[command(subcommand)]
    Index(index::Index),
    /// Print ocx version, registry, platform, shell, and home directory.
    About(about::About),
    /// Create a minimal ocx.toml in the current directory.
    Init(init::Init),
    /// Resolve tool tags to digests and write ocx.lock.
    Lock(lock::Lock),
    /// Authenticate to a registry and persist credentials.
    Login(login::Login),
    /// Remove credentials for a registry.
    Logout(logout::Logout),
    /// Re-resolve advisory tags and rewrite ocx.lock for one or more tools.
    Upgrade(upgrade::Upgrade),
    /// Internal subcommands used by generated entry-point launchers (hidden).
    #[command(subcommand)]
    Launcher(launcher::Launcher),
    /// Operations related to packages (e.g. bundling or deploying)
    #[command(subcommand)]
    Package(package::Package),
    /// Pre-warm the object store from the project ocx.lock without creating symlinks.
    Pull(pull::Pull),
    /// Remove a tool binding from ocx.toml.
    Remove(remove::Remove),
    /// Run a command with the composed environment from the project toolchain.
    Run(run::Run),
    #[command(subcommand)]
    Shell(shell::Shell),
    /// Print the version of ocx
    Version(version::Version),
}

impl Command {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        match self {
            Command::Env(env) => env.execute(context).await,
            Command::Add(add) => add.execute(context).await,
            Command::Clean(clean) => clean.execute(context).await,
            Command::Deps(deps) => deps.execute(context).await,
            Command::Which(which) => which.execute(context).await,
            Command::Direnv(direnv) => direnv.execute(context).await,
            Command::Index(index) => index.execute(context).await,
            Command::About(about) => about.execute(context).await,
            Command::Init(init) => init.execute(context).await,
            Command::Lock(lock) => lock.execute(context).await,
            Command::Login(login) => login.execute(context).await,
            Command::Logout(logout) => logout.execute(context).await,
            Command::Upgrade(upgrade) => upgrade.execute(context).await,
            Command::Launcher(launcher) => launcher.execute(context).await,
            Command::Package(package) => package.execute(context).await,
            Command::Pull(pull) => pull.execute(context).await,
            Command::Remove(remove) => remove.execute(context).await,
            Command::Run(r) => r.execute(context).await,
            Command::Shell(shell) => shell.execute(context).await,
            Command::Version(version) => version.execute().await,
        }
    }
}
