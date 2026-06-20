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
    /// Create or refresh ocx shell integration. Installs the latest published
    /// ocx into the content store, writes the per-shell env shims, and adds a
    /// managed activation block to your shell profiles. Safe to re-run; pass
    /// `--dry-run` to preview, `--no-modify-path` to skip profile edits, and
    /// `--force` to overwrite a block you have edited (otherwise exit 82).
    Setup(setup::SelfSetup),
    /// Update ocx itself to the latest released version. Without `--check`,
    /// installs the new binary if one is available. With `--check`, reports the
    /// result without installing.
    ///
    /// The latest version is resolved through the local index, so `--offline`,
    /// `--frozen`, and `OCX_INDEX` apply; pass `--remote` to query the registry
    /// directly. Both forms always bypass the auto-check throttle.
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
