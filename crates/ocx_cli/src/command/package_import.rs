// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use ocx_package_manager::ImportRequest;

use crate::api::data::import::ImportReport;
use crate::{conventions, options};

/// Imports packages from an export directory without network access.
#[derive(Parser, Clone)]
pub struct PackageImport {
    /// The export directory: the one holding `ocx-export.json`, not its `index` or `blobs` subdirectory.
    #[arg(value_name = "DIR")]
    input: PathBuf,

    /// Import only this platform; repeat for several. Defaults to the host platform.
    #[arg(short = 'p', long = "platform", value_name = "PLATFORM")]
    platforms: Vec<ocx_oci::Platform>,

    /// Packages to import; without any, every package the export lists. A named package may move a tag the
    /// local index pins differently.
    #[arg(value_name = "PACKAGE")]
    packages: Vec<options::Identifier>,
}

impl PackageImport {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let packages = options::Identifier::transform_all(self.packages.clone(), context.default_registry())?;
        options::Identifier::reject_duplicate_references(&packages)?;
        let outcome = context
            .manager()
            .import(ImportRequest {
                input: conventions::absolute_dir(&self.input)?,
                packages,
                platforms: self.platforms.clone(),
            })
            .await?;
        context.api().report(&ImportReport::from_outcome(outcome))?;
        Ok(ExitCode::SUCCESS)
    }
}
