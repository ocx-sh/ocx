// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Subcommand;

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
}

impl ShellProfile {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        match self {
            ShellProfile::Add(add) => add.execute(context).await,
            ShellProfile::Remove(remove) => remove.execute(context).await,
            ShellProfile::List(list) => list.execute(context).await,
            ShellProfile::Load(load) => load.execute(context).await,
        }
    }
}
