// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use crate::{api, options};
use clap::Parser;

/// Remove the installed candidate for one or more packages.
///
/// Removes the candidate symlink and its back-reference. Only the reference
/// goes; `ocx clean`, or `--purge` right away, deletes a package nothing else
/// references.
///
/// To remove the current-version symlink as well, pass `-d`/`--deselect`.
#[derive(Parser)]
pub struct Uninstall {
    /// Also remove the current symlink (equivalent to also running `ocx package deselect`).
    #[clap(short = 'd', long = "deselect")]
    deselect: bool,

    #[clap(flatten)]
    purge: options::Purge,

    /// Package identifiers to uninstall.
    #[arg(required = true, num_args = 1.., value_name = "PACKAGE")]
    packages: Vec<options::Identifier>,
}

impl Uninstall {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let identifiers = options::Identifier::transform_all(self.packages.clone(), context.default_registry())?;

        let results = context
            .manager()
            .uninstall_all(&identifiers, self.deselect, self.purge.enabled())
            .await?;

        let mut entries = Vec::new();
        for (pkg, result) in self.packages.iter().zip(results) {
            let name = pkg.raw().to_string();
            match result {
                Some(r) => {
                    entries.push(api::data::removed::RemovedEntry::new(
                        name.clone(),
                        api::data::removed::RemovedStatus::Removed,
                        Some(r.candidate),
                    ));
                    if let Some(obj_dir) = r.purged {
                        entries.push(api::data::removed::RemovedEntry::new(
                            name,
                            api::data::removed::RemovedStatus::Purged,
                            Some(obj_dir),
                        ));
                    }
                }
                None => {
                    entries.push(api::data::removed::RemovedEntry::new(
                        name,
                        api::data::removed::RemovedStatus::Absent,
                        None,
                    ));
                }
            }
        }

        context.api().report(&api::data::removed::Removed::new(entries))?;

        Ok(ExitCode::SUCCESS)
    }
}
