// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::managed_config::preview_managed_config;

use crate::api::data::config_test::ConfigTestData;

/// Arguments for `ocx config test`.
#[derive(Parser)]
pub struct ConfigTestArgs {
    /// The config file to check.
    config: std::path::PathBuf,
}

impl ConfigTestArgs {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let bytes = tokio::fs::read(&self.config).await.map_err(|source| {
            ocx_lib::managed_config::ManagedConfigPublishError::ReadFailed {
                path: self.config.clone(),
                source,
            }
        })?;

        // The candidate takes the managed tier's place in this machine's own
        // fold: base tiers, then the candidate, then the explicit overlay.
        // Same order adoption produces, so the report is the configuration the
        // machine would really resolve — overlay included.
        let preview = preview_managed_config(&bytes, context.config_base().clone(), context.config_overlay())?;

        // The gates every ocx invocation applies to its own config, run against
        // the effective merge — a payload that parses can still be one no
        // machine can start under (a plain-HTTP mirror, an empty patch
        // registry). Catching that here is the point of previewing.
        // The plain-HTTP set comes from the CANDIDATE's own `[registries]`
        // entries unioned with this machine's env — a payload that declares
        // `insecure = true` for its own mirror host must pass its own gate.
        let insecure_hosts = ocx_lib::insecure_hosts(&preview.effective, &ocx_lib::env::insecure_registries());
        ocx_lib::resolve_mirror_map(&preview.effective, ocx_lib::env::mirrors()?, &insecure_hosts)?;
        // Reported, not just consumed: a `[registries.<name>]` key that is not
        // an exact `host[:port]` grants nothing and fails much later as an
        // opaque TLS error, so the one command whose job is previewing a
        // config has to answer "did my entry take effect?".
        // Same config-tier-then-env precedence `Context::try_init` uses, so the
        // report matches what the machine actually resolves.
        let patches = match ocx_lib::resolve_patch_config(&preview.effective)? {
            Some(resolved) => Some(resolved),
            None => ocx_lib::patches_from_env()?,
        };
        // The machine's own tier, never the candidate's — a payload declaring
        // `[managed]` was already refused above.
        let managed = ocx_lib::resolve_managed_target(context.config(), context.managed_config_env_override())?;

        context.api().report(&ConfigTestData::new(
            &self.config,
            preview,
            patches,
            managed.as_ref(),
            insecure_hosts,
        ))?;

        Ok(ExitCode::SUCCESS)
    }
}
