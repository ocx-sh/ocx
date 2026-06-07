// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::ffi::OsString;
use std::process::ExitCode;

use clap::Subcommand;

pub mod about;
pub mod add;
pub mod clean;
pub mod config;
pub mod config_push;
pub mod config_setup;
pub mod config_update;
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
pub mod package_sign;
pub mod package_test;
pub mod patch;
pub mod patch_common;
pub mod patch_freeze;
pub mod patch_publish;
pub mod patch_sync;
pub mod patch_test;
pub mod patch_why;
pub mod pull;
pub mod remove;
pub mod run;
pub mod script_runner;
pub mod select;
pub mod self_group;
pub mod shell;
pub mod shell_completion;
pub mod toolchain_env;
pub mod uninstall;
pub mod update;
pub mod verify;
pub mod version;
pub mod which;

// ci.rs and ci_export.rs are deleted (C4 — handshake §7 / §6).
// shell_hook.rs, shell_init.rs, shell_env.rs are deleted (C4); their command
// bodies are gone and `resolve_global_current_env` is relocated into
// `command/toolchain_env.rs` (Phase 2 relocate, now wired into `ocx env`).
// install.rs global field + execute_global are deleted (C4).
// Root variants Install, Uninstall, Select, Exec, Deselect, Which, Deps are
// moved to `Package` group (C1 — handshake §2). Deselect, Which, and Deps are
// MOVEs (body preserved); `which` and `deps` are OCI-tier identifier queries
// that never read `ocx.toml`, so they belong under `ocx package`.

#[derive(Subcommand)]
pub enum Command {
    /// Compose and print the toolchain environment.
    ///
    /// Reads the in-scope `ocx.toml` + `ocx.lock`, or the global toolchain
    /// under `--global`. Defaults to a plain table; `ocx --format json env`
    /// emits JSON. `--shell[=NAME]` is the only eval-safe form.
    Env(toolchain_env::ToolchainEnv),
    /// Add one or more tool bindings to ocx.toml.
    Add(add::Add),
    /// Remove unreferenced objects from the local object store.
    Clean(clean::Clean),
    /// Manage the corporate managed-configuration tier.
    #[command(subcommand)]
    Config(config::ConfigGroup),
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
    /// Re-resolve declared tags against the registry; whole file or a subset.
    ///
    /// Resolves declared tags live against the registry by default (the root
    /// `--remote` flag is redundant here, but accepted) and records the result
    /// in `ocx.lock` only - the local index tag snapshot is never modified.
    /// A moving tag (`:latest`, `:3`) advances to wherever it points today.
    /// Under `--frozen`, resolution is capped at the local index snapshot (an
    /// unsnapshotted tag exits 81); `--offline` forbids network access. Pass
    /// binding names or `-g/--group` to advance only part of the toolchain
    /// and freeze the rest: `ocx update ripgrep` advances one binding,
    /// `ocx update -g ci` advances a whole group. A scoped update needs an
    /// existing `ocx.lock` (exit 78), refuses a drifted `ocx.toml` (exit 65),
    /// and rejects an unknown group or name (exit 64).
    Update(update::Update),
    /// Internal subcommands used by generated entry-point launchers (hidden).
    #[command(subcommand)]
    Launcher(launcher::Launcher),
    /// Operations related to packages (e.g. bundling or deploying)
    #[command(subcommand)]
    Package(package::Package),
    /// Manage patch overlays for a project.
    #[command(subcommand)]
    Patch(patch::PatchGroup),
    /// Pre-warm the object store from the project ocx.lock without creating symlinks.
    Pull(pull::Pull),
    /// Remove one or more tool bindings from ocx.toml.
    Remove(remove::Remove),
    /// Run a command with the composed environment from the project toolchain.
    Run(run::Run),
    #[command(subcommand)]
    Shell(shell::Shell),
    /// Manage the OCX installation itself (PATH activation, completions, self-update).
    #[command(name = "self", subcommand)]
    Self_(self_group::SelfGroup),
    /// Print the version of ocx
    Version(version::Version),
    /// External subcommand: dispatched to an `ocx-<name>` binary discovered on PATH.
    /// See `adr_cli_plugin_pattern.md` and `app::plugin_dispatch`.
    #[command(external_subcommand)]
    External(Vec<OsString>),
}

impl Command {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        match self {
            Command::Env(env) => env.execute(context).await,
            Command::Add(add) => add.execute(context).await,
            Command::Clean(clean) => clean.execute(context).await,
            Command::Config(config_group) => config_group.execute(context).await,
            Command::Direnv(direnv) => direnv.execute(context).await,
            Command::Index(index) => index.execute(context).await,
            Command::About(about) => about.execute(context).await,
            Command::Init(init) => init.execute(context).await,
            Command::Lock(lock) => lock.execute(context).await,
            Command::Login(login) => login.execute(context).await,
            Command::Logout(logout) => logout.execute(context).await,
            Command::Update(update) => update.execute(context).await,
            Command::Launcher(launcher) => launcher.execute(context).await,
            Command::Package(package) => package.execute(context).await,
            Command::Patch(patch_group) => patch_group.execute(context).await,
            Command::Pull(pull) => pull.execute(context).await,
            Command::Remove(remove) => remove.execute(context).await,
            Command::Run(r) => r.execute(context).await,
            Command::Shell(shell) => shell.execute(context).await,
            Command::Self_(group) => group.execute(context).await,
            Command::Version(_) => unreachable!("Version is handled in the static-command bypass in App::run"),
            Command::External(_) => {
                // External subcommands are dispatched from `App::run` before
                // `Context::try_init`, so this arm is unreachable.
                unreachable!("Command::External must be handled in App::run before reaching execute()")
            }
        }
    }
}
