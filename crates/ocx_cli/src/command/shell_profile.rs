// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Subcommand;

/// Emit a one-line deprecation note to stderr for legacy interactive
/// `shell profile` subcommands (`add`, `remove`, `list`).
///
/// Single source of truth for the wording — every legacy interactive
/// subcommand calls this as its first line so the note is consistent.
/// Stderr only — never affects stdout, never changes exit codes.
///
/// Intentionally NOT emitted from:
/// - `shell profile generate` — the new (forward-looking) path; emitting
///   the note would be self-referential.
/// - `shell profile load` — consumed by `eval "$(... shell profile load)"`
///   in shell init; a stderr note would spam every new terminal.
pub(super) fn emit_deprecation_note() {
    eprintln!(
        "Note: 'shell profile' is deprecated; use 'ocx shell profile generate' (file-based) or 'ocx shell init' (project-level toolchain). Migration: ocx.sh/docs/migration"
    );
}

#[derive(Subcommand)]
pub enum ShellProfile {
    /// Add one or more packages to the shell profile.
    Add(super::shell_profile_add::ShellProfileAdd),
    /// Remove one or more packages from the shell profile.
    Remove(super::shell_profile_remove::ShellProfileRemove),
    /// List all packages in the shell profile with their status.
    List(super::shell_profile_list::ShellProfileList),
    /// Output shell export statements for all profiled packages.
    Load(super::shell_profile_load::ShellProfileLoad),
    /// Generate a shell init file containing all profile exports.
    Generate(super::shell_profile_generate::ShellProfileGenerate),
}

impl ShellProfile {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        match self {
            ShellProfile::Add(add) => add.execute(context).await,
            ShellProfile::Remove(remove) => remove.execute(context).await,
            ShellProfile::List(list) => list.execute(context).await,
            ShellProfile::Load(load) => load.execute(context).await,
            ShellProfile::Generate(generate) => generate.execute(context).await,
        }
    }
}
