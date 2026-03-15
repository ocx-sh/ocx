// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;

use ocx_lib::oci;

use crate::{api, conventions::platforms_or_default, options};

/// Downloads packages into the local object store without creating install symlinks.
///
/// Unlike [`install`](super::install), this command does not create candidate or
/// current symlinks — it only populates the content-addressed object store.
/// This is the recommended primitive for CI environments where reproducibility
/// matters and symlink management is unnecessary.
#[derive(Parser, Clone)]
pub struct PackagePull {
    #[clap(short = 'p', long = "platform", value_delimiter = ',', value_name = "PLATFORM")]
    platforms: Vec<oci::Platform>,

    /// Package identifiers to pull.
    #[arg(required = true, num_args = 1..)]
    packages: Vec<options::Identifier>,
}

impl PackagePull {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let oci_packages =
            options::Identifier::transform_all(self.packages.clone().into_iter(), context.default_registry())?;
        let install_infos = context
            .manager()
            .install_all(
                oci_packages.clone(),
                platforms_or_default(&self.platforms),
                false,
                false,
            )
            .await?;

        let entries = self
            .packages
            .iter()
            .zip(install_infos.iter())
            .map(|(raw, info)| api::data::paths::PathEntry {
                package: raw.raw().to_string(),
                path: info.content.clone(),
            })
            .collect();
        let paths = api::data::paths::Paths::new(entries);
        context.api().report(&paths)?;

        Ok(ExitCode::SUCCESS)
    }
}
