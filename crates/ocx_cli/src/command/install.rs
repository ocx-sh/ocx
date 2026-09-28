// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

// No `--global`: `ocx package install --global` is a deliberate usage error (64); the toolchain-tier
// form is `ocx --global add` (`handshake_toolchain_cli.md`).

use std::process::ExitCode;

use clap::Parser;

use crate::{api, conventions, options};

#[derive(Parser, Clone)]
pub struct Install {
    /// Also set the installed version as current (creates the current symlink)
    #[clap(short = 's', long = "select")]
    select: bool,

    #[clap(flatten)]
    platform: options::PlatformOption,

    #[clap(flatten)]
    verify: options::SignatureVerify,

    /// Package identifiers to install.
    #[arg(required = true, num_args = 1..)]
    packages: Vec<options::Identifier>,
}

impl Install {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let oci_packages = options::Identifier::transform_all(self.packages.clone(), context.default_registry())?;
        log::info!(
            "Installing packages: {}",
            oci_packages
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
        let manager = crate::conventions::manager_with_verify_flag(&context, &self.verify);
        let install_infos = manager
            .install_all(
                oci_packages.clone(),
                conventions::platform_or_default(self.platform.platform.clone()),
                true,
                self.select,
                context.concurrency(),
                false, // user-requested install — run patch discovery
            )
            .await?;

        let fs = context.file_structure();
        let packages = self
            .packages
            .iter()
            .zip(oci_packages.iter())
            .zip(install_infos.iter())
            .map(|((raw, oci_pkg), info)| {
                // The symlink actually written: none for a foreign-platform install (the
                // `wire_selection` gate), `current` under `--select`, else the candidate.
                let path = if !info.is_host_runnable() {
                    None
                } else if self.select {
                    Some(fs.symlinks.current(oci_pkg))
                } else {
                    Some(fs.symlinks.candidate(oci_pkg))
                };
                (
                    raw.raw().to_string(),
                    api::data::install::InstallEntry {
                        identifier: info.identifier().clone().into(),
                        metadata: info.metadata().clone(),
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
