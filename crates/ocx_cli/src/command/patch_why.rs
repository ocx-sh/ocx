// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx patch why <base>` — trace which companion contributes each patched
//! env var to a base, and by which descriptor rule.

use std::process::ExitCode;
use std::sync::Arc;

use clap::Args;
use ocx_package::install_info::InstallInfo;
use ocx_package_manager::EnvScope;

use crate::{api, conventions, options};

/// Arguments for `ocx patch why`.
#[derive(Args)]
pub struct PatchWhyArgs {
    #[clap(flatten)]
    platform: options::PlatformOption,

    /// Use the base's own surface, the one its launchers see, instead of its consumers'
    ///
    /// Without `--self`, the result is the interface surface a consumer of the base composes:
    /// `public` and `interface` variables. With `--self`, it is the surface the base's own
    /// launchers compose: `public` and `private` variables.
    #[clap(long = "self", default_value_t = false)]
    self_view: bool,

    /// Base identifier to trace patch provenance for.
    #[clap(value_name = "BASE-ID", required = true)]
    base: options::Identifier,
}

impl PatchWhyArgs {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let base_id = self.base.with_domain(context.default_registry())?;
        let platform = conventions::platform_or_default(self.platform.platform.clone());
        let manager = context.manager();

        let info = manager
            .find_or_install_all(std::slice::from_ref(&base_id), platform.clone(), context.concurrency())
            .await?;
        let info: Vec<Arc<InstallInfo>> = info.into_iter().map(|found| Arc::new(found.info)).collect();

        // The base's `platform`, not the host, or a companion with no host leaf goes untraced under `-p`.
        let (entries, patch_start, provenance, claims) = manager
            .resolve_env_with_attribution(&info, self.self_view, EnvScope::package_tier(), &platform)
            .await?;

        // `provenance` aligns with `entries[patch_start..]`; an empty overlay is not an error.
        let why_entries: Vec<api::data::patch_why::PatchWhyEntry> = entries[patch_start..]
            .iter()
            .zip(provenance.iter())
            .map(|(entry, prov)| {
                api::data::patch_why::PatchWhyEntry::new(
                    entry.key.clone(),
                    prov.rule_match.clone(),
                    prov.companion.to_string(),
                )
            })
            .collect();

        let companions = claims
            .companions
            .iter()
            .map(|companion| companion.pinned.to_string())
            .collect();
        context.api().report(&api::data::patch_why::PatchWhyReport::new(
            base_id.to_string(),
            companions,
            why_entries,
        ))?;

        Ok(ExitCode::SUCCESS)
    }
}
