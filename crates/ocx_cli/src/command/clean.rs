// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;

use crate::api;

/// Remove unreferenced objects and stale temp directories.
///
/// An object is unreferenced when its `refs/` directory is empty or absent, as
/// after `ocx uninstall` without `--purge` or a manual symlink removal. A temp
/// directory is stale when no running process holds its `install.lock`: a
/// previous download was interrupted.
///
/// Packages any registered project's `ocx.lock` holds are retained, even from
/// another project directory; `--force` ignores the project registry.
/// `--dry-run` previews without changing anything.
#[derive(Parser)]
pub struct Clean {
    /// Show what would be removed without actually removing anything.
    #[clap(long = "dry-run")]
    dry_run: bool,

    /// Ignore the project registry and collect all unreferenced packages,
    /// including those held by other projects' `ocx.lock` files.
    ///
    /// Without `--force`, `ocx clean` consults the per-user project registry
    /// (`$OCX_HOME/projects/`) and retains any package pinned by a
    /// registered lock. With `--force`, that guard is bypassed entirely.
    /// Live install symlinks are always honoured regardless of this flag.
    #[clap(long = "force")]
    force: bool,
}

impl Clean {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let result = context.manager().clean(self.dry_run, self.force).await?;

        context.api().report(&api::data::clean::Clean::new(
            result.objects,
            result.temp,
            result.consent,
            self.dry_run,
        ))?;

        Ok(ExitCode::SUCCESS)
    }
}
