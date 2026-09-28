// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx [--global] patch freeze` — pin companion digests to a snapshot.

use std::process::ExitCode;

use clap::Args;
use ocx_package_manager::patch::{PATCH_SNAPSHOT_FILE, PatchSnapshot};

/// Arguments for `ocx [--global] patch freeze`.
#[derive(Args)]
pub struct PatchFreezeArgs {
    // No arguments: freeze targets the in-scope project, or `$OCX_HOME` under `--global`.
}

impl PatchFreezeArgs {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let snapshot_dir = resolve_snapshot_dir(&context).await?;
        let snapshot_path = snapshot_dir.join(PATCH_SNAPSHOT_FILE);

        let host = ocx_oci::Platform::current().unwrap_or_else(ocx_oci::Platform::any);
        // `Recorded`, or an already-active snapshot re-freezes its own output.
        let roots = context
            .manager()
            .resolve_site_patch_roots(&host, ocx_package_manager::PatchRootScope::Recorded)
            .await
            .map_err(anyhow::Error::new)?;

        let companion_count = roots.companions.len();
        let descriptor_count = roots.descriptors.len();

        let snapshot = PatchSnapshot::from_roots(&roots);
        snapshot.write(&snapshot_path).await.map_err(anyhow::Error::new)?;

        context
            .api()
            .report(&crate::api::data::patch_freeze::PatchFreezeReport::new(
                companion_count,
                descriptor_count,
                snapshot_path,
            ))?;

        Ok(ExitCode::SUCCESS)
    }
}

/// The directory for `patches.snapshot.json`: `$OCX_HOME` under `--global`, else the project directory.
async fn resolve_snapshot_dir(context: &crate::app::Context) -> anyhow::Result<std::path::PathBuf> {
    if context.global() {
        Ok(context.file_structure().root().to_path_buf())
    } else {
        use crate::app::project_context::load_project_with_lock;
        let ctx = load_project_with_lock(context).await?;
        let dir = match ctx.lock_path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
            // A bare `ocx.lock` falls back to ".", never the lock path, or the snapshot lands under a file.
            _ => std::path::PathBuf::from("."),
        };
        Ok(dir)
    }
}
