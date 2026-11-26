// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use ocx_package_manager::ExportRequest;

use crate::api::data::export::ExportReport;
use crate::{conventions, options};

/// Writes packages and their blobs to a directory for offline transfer.
#[derive(Parser, Clone)]
pub struct PackageExport {
    /// Where to write the export: a directory, or the archive file with --archive; created when absent
    #[arg(short = 'o', long = "output", value_name = "PATH", required = true)]
    output: PathBuf,

    /// Write one archive file instead of a directory
    ///
    /// The format follows the file name: `.zip`, `.tar.gz`, `.tar.xz` or `.tar.zst`, and a plain tar for any
    /// other name. An existing file is replaced; an existing directory at the path is refused.
    #[arg(long)]
    archive: bool,

    /// Export only this platform; repeat for several. Defaults to every platform the package publishes.
    #[arg(short = 'p', long = "platform", value_name = "PLATFORM")]
    platforms: Vec<ocx_oci::Platform>,

    #[clap(flatten)]
    referrers: options::ExportReferrers,

    /// Packages to export, as `registry/repository:tag` or `registry/repository@sha256:<hex>`.
    #[arg(required = true, num_args = 1.., value_name = "PACKAGE")]
    packages: Vec<options::Identifier>,
}

impl PackageExport {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let packages = options::Identifier::transform_all(self.packages.clone(), context.default_registry())?;
        options::Identifier::reject_duplicate_references(&packages)?;
        conventions::warn_archive_name_without_flag(context.ui(), &self.output, self.archive);
        let outcome = context
            .manager()
            .export(ExportRequest {
                output: conventions::absolute_dir(&self.output)?,
                packages,
                lock_pins: Vec::new(),
                platforms: self.platforms.clone(),
                referrers: self.referrers.enabled(),
                archive: self.archive,
            })
            .await?;
        context.api().report(&ExportReport::from_outcome(outcome)?)?;
        Ok(ExitCode::SUCCESS)
    }
}
