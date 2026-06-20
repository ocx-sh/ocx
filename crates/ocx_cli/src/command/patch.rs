// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx [--global] patch` — patch overlay management commands.
//!
//! Sub-commands:
//! - **freeze**: write a `patches.snapshot.json` to pin companion digests for
//!   reproducible builds.
//! - **sync**: refresh patch descriptors and companions from the registry for
//!   installed packages.
//!
//! # Design
//!
//! These commands are toolchain-tier: they operate on an `ocx.toml` /
//! `ocx.lock` project (or `$OCX_HOME` under `--global`). The snapshot file
//! is written as a sibling of `ocx.lock`.

use std::process::ExitCode;

use clap::{Args, Subcommand};
use ocx_lib::{
    oci,
    patch::{PATCH_SNAPSHOT_FILE, PatchSnapshot},
};

use crate::{conventions::platforms_or_default, options};

/// Manage patch overlays for a project.
///
/// Commands here read or write `patches.snapshot.json`, which pins companion
/// package digests for reproducible builds. Use `patch freeze` to write the
/// snapshot and `patch sync` to refresh patch descriptors and companions from
/// the registry.
#[derive(Subcommand)]
pub enum PatchGroup {
    /// Freeze companion package digests to a snapshot for reproducible builds.
    ///
    /// Resolves every companion and descriptor digest in the active patch
    /// overlay and writes a `patches.snapshot.json` file beside `ocx.lock`.
    /// Once frozen, setting `OCX_PATCH_SNAPSHOT` to that path makes all env
    /// composition prefer the pinned digests over live tag lookups.
    ///
    /// Works offline: only the local object store is consulted.
    Freeze(PatchFreezeArgs),

    /// Refresh patch descriptors and companion packages from the registry.
    ///
    /// Re-fetches every patch descriptor for all installed packages and the
    /// global root. Installs any newly-referenced companion packages. Requires
    /// network access.
    ///
    /// This command also picks up patches for packages installed before patch
    /// configuration was added. All states are re-checked regardless of what
    /// was previously recorded.
    Sync(PatchSyncArgs),
}

/// Arguments for `ocx [--global] patch freeze`.
#[derive(Args)]
pub struct PatchFreezeArgs {
    // No positional arguments — freeze always targets the in-scope project
    // (or `$OCX_HOME` under `--global`).
}

/// Arguments for `ocx [--global] patch sync`.
#[derive(Args)]
pub struct PatchSyncArgs {
    #[clap(flatten)]
    platforms: options::Platforms,
}

impl PatchGroup {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        match self {
            PatchGroup::Freeze(args) => args.execute(context).await,
            PatchGroup::Sync(args) => args.execute(context).await,
        }
    }
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

impl PatchSyncArgs {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // ── Step 1: Resolve the platform set to sync. ──
        let platforms = platforms_or_default(self.platforms.as_slice());

        // ── Step 2: Run the sync. ──
        let report = context
            .manager()
            .sync_patches(&platforms)
            .await
            .map_err(anyhow::Error::new)?;

        // ── Step 3: Report. ──
        context
            .api()
            .report(&crate::api::data::patch_sync::PatchSyncReport::new(report))?;

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
            // Both branches failed: the lock path has no directory component AND
            // the CWD is unreadable. Fall back to the CWD-relative ".", never the
            // lock file path itself (which would yield a path *under* a file).
            _ => std::path::PathBuf::from("."),
        };
        Ok(dir)
    }
}
