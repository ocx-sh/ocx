// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{ci::CiFlavor, log};

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
    /// Expose the package's full env, including private (self-only) entries.
    /// See `ocx exec --help` for full view semantics.
    ///
    /// Generated launchers embed `--self`; avoid passing it directly unless
    /// building a launcher equivalent.
    #[clap(long = "self", default_value_t = false)]
    self_view: bool,

    #[clap(flatten)]
    platforms: options::Platforms,

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
        warn_if_pathext_missing_launcher();
        let flavor = match self.flavor {
            Some(f) => f,
            None => CiFlavor::detect()
                .ok_or_else(|| anyhow::anyhow!("could not detect CI environment; use --flavor to specify"))?,
        };
        log::debug!("Using CI flavor: {}", flavor);

        let manager = context.manager();
        let package_infos = resolve_packages(
            self.packages.clone(),
            self.platforms.as_slice(),
            &self.content_path,
            manager,
            context.default_registry(),
        )
        .await?;

        let package_infos: Vec<std::sync::Arc<ocx_lib::package::install_info::InstallInfo>> =
            package_infos.into_iter().map(std::sync::Arc::new).collect();
        let entries = manager.resolve_env(&package_infos, self.self_view).await?;

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
