// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use ocx_lib::profile::ProfileMode;

use crate::{api, options};
use clap::Parser;

/// Remove the installed candidate for one or more packages.
///
/// Removes the candidate symlink and its back-reference.  The object-store
/// content is left on disk unless `--purge` is given, in which case the
/// object directory is deleted when no other references remain.
///
/// To remove the current-version symlink as well, pass `-d`/`--deselect`.
/// To clean up all unreferenced objects at once use `ocx clean`.
#[derive(Parser)]
pub struct Uninstall {
    /// Also remove the current symlink (equivalent to also running `ocx deselect`).
    #[clap(short = 'd', long = "deselect")]
    deselect: bool,

    /// Delete the object from the store when no references remain after uninstall.
    #[clap(long = "purge")]
    purge: bool,

    /// Package identifiers to uninstall.
    #[arg(required = true, num_args = 1.., value_name = "PACKAGE")]
    packages: Vec<options::Identifier>,
}

impl Uninstall {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let identifiers =
            options::Identifier::transform_all(self.packages.clone().into_iter(), context.default_registry())?;

        let results = context
            .manager()
            .uninstall_all(&identifiers, self.deselect, self.purge)
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

        let snapshot = context.manager().profile().snapshot();
        for identifier in &identifiers {
            for entry in snapshot.entries_for(identifier) {
                match entry.mode {
                    ProfileMode::Candidate => tracing::warn!(
                        "{} is in your shell profile in candidate mode. \
                         Re-install with `ocx install {}` or remove with \
                         `ocx shell profile remove {}`.",
                        entry.identifier,
                        entry.identifier,
                        entry.identifier,
                    ),
                    ProfileMode::Current => tracing::warn!(
                        "{} is in your shell profile in current mode. \
                         Re-select with `ocx select {}` or switch to \
                         `ocx shell profile add --candidate {}`.",
                        entry.identifier,
                        entry.identifier,
                        entry.identifier,
                    ),
                    ProfileMode::Content => tracing::warn!(
                        "{} is in your shell profile in content mode. \
                         Re-install with `ocx install {}` or remove with \
                         `ocx shell profile remove {}`.",
                        entry.identifier,
                        entry.identifier,
                        entry.identifier,
                    ),
                }
            }
        }

        context.api().report(&api::data::removed::Removed::new(entries))?;

        Ok(ExitCode::SUCCESS)
    }
}
