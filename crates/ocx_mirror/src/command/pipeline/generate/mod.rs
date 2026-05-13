// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx-mirror pipeline generate` subcommand group.
//!
//! Currently contains a single subcommand `ci`; the group structure leaves
//! room for future `generate <other>` targets (e.g. `generate devcontainer`).

pub mod ci;

/// Dispatcher for `ocx-mirror pipeline generate <subcommand>`.
#[derive(clap::Subcommand)]
pub enum GenerateCommand {
    /// Generate CI workflow files from a mirror spec.
    Ci(ci::GenerateCi),
}
