// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;

use crate::api::data::profile_removed::{ProfileRemoved, ProfileRemovedEntry, ProfileRemovedStatus};
use crate::options;

/// Remove one or more packages from the shell profile.
///
/// The package is removed from `profile.json` so it will no longer be loaded
/// at shell startup. This does not uninstall the package.
#[derive(Parser)]
pub struct ShellProfileRemove {
    /// Package identifiers to remove from the profile.
    #[arg(required = true, num_args = 1.., value_name = "PACKAGE")]
    packages: Vec<options::Identifier>,
}

impl ShellProfileRemove {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let default_registry = context.default_registry();
        let identifiers = options::Identifier::transform_all(self.packages.clone().into_iter(), default_registry)?;

        let id_strings: Vec<String> = identifiers.iter().map(|id| id.to_string()).collect();
        let results = context.manager().profile().remove_all(&id_strings)?;

        let entries: Vec<_> = id_strings
            .into_iter()
            .zip(results)
            .map(|(id_str, removed)| {
                let status = if removed {
                    ProfileRemovedStatus::Removed
                } else {
                    ProfileRemovedStatus::Absent
                };
                ProfileRemovedEntry::new(id_str, status)
            })
            .collect();

        context.api().report_profile_removed(ProfileRemoved::new(entries))?;
        Ok(ExitCode::SUCCESS)
    }
}
