// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx patch why <base>` — trace which companion contributes each patched
//! env var to a base, and by which descriptor rule.
//!
//! An OCI-tier diagnostic: it resolves the base identifier directly (no
//! `ocx.toml` in scope) and reports the same companion overlay
//! `resolve_env_with_patch_boundary` would apply to it, one row per
//! contributed env var.

use std::process::ExitCode;
use std::sync::Arc;

use clap::Args;
use ocx_lib::package::install_info::InstallInfo;
use ocx_lib::package_manager::PatchScope;

use crate::{api, conventions, options};

/// Arguments for `ocx patch why`.
#[derive(Args)]
pub struct PatchWhyArgs {
    #[clap(flatten)]
    platform: options::PlatformOption,

    /// Base identifier to trace patch provenance for.
    #[clap(value_name = "BASE-ID", required = true)]
    base: options::Identifier,
}

impl PatchWhyArgs {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // ── Step 1: Resolve the base identifier and find-or-install it. ──
        //
        // OCI-tier diagnostic: no project is in scope, so the resolution uses
        // `PatchScope::NoProjectContext` — it shows what the configured
        // `[patches]` tier overlays for the base, not a project opt-out
        // decision (there is no project to opt out from).
        let base_id = self.base.with_domain(context.default_registry())?;
        let platform = conventions::platform_or_default(self.platform.platform.clone());
        let manager = context.manager();

        let info = manager
            .find_or_install_all(vec![base_id.clone()], platform, context.concurrency())
            .await?;
        let info: Vec<Arc<InstallInfo>> = info.into_iter().map(Arc::new).collect();

        // ── Step 2: Reuse the existing provenance resolution — no new
        // resolution path. ──
        let (entries, patch_start, provenance) = manager
            .resolve_env_with_patch_boundary(&info, false, PatchScope::NoProjectContext)
            .await?;

        // ── Step 3: Zip the overlay slice with its aligned provenance. ──
        //
        // Empty when no `[patches]` tier is configured (provenance is empty by
        // construction — see `resolve_env_with_patch_boundary`) or no
        // companion contributes a var to this base. Both cases collapse to
        // the same "no patches apply" report; this is not an error.
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

        // ── Step 4: Report. Format is a context-level concern (root
        // `--format`); this command does not override it. ──
        context.api().report(&api::data::patch_why::PatchWhyReport::new(
            base_id.to_string(),
            why_entries,
        ))?;

        Ok(ExitCode::SUCCESS)
    }
}
