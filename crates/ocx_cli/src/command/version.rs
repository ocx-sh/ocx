// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_console::ColorModeConfig;

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
    /// Context-free path run before `Context::try_init`.
    ///
    /// `query_installed_version` spawns this with `env_clear()`: reading `HOME`, `PATH` or `OCX_*` on the
    /// non-verbose path makes `ocx self update` silently fall back to bootstrap mode.
    pub async fn execute(&self, options: &ContextOptions, color_config: ColorModeConfig) -> anyhow::Result<ExitCode> {
        let data = VersionData::enriched(crate::app::version(), env!("CARGO_PKG_VERSION"));
        let api = options.build_api(color_config);
        if self.verbose {
            // The static bypass skipped `Context::try_init`'s host-libc caching; the verbose `host:` row needs it.
            ocx_oci::HostCapabilities::detect_and_cache(
                ocx_config::home::default_ocx_root()
                    .map(|root| ocx_store::file_structure::StateStore::new(root.join("state")).host_capabilities_file())
                    .as_deref(),
            )
            .await;
            api.report(&VerboseVersionData(data))?;
        } else {
            api.report(&data)?;
        }
        Ok(ExitCode::SUCCESS)
    }
}
