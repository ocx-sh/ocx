// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{ci::CiFlavor, log, oci};

use crate::{api, conventions::*, options};

/// Export package environment variables in CI-specific format.
///
/// Writes environment variable definitions directly to the CI system's runtime
/// files. For GitHub Actions, this appends to `$GITHUB_PATH` and `$GITHUB_ENV`.
///
/// The CI flavor is auto-detected from the environment (e.g. `$GITHUB_ACTIONS`)
/// but can be overridden with `--flavor`.
///
/// This command does not auto-install packages. If a package is not already
/// available locally it will fail with an error.
#[derive(Parser)]
pub struct CiExport {
    /// Target platforms to consider when resolving packages.
    #[clap(short = 'p', long = "platform", value_delimiter = ',', value_name = "PLATFORM")]
    platforms: Vec<oci::Platform>,

    /// CI system to generate export commands for. Auto-detected if omitted.
    #[clap(long = "flavor")]
    flavor: Option<CiFlavor>,

    #[clap(flatten)]
    content_path: options::ContentPath,

    /// Package identifiers to export the environment for.
    #[arg(required = true, num_args = 1.., value_name = "PACKAGE")]
    packages: Vec<options::Identifier>,
}

impl CiExport {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let flavor = match self.flavor {
            Some(f) => f,
            None => CiFlavor::detect()
                .ok_or_else(|| anyhow::anyhow!("Could not detect CI environment. Use --flavor to specify."))?,
        };
        log::debug!("Using CI flavor: {}", flavor);

        let platforms = platforms_or_default(&self.platforms);
        let identifiers =
            options::Identifier::transform_all(self.packages.clone().into_iter(), context.default_registry())?;

        let manager = context.manager();

        let package_infos = if let Some(kind) = self.content_path.symlink_kind() {
            manager.find_symlink_all(identifiers, kind).await?
        } else {
            manager.find_all(identifiers, platforms).await?
        };

        let entries = resolve_env_entries(&package_infos)?;

        flavor.export(&entries)?;

        let report_entries = entries
            .into_iter()
            .map(|e| api::data::env::EnvEntry {
                key: e.key,
                value: e.value,
                kind: e.kind,
            })
            .collect();
        context
            .api()
            .report(&api::data::ci_export::CiExported::new(report_entries))?;

        Ok(ExitCode::SUCCESS)
    }
}
