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
        let mut options = self.options.clone();
        options.dry_run = true;
        let sync = super::sync::Sync {
            spec: self.spec.clone(),
            options,
        };
        sync.execute().await
    }
}
