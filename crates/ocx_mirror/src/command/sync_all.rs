// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

use ocx_lib::log;

use super::options::SyncOptions;
use crate::error::MirrorError;

#[derive(clap::Args)]
pub struct SyncAll {
    /// Directory containing package subdirectories, each with a mirror.yml spec
    pub dir: PathBuf,

    #[clap(flatten)]
    pub options: SyncOptions,
}

impl SyncAll {
    pub async fn execute(&self) -> Result<(), MirrorError> {
        let pattern = self.dir.join("*/mirror.yml");
        let pattern_str = pattern.to_string_lossy();

        let mut specs: Vec<PathBuf> = glob::glob(&pattern_str)
            .map_err(|e| MirrorError::SpecInvalid(vec![format!("invalid glob pattern: {e}")]))?
            .filter_map(|entry| entry.ok())
            .collect();
        specs.sort();

        if specs.is_empty() {
            log::info!("No mirror specs found in {}", self.dir.display());
            return Ok(());
        }

        log::info!("Found {} mirror specs", specs.len());

        let mut failures = Vec::new();

        for spec_path in &specs {
            log::info!("Processing {}", spec_path.display());

            let sync = super::sync::Sync {
                spec: spec_path.clone(),
                options: SyncOptions {
                    work_dir: self.options.work_dir.clone(),
                    version: self.options.version.clone(),
                    dry_run: self.options.dry_run,
                    fail_fast: self.options.fail_fast,
                    format: self.options.format,
                },
            };

            if let Err(e) = sync.execute().await {
                log::error!("Failed {}: {e}", spec_path.display());
                failures.push(format!("{}: {e}", spec_path.display()));
                if self.options.fail_fast {
                    break;
                }
            }
        }

        if !failures.is_empty() {
            return Err(MirrorError::ExecutionFailed(failures));
        }

        Ok(())
    }
}
