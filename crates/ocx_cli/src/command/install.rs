// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;

use ocx_lib::{log, oci};

use crate::{api, conventions::platforms_or_default, options};

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
        let install_infos = context
            .manager()
            .install_all(
                oci_packages.clone(),
                platforms_or_default(&self.platforms),
                true,
                self.select,
            )
            .await?;

        let fs = context.file_structure();
        let packages = self
            .packages
            .iter()
            .zip(oci_packages.iter())
            .zip(install_infos.iter())
            .map(|((raw, oci_pkg), info)| {
                let path = fs.installs.candidate(oci_pkg);
                (
                    raw.raw().to_string(),
                    api::data::install::InstallEntry {
                        identifier: info.identifier.clone(),
                        metadata: info.metadata.clone(),
                        path,
                    },
                )
            })
            .collect();
        let install_data = api::data::install::Installs::new(packages);
        context.api().report(&install_data)?;

        Ok(ExitCode::SUCCESS)
    }
}
