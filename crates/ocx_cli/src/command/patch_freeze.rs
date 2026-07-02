// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx [--global] patch freeze` — pin companion digests to a snapshot.
//!
//! Resolves every companion and descriptor digest in the active patch overlay
//! and writes a `patches.snapshot.json` beside `ocx.lock` (or under `$OCX_HOME`
//! with `--global`). Works offline: only the local object store is consulted.

use std::process::ExitCode;

use clap::Args;
use ocx_lib::oci;
use ocx_lib::patch::{PATCH_SNAPSHOT_FILE, PatchSnapshot};

/// Arguments for `ocx [--global] patch freeze`.
#[derive(Args)]
pub struct PatchFreezeArgs {
    // No positional arguments — freeze always targets the in-scope project
    // (or `$OCX_HOME` under `--global`).
}

impl PatchFreezeArgs {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // ── Step 1: Resolve the directory where the snapshot will be written. ──
        //
        // The snapshot lives beside `ocx.lock` (or `$OCX_HOME` under --global).
        // For the project tier, we locate the project via the standard resolution
        // chain (--global / --project / OCX_PROJECT / CWD walk). For the global
        // tier, the directory is always `$OCX_HOME`.
        let snapshot_dir = resolve_snapshot_dir(&context).await?;
        let snapshot_path = snapshot_dir.join(PATCH_SNAPSHOT_FILE);

        // ── Step 2: Resolve site-patch roots (local only, no network). ──
        //
        // Uses the manager's current `patches` config (from OCX_PATCHES /
        // `[patches]` config tier). When no patch tier is configured, roots is
        // empty and the snapshot records zero companions / descriptors.
        let host = oci::Platform::current().unwrap_or_else(oci::Platform::any);
        let roots = context
            .manager()
            .resolve_site_patch_roots(&[host])
            .await
            .map_err(anyhow::Error::new)?;

        let companion_count = roots.companions.len();
        let descriptor_count = roots.descriptors.len();

        // ── Step 3: Build and write the snapshot. ──
        let snapshot = PatchSnapshot::from_roots(&roots);
        snapshot.write(&snapshot_path).await.map_err(anyhow::Error::new)?;

        // ── Step 4: Report. ──
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

/// Resolve the directory that will contain `patches.snapshot.json`.
///
/// Under `--global` (`context.global()` is true): `$OCX_HOME`.
/// Under project tier: the parent directory of the resolved `ocx.lock` (i.e.,
/// the same directory as `ocx.toml`). Returns an error if no project can be
/// found.
async fn resolve_snapshot_dir(context: &crate::app::Context) -> anyhow::Result<std::path::PathBuf> {
    if context.global() {
        // Global tier: snapshot beside $OCX_HOME/ocx.lock.
        Ok(context.file_structure().root().to_path_buf())
    } else {
        // Project tier: use the same project-resolution chain as pull/env/run.
        use crate::app::project_context::load_project_with_lock;
        let ctx = load_project_with_lock(context).await?;
        // The lock path is always <project_dir>/ocx.lock, so its parent is the
        // project directory. The snapshot goes beside it. If `parent()` returns
        // `None` (a bare filename with no directory component), fall back to the
        // current working directory rather than treating the lock file itself as
        // a directory, which would produce a path under a file and confuse the
        // error message on write failure.
        let dir = match ctx.lock_path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
            // `parent()` is `None` or empty when `lock_path` is a bare filename
            // with no directory component (e.g. "ocx.lock"). Fall back to the
            // CWD-relative ".", never the lock file path itself (which would
            // yield a path *under* a file).
            _ => std::path::PathBuf::from("."),
        };
        Ok(dir)
    }
}
