// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Hidden `launcher` subcommand group.
//!
//! This group is hidden from `ocx --help` (`#[command(hide = true)]`) — it is
//! an internal-only API used exclusively by generated entry-point launchers.
//! Hiding prevents it from appearing in user-facing help output while still
//! allowing `ocx launcher --help` to work for debugging.
//!
//! The stable surface is exactly two wire commitments: the `launcher` + `exec`
//! subcommand names. All implementation details (presentation flags, self-view
//! selection, binary pinning) are encapsulated behind this interface.

use std::process::ExitCode;

use clap::Subcommand;

pub mod exec;

/// Internal subcommands used by generated entry-point launchers.
///
/// Hidden from user-facing help output. The `exec` subcommand is the sole
/// stable entry point from installed launchers into the OCX runtime.
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
}

impl Launcher {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        match self {
            Launcher::Exec(exec) => exec.execute(context).await,
        }
    }
}
