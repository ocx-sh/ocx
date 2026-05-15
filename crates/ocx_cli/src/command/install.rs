// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;

use ocx_lib::log;

use crate::{api, conventions::platforms_or_default, options};

#[derive(Parser, Clone)]
pub struct Install {
    /// Also set the installed version as current (creates the current symlink)
    #[clap(short = 's', long = "select")]
    select: bool,

    #[clap(flatten)]
    platforms: options::Platforms,

    /// Also record into and select for the global toolchain (implies
    /// --select for the global PATH).
    ///
    /// `ocx install --global <pkg>` is sugar for
    /// `ocx add --global <pkg>` + re-lock the global file + install
    /// **and** select the resolved package (it becomes a `current`
    /// symlink on the global PATH) — so `--global` implies `--select`
    /// even without `-s`, unlike plain `install`. `install`/`select`
    /// stay OCI-tier primitives; `--global` only adds the
    /// global-toolchain recording + selection effect
    /// (adr_global_toolchain_tier.md §Decision 3). Mutually exclusive
    /// with `--project`.
    #[arg(long)]
    global: bool,

    /// Package identifiers to install.
    #[arg(required = true, num_args = 1..)]
    packages: Vec<options::Identifier>,
}

impl Install {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        if self.global {
            return self.execute_global(context).await;
        }

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
                platforms_or_default(self.platforms.as_slice()),
                true,
                self.select,
                context.concurrency(),
            )
            .await?;

        let fs = context.file_structure();
        let packages = self
            .packages
            .iter()
            .zip(oci_packages.iter())
            .zip(install_infos.iter())
            .map(|((raw, oci_pkg), info)| {
                let path = fs.symlinks.candidate(oci_pkg);
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

    /// `ocx install --global <pkg>...` = `ocx add --global <pkg>`
    /// (auto-init the global file when absent — F7) + re-lock global +
    /// install **and select** (the resolved package becomes a `current`
    /// symlink on the global PATH) — adr_global_toolchain_tier.md
    /// §Decision 3.
    ///
    /// Reuses [`crate::command::Add`]'s mutate path verbatim (exclusivity
    /// seam, F7 auto-init, `add_binding`, re-lock, install) and the
    /// existing wire-selection primitive `ocx select` uses — it does NOT
    /// fork a parallel install pipeline (feedback_extend_dont_duplicate).
    /// `--global` only adds the "also record into and select for the
    /// global toolchain" effect on top of the OCI-tier install/select
    /// primitives.
    async fn execute_global(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // Resolve raw args → fully-qualified OCI identifiers once, up
        // front, so the post-add select step targets the exact resolved
        // package (not the raw arg).
        let oci_packages = options::Identifier::transform_all(self.packages.clone(), context.default_registry())?;

        // Add+lock+install each binding through the `Add` command's
        // existing path. `Add::execute` already runs `with_command_global`
        // (the single exclusivity seam → `--global --project` = exit 64)
        // and `ensure_global_project_initialized` (F7). `Context` is
        // `Clone`; each `Add` invocation gets its own clone since
        // `execute` consumes it.
        for ident in &oci_packages {
            let add = crate::command::add::Add {
                group: None,
                global: true,
                identifier: ident.to_string(),
            };
            let code = add.execute(context.clone()).await?;
            if code != ExitCode::SUCCESS {
                return Ok(code);
            }
        }

        // Surface the implied selection (UX-A1/A3): `--global` performs a
        // PATH-mutating select without the user passing `-s`, asymmetric
        // with plain `install`. Behaviour is ADR-ratified
        // (adr_global_toolchain_tier.md §Decision 3) — make it observable
        // via the existing cargo-style status channel (stderr / log,
        // never stdout data) so a global-only tool landing on PATH is not
        // a surprise.
        context.ui().status(
            "Selecting",
            "global toolchain (--global implies --select for the global PATH)",
        );

        // Select step: drive the same wire-selection pipeline as
        // `ocx select` / `install --select` so the resolved package
        // becomes the `current` symlink on the global PATH. Find (no
        // re-download — `Add` already installed) then `wire_selection`.
        let platforms = platforms_or_default(&[]);
        let infos = context.manager().find_all(oci_packages.clone(), platforms).await?;

        let mut packages = std::collections::HashMap::with_capacity(infos.len());
        for ((raw, ident), info) in self.packages.iter().zip(oci_packages.iter()).zip(infos.iter()) {
            let outcome = context.manager().wire_selection(ident, info, false, true).await?;
            packages.insert(
                raw.raw().to_string(),
                api::data::install::InstallEntry {
                    identifier: info.identifier().clone().into(),
                    metadata: info.metadata().clone(),
                    path: outcome.current,
                },
            );
        }

        context.api().report(&api::data::install::Installs::new(packages))?;
        Ok(ExitCode::SUCCESS)
    }
}
