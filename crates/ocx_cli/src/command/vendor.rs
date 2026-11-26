// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use ocx_package_manager::{VendorRequest, VendorSelection};

use crate::api::data::vendor::VendorReport;
use crate::app::project_context::{ensure_group_segments_nonempty, load_project_with_lock};
use crate::{conventions, options};

/// Writes the project's tools, entry points and a manifest to a relocatable directory.
#[derive(Parser, Clone)]
pub struct Vendor {
    /// Directory to write; absent or empty.
    #[arg(short = 'o', long = "output", value_name = "DIR", required = true)]
    output: PathBuf,

    #[clap(flatten)]
    platform: options::PlatformOption,

    /// Vendor only the named group(s); repeatable and comma-separated. Defaults to the default group.
    #[clap(flatten)]
    groups: options::GroupSelection,

    /// Write `manifest.json` only: no package content and no launcher.
    #[arg(long = "manifest-only")]
    manifest_only: bool,

    /// Create no symlink under the directory.
    #[arg(long = "omit-links")]
    omit_links: bool,

    /// Tool names to vendor; every tool of the selected groups when omitted.
    #[arg(value_name = "NAME")]
    names: Vec<String>,
}

impl Vendor {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        ensure_group_segments_nonempty(self.groups.names())?;
        let project = load_project_with_lock(&context).await?;
        let groups = self.groups.effective(&project.config)?;
        let outcome = context
            .manager()
            .vendor(VendorRequest {
                output: conventions::absolute_dir(&self.output)?,
                platform: conventions::platform_or_default(self.platform.platform.clone()),
                selection: VendorSelection::Project {
                    tools: project.lock.tools.clone(),
                    groups,
                    names: self.names.clone(),
                },
                manifest_only: self.manifest_only,
                omit_links: self.omit_links,
            })
            .await?;
        context.api().report(&VendorReport::from_outcome(outcome)?)?;
        Ok(ExitCode::SUCCESS)
    }
}
