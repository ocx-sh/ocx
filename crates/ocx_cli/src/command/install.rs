// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

// C4 (plan_toolchain_cli.md Phase 1): the `global: bool` field, the
// `if self.global { return self.execute_global(context).await; }` dispatch
// branch, and the entire `execute_global` method are deleted.
//
// `ocx install --global <pkg>` no longer exists (handshake §7 — this was the
// ONE `--global` site from a4211591 that does NOT survive).  The toolchain-tier
// equivalent is `ocx --global add <pkg>` (which auto-initialises the global
// file, re-locks, installs, and selects).
//
// `install` itself is moved from root `Command` to `Package::Install` (C1).
// `ocx package install --global` → clap unknown-flag error (exit 64) because
// `--global` is not declared on this struct; ocx maps clap usage errors → EX_USAGE 64.

use std::process::ExitCode;

use clap::Parser;

use ocx_lib::log;

use crate::{api, conventions, options};

#[derive(Parser, Clone)]
pub struct Install {
    /// Also set the installed version as current (creates the current symlink)
    #[clap(short = 's', long = "select")]
    select: bool,

    #[clap(flatten)]
    platform: options::PlatformOption,

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
        let install_infos = context
            .manager()
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
                // Report the symlink actually written. A foreign-platform install
                // writes neither host pointer (issue #179), so surface no path;
                // otherwise `--select` moves the `current` pointer while a plain
                // install writes the tag-pinned candidate. The host-runnable check
                // is the same gate `wire_selection` applied, so the report never
                // claims a path that was suppressed.
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
