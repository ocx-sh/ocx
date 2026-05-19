// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

use ocx_lib::cli::progress::ProgressManager;

use super::options::SyncOptions;
use crate::error::MirrorError;

#[derive(clap::Args)]
pub struct Check {
    /// Path to the mirror spec YAML file
    pub spec: PathBuf,

    #[clap(flatten)]
    pub options: SyncOptions,
}

impl Check {
    pub async fn execute(&self, printer: &ocx_lib::cli::DataInterface) -> Result<(), MirrorError> {
        let mut options = self.options.clone();
        options.dry_run = true;
        let sync = super::sync::Sync {
            spec: self.spec.clone(),
            options,
        };
        // Dry-run never renders progress; pass a disabled manager.
        sync.execute(printer, &ProgressManager::disabled()).await
    }
}
