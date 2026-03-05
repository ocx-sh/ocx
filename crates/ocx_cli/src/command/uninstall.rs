use std::process::ExitCode;

use clap::Parser;

use crate::{api, options};

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
            .uninstall_all(&identifiers, self.deselect, self.purge)?;

        let entries = self
            .packages
            .iter()
            .zip(results)
            .map(|(pkg, content_path)| {
                api::data::removed::RemovedEntry::from_result(pkg.raw().to_string(), content_path)
            })
            .collect();

        context
            .api()
            .report_removed(api::data::removed::Removed::new(entries))?;

        Ok(ExitCode::SUCCESS)
    }
}
