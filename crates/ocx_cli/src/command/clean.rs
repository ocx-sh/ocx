// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;

use crate::api;

/// Remove unreferenced objects and stale temp directories.
///
/// An object is unreferenced when its `refs/` directory is empty or absent —
/// no candidate or current symlink points to it anymore.  This happens after
/// `ocx uninstall` (without `--purge`) or when symlinks are removed manually.
///
/// A temp directory is stale when its `install.lock` is not held by any
/// running process — this indicates a previous download was interrupted.
///
/// Use `--dry-run` to preview what would be removed without making any changes.
///
/// By default, packages held by any registered project's `ocx.lock` are
/// retained even when running `clean` from a different project directory.
/// Use `--force` to ignore the project registry and collect all otherwise-
/// unreferenced packages.
#[derive(Parser)]
pub struct Clean {
    /// Show what would be removed without actually removing anything.
    #[clap(long = "dry-run")]
    dry_run: bool,

    /// Ignore the project registry and collect all unreferenced packages,
    /// including those held by other projects' `ocx.lock` files.
    ///
    /// Without `--force`, `ocx clean` consults the per-user project registry
    /// (`$OCX_HOME/projects.json`) and retains any package pinned by a
    /// registered lock. With `--force`, that guard is bypassed entirely.
    /// Live install symlinks are always honoured regardless of this flag.
    #[clap(long = "force")]
    force: bool,
}

impl Clean {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let result = context.manager().clean(self.dry_run, self.force).await?;

        context
            .api()
            .report(&api::data::clean::Clean::new(result.objects, result.temp, self.dry_run))?;

        Ok(ExitCode::SUCCESS)
    }
}
