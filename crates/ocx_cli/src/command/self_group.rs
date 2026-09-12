// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Subcommand;

pub mod activate;
pub mod setup;
pub mod update;

/// Subcommands for managing the OCX installation itself.
///
/// Exposed as `ocx self` (clap rename avoids the `self` Rust keyword). This
/// group owns PATH activation, shell-completion injection, and binary
/// self-update.
#[derive(Subcommand)]
pub enum SelfGroup {
    /// Sourced from `$OCX_HOME/env.sh` at shell startup to activate ocx in the
    /// current shell. Prepends `$OCX_HOME/symlinks/.../bin` to `PATH`, injects
    /// completions (unless `OCX_NO_COMPLETIONS=1`), and evaluates the global
    /// toolchain env. Safe to re-source: the PATH updates are idempotent
    /// (move-to-front), so a re-source never duplicates an entry.
    Activate(activate::SelfActivate),
    /// Create or refresh ocx shell integration.
    ///
    /// Completes a bare-binary install: installs the latest published ocx into
    /// the content store, writes the per-shell env shims into `$OCX_HOME`, and
    /// adds a managed activation block to your shell profiles. Pass an
    /// optional VERSION to install a specific release instead of the latest.
    ///
    /// Setup also registers the ocx install `bin` directory and
    /// `$OCX_HOME/toolchain/active/bin` on the session PATH - the Windows user
    /// environment, an `environment.d` drop-in on Linux, a login LaunchAgent
    /// on macOS - so processes started outside a shell find them too, such as
    /// desktop launchers, IDEs and services.
    ///
    /// Neither change reaches a session that is already running. Terminals
    /// that are already open see nothing until they are restarted, and the
    /// session PATH reaches new processes only after the next login.
    ///
    /// A session-PATH location that cannot be written is reported and warned
    /// about, never fatal: setup still exits 0, and re-running it retries. The
    /// one exception is an `$OCX_HOME` this platform's PATH format cannot
    /// spell, which is refused before anything is written (exit 78).
    ///
    /// Safe to re-run: the shims and profile blocks are diff-gated, so an
    /// unchanged setup is a no-op. Pass `--dry-run` to preview,
    /// `--no-modify-path` to skip the profiles and the session PATH alike, and
    /// `--force` to overwrite a managed block you have edited by hand
    /// (otherwise exit 82). Pass `--no-profile` to skip only the profile
    /// blocks, or `--profile PATH` to target specific files instead of the
    /// detected ones.
    ///
    /// `--no-modify-path` and `--profile` / `--no-profile` persist as `[shell]
    /// modify_path` and `[shell] profiles` in config.toml, so the choice
    /// applies to every later run too - not only this one.
    ///
    /// https://ocx.sh/docs/user-guide#install-bare-binary
    Setup(setup::SelfSetup),
    /// Update ocx itself to the latest released version. Without `--check`,
    /// downloads the newest release and activates it. With `--check`, reports
    /// the result without installing.
    ///
    /// Downloading and activating are two separate steps: the newest release
    /// is pulled first, then the new binary activates itself by running its
    /// own `ocx self setup`. If that inner setup cannot finish - for example a
    /// shell profile carries edits it refuses to overwrite - the download
    /// stands but nothing is activated: exit 75 reports this as pulled rather
    /// than installed, and running `ocx self setup` again finishes the job.
    ///
    /// The latest version is looked up live from the published index rather
    /// than your local index, so the freshest release is always found;
    /// `--offline` skips the check. Both forms always bypass the auto-check
    /// throttle.
    Update(update::SelfUpdate),
}

impl SelfGroup {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        match self {
            // QUAL-6: Activate is always dispatched context-free from app.rs before
            // Context::try_init — see the `should_check_for_update` bypass list.
            // If this arm fires, the bypass list no longer covers Activate, which
            // is a bug: the panic surfaces it immediately.
            SelfGroup::Activate(_) => {
                unreachable!(
                    "SelfGroup::Activate is always dispatched via app.rs::App::run before Context::try_init — see should_check_for_update bypass list"
                )
            }
            SelfGroup::Setup(setup) => setup.execute(context).await,
            SelfGroup::Update(update) => update.execute(context).await,
        }
    }
}
