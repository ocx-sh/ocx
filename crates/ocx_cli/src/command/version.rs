// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::cli::ColorModeConfig;

use crate::{api::data::version::VersionData, app::ContextOptions};

#[derive(Parser)]
pub struct Version;

impl Version {
    /// Context-free execution path — called from `app.rs` before
    /// `Context::try_init`.
    ///
    /// Delegates printer + format-default + quiet wiring to
    /// [`ContextOptions::build_api`] — the same init seam
    /// `Context::try_init` uses — so `--format`, `--color`, and `--quiet`
    /// behave identically on the static-command bypass path.
    ///
    /// # Hermetic-subprocess invariant
    ///
    /// `ocx_lib::package_manager::tasks::update_check::query_installed_version`
    /// spawns this command with `env_clear()` to query a previously installed
    /// binary's version during `ocx self update`. This method MUST therefore
    /// NOT depend on `HOME`, `PATH`, or any `OCX_*` env var to produce the
    /// JSON `version` payload — the subprocess child receives only the
    /// `resolve_env`-composed entries. A regression that adds env reads
    /// silently routes the self-update path to bootstrap mode (the JSON
    /// parse fails or the subprocess errors), which is hard to debug. If
    /// new behaviour requires config, route it through the parent process
    /// and let the subprocess stay pure-version. See
    /// `subsystem-package-manager.md` "OCX Configuration Forwarding".
    pub async fn execute(&self, options: &ContextOptions, color_config: ColorModeConfig) -> anyhow::Result<ExitCode> {
        options
            .build_api(color_config)
            .report(&VersionData::new(crate::app::version()))?;
        Ok(ExitCode::SUCCESS)
    }
}
