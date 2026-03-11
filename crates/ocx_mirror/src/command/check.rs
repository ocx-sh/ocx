// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

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
    pub async fn execute(&self) -> Result<(), MirrorError> {
        let sync = super::sync::Sync {
            spec: self.spec.clone(),
            options: SyncOptions {
                work_dir: self.options.work_dir.clone(),
                version: self.options.version.clone(),
                dry_run: true,
                fail_fast: self.options.fail_fast,
                format: self.options.format,
            },
        };
        sync.execute().await
    }
}
