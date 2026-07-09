// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;

use crate::{api, conventions, options};

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
    #[clap(flatten)]
    platform: options::PlatformOption,

    #[clap(flatten)]
    verify: options::Verify,

    /// Package identifiers to pull.
    #[arg(required = true, num_args = 1..)]
    packages: Vec<options::Identifier>,
}

impl PackagePull {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let oci_packages = options::Identifier::transform_all(self.packages.clone(), context.default_registry())?;
        // Auto-verify is attached on the shared manager (Context::try_init);
        // refine its opt-out from this command's --verify/--no-verify flag.
        let manager = crate::conventions::manager_with_verify_flag(&context, &self.verify);
        let install_infos = manager
            .pull_all(
                &oci_packages,
                conventions::platform_or_default(self.platform.platform.clone()),
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
