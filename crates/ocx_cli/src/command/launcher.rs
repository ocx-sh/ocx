// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Hidden `launcher` group, used only by generated entry-point launchers (`exec`) and shims (`shim`).
//!
//! Each verb is a wire commitment (`launcher`, the verb, its positional shape); everything else stays
//! behind that interface.

use std::process::ExitCode;

use clap::Subcommand;

pub mod exec;
pub mod shim;

/// Internal subcommands used by generated launchers and shims.
///
/// Hidden from user-facing help output. Together they are the only stable
/// entry points from generated on-disk bodies into the OCX runtime.
#[derive(Subcommand)]
#[command(hide = true)]
pub enum Launcher {
    /// Execute an installed package entrypoint from a generated launcher.
    ///
    /// Called by generated launcher scripts as:
    ///   `ocx launcher exec '<pkg-root>' -- <argv0> [args...]`
    ///
    /// Validates the package root, forces self-view and silent presentation,
    /// then execs the resolved entrypoint binary.
    Exec(exec::LauncherExec),

    /// Materialize a deferred package and run one of its declared names.
    ///
    /// Called by generated shims as:
    ///   `ocx launcher shim '<pinned-id>' -- <argv0> [args...]`
    ///
    /// Validates the invoked name against the package's own claims, downloads
    /// the content on this first invocation, then composes the package's
    /// consumer-facing environment and execs the name on that PATH.
    Shim(shim::LauncherShim),
}

impl Launcher {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        match self {
            Launcher::Exec(exec) => exec.execute(context).await,
            Launcher::Shim(shim) => shim.execute(context).await,
        }
    }
}
