use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{Error, log, reference_manager::ReferenceManager};

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

        let fs = context.file_structure().clone();
        let rm = ReferenceManager::new(fs.clone());

        for identifier in &identifiers {
            log::debug!("Deselecting package '{}'.", identifier);

            if identifier.digest().is_some() {
                return Err(Error::PackageSymlinkRequiresTag(identifier.clone()).into());
            }

            let current_path = fs.installs.current(identifier);

            if current_path.is_symlink() {
                rm.unlink(&current_path)?;
            } else {
                log::warn!(
                    "Package '{}' has no current symlink at '{}' — nothing to deselect.",
                    identifier,
                    current_path.display(),
                );
            }
        }

        context.api().report_removed(api::data::removed::Removed::new(
            self.packages.iter().map(|p| p.raw().to_string()).collect(),
        ))?;

        Ok(ExitCode::SUCCESS)
    }
}
