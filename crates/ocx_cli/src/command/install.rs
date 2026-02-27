use std::process::ExitCode;

use clap::Parser;

use ocx_lib::{log, oci};

use crate::{api, conventions::platforms_or_default, options, task};

#[derive(Parser, Clone)]
pub struct Install {
    /// Force overwrite of output file if it already exists
    #[clap(short = 's', long = "select")]
    select: bool,

    #[clap(short = 'p', long = "platform", value_delimiter = ',', value_name = "PLATFORM")]
    platforms: Vec<oci::Platform>,

    /// Package identifiers to install.
    #[arg(required = true, num_args = 1..)]
    packages: Vec<options::Identifier>,
}

impl Install {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let oci_packages =
            options::Identifier::transform_all(self.packages.clone().into_iter(), context.default_registry())?;
        log::info!(
            "Installing packages: {}",
            oci_packages
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
        let install_infos = task::package::install::Install {
            context: context.clone(),
            file_structure: context.file_structure().clone(),
            platforms: platforms_or_default(&self.platforms),
            candidate: true,
            select: self.select,
        }
        .install_all(oci_packages.clone())
        .await?;

        let install_data = api::data::install::InstallCollection::new(
            self.packages
                .iter()
                .map(|p| p.raw().to_string())
                .zip(install_infos.iter().cloned())
                .collect(),
        );
        context.api().report_installs(install_data)?;

        Ok(ExitCode::SUCCESS)
    }
}
