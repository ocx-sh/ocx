// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use crate::{api, options};
use clap::Parser;

/// Remove the current-version symlink for one or more packages.
///
/// The package is deselected but not uninstalled: its candidate symlink and
/// object-store content remain intact.  To also remove the installed files
/// use `ocx uninstall`.
#[derive(Parser)]
pub struct Deselect {
    /// Package identifiers to deselect.
    #[arg(required = true, num_args = 1.., value_name = "PACKAGE")]
    packages: Vec<options::Identifier>,
}

impl Deselect {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let identifiers = options::Identifier::transform_all(self.packages.clone(), context.default_registry())?;

        let results = context.manager().deselect_all(&identifiers).await?;

        let entries = self
            .packages
            .iter()
            .zip(results)
            .map(|(pkg, symlink_path)| {
                let name = pkg.raw().to_string();
                let status = if symlink_path.is_some() {
                    api::data::removed::RemovedStatus::Removed
                } else {
                    api::data::removed::RemovedStatus::Absent
                };
                api::data::removed::RemovedEntry::new(name, status, symlink_path)
            })
            .collect();

        context.api().report(&api::data::removed::Removed::new(entries))?;

        Ok(ExitCode::SUCCESS)
    }
}
