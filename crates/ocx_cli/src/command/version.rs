// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::cli::ColorModeConfig;

use crate::api::data::version::{VerboseVersionData, VersionData};
use crate::app::ContextOptions;

#[derive(Parser)]
pub struct Version {
    /// Emit enriched build provenance - commit, dirty flag, build time,
    /// target, rustc, CI run URL. JSON output always includes the
    /// populated subset; this flag only affects plain text.
    #[arg(short, long)]
    verbose: bool,
}

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
    ///
    /// The build provenance fields populated by
    /// [`VersionData::enriched`] are read from compile-time `option_env!`
    /// constants (see `app::build_info`), not runtime `std::env` — they
    /// stay correct under `env_clear()`.
    pub async fn execute(&self, options: &ContextOptions, color_config: ColorModeConfig) -> anyhow::Result<ExitCode> {
        let data = VersionData::enriched(crate::app::version(), env!("CARGO_PKG_VERSION"));
        let api = options.build_api(color_config);
        if self.verbose {
            // `ocx version` runs on the static bypass, so `Context::try_init`
            // (which normally caches host capabilities) has not run. Populate
            // the host-libc cache here so the verbose `host:` row can report
            // the detected family. Only the verbose plain path needs it; the
            // bare/JSON path stays pure for the self-update subprocess parser.
            ocx_lib::oci::HostCapabilities::detect_and_cache().await;
            api.report(&VerboseVersionData(data))?;
        } else {
            api.report(&data)?;
        }
        Ok(ExitCode::SUCCESS)
    }
}
