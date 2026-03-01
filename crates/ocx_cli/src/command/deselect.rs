use std::process::ExitCode;

use clap::Parser;

use crate::{api, options};

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
        let identifiers = options::Identifier::transform_all(
            self.packages.clone().into_iter(),
            context.default_registry(),
        )?;

        context.manager().deselect_all(&identifiers)?;

        context.api().report_removed(api::data::removed::Removed::new(
            self.packages.iter().map(|p| p.raw().to_string()).collect(),
        ))?;

        Ok(ExitCode::SUCCESS)
    }
}
