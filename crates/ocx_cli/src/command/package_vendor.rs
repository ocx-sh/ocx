// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use ocx_package_manager::{VendorRequest, VendorSelection};

use crate::api::data::vendor::VendorReport;
use crate::{conventions, options};

/// Writes packages, launchers and a manifest to a relocatable directory.
#[derive(Parser, Clone)]
pub struct PackageVendor {
    /// Directory to write; absent or empty.
    #[arg(short = 'o', long = "output", value_name = "DIR", required = true)]
    output: PathBuf,

    #[clap(flatten)]
    platform: options::PlatformOption,

    /// Write `manifest.json` only: no package content and no launcher.
    #[arg(long = "manifest-only")]
    manifest_only: bool,

    /// Create no symlink under the directory.
    #[arg(long = "omit-links")]
    omit_links: bool,

    /// Packages to vendor, as `registry/repository:tag` or `registry/repository@sha256:<hex>`.
    #[arg(required = true, num_args = 1.., value_name = "PACKAGE")]
    packages: Vec<options::Identifier>,
}

impl PackageVendor {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let packages = options::Identifier::transform_all(self.packages.clone(), context.default_registry())?;
        options::Identifier::reject_duplicate_references(&packages)?;
        let outcome = context
            .manager()
            .vendor(VendorRequest {
                output: conventions::absolute_dir(&self.output)?,
                platform: conventions::platform_or_default(self.platform.platform.clone()),
                selection: VendorSelection::Packages(packages),
                manifest_only: self.manifest_only,
                omit_links: self.omit_links,
            })
            .await?;
        context.api().report(&VendorReport::from_outcome(outcome)?)?;
        Ok(ExitCode::SUCCESS)
    }
}
