// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::package_manager::ProfileEntryResolution;

use crate::api;

/// List all packages in the shell profile with their status.
///
/// Shows each profiled package, its resolution mode, whether the corresponding
/// symlink is active or broken, and the resolved path.
#[derive(Parser)]
pub struct ShellProfileList {}

impl ShellProfileList {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let manifest = context.manager().profile().load()?;

        let resolutions = context.manager().resolve_profile_all(&manifest.packages).await;

        let entries: Vec<_> = resolutions
            .into_iter()
            .map(|resolution| match resolution {
                ProfileEntryResolution::Resolved(entry) => api::data::profile::ProfileListEntry::new(
                    entry.identifier.to_string(),
                    entry.mode,
                    api::data::profile::ProfileStatus::Active,
                    Some(entry.content_path),
                ),
                ProfileEntryResolution::Broken {
                    identifier, mode, path, ..
                } => api::data::profile::ProfileListEntry::new(
                    identifier.to_string(),
                    mode,
                    api::data::profile::ProfileStatus::Broken,
                    path,
                ),
            })
            .collect();

        context.api().report(&api::data::profile::ProfileList::new(entries))?;

        Ok(ExitCode::SUCCESS)
    }
}
