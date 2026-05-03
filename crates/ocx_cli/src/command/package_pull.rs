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
///
/// Reports the package root directory for each package — the parent of
/// `content/` and `entrypoints/`. Traverse into `<root>/content/` for the
/// installed files or `<root>/entrypoints/` for generated launchers.
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
        let oci_packages = options::Identifier::transform_all(self.packages.clone(), context.default_registry())?;
        let install_infos = context
            .manager()
            .pull_all(
                &oci_packages,
                platforms_or_default(&self.platforms),
                context.concurrency(),
            )
            .await?;

        let entries = self
            .packages
            .iter()
            .zip(install_infos.iter())
            .map(|(raw, info)| api::data::paths::PathEntry {
                package: raw.raw().to_string(),
                path: info.dir().root().to_path_buf(),
            })
            .collect();
        let paths = api::data::paths::Paths::new(entries);
        context.api().report(&paths)?;

        Ok(ExitCode::SUCCESS)
    }
}
