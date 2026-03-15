// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

use ocx_lib::cli::Printer;

use crate::pipeline::mirror_result::MirrorResult;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum OutputFormat {
    Plain,
    Json,
}

#[derive(Clone, clap::Args)]
pub struct SyncOptions {
    /// Working directory for downloads, bundles, and intermediate artifacts.
    /// Artifacts persist between runs so failed tasks can resume without re-downloading.
    /// Cleaned up per-task after successful push.
    #[arg(long, default_value = "./.ocx-mirror")]
    pub work_dir: PathBuf,

    /// Only check what would be mirrored (dry-run)
    #[arg(long)]
    pub dry_run: bool,

    /// Only mirror specific versions (e.g., --version 3.28.0,3.29.0 or --version 3.28.0 --version 3.29.0).
    /// Matches against the extracted version string from the source.
    #[arg(long, value_delimiter = ',')]
    pub version: Vec<String>,

    /// Only mirror the latest (highest) version.
    /// Applied after all other filters (exact versions, min/max bounds, etc.).
    #[arg(long)]
    pub latest: bool,

    /// Stop on first failure instead of continuing
    #[arg(long)]
    pub fail_fast: bool,

    /// Output format
    #[arg(long, value_enum, default_value = "plain")]
    pub format: OutputFormat,
}

/// Print structured results and return whether any failures occurred.
pub fn report_results(results: &[MirrorResult], format: OutputFormat, printer: &Printer) -> bool {
    let pushed = results
        .iter()
        .filter(|r| matches!(r, MirrorResult::Pushed { .. }))
        .count();
    let skipped = results
        .iter()
        .filter(|r| matches!(r, MirrorResult::Skipped { .. }))
        .count();
    let failed = results
        .iter()
        .filter(|r| matches!(r, MirrorResult::Failed { .. }))
        .count();
    let total = results.len();

    match format {
        OutputFormat::Json => {
            if let Ok(json) = serde_json::to_string_pretty(results) {
                println!("{json}");
            }
        }
        OutputFormat::Plain => {
            if !results.is_empty() {
                let mut versions = Vec::new();
                let mut platforms = Vec::new();
                let mut statuses = Vec::new();
                let mut details = Vec::new();

                for result in results {
                    match result {
                        MirrorResult::Pushed {
                            version,
                            platform,
                            digest,
                        } => {
                            versions.push(version.clone());
                            platforms.push(platform.to_string());
                            statuses.push("pushed".to_string());
                            details.push(digest.clone());
                        }
                        MirrorResult::Skipped { version } => {
                            versions.push(version.clone());
                            platforms.push(String::new());
                            statuses.push("skipped".to_string());
                            details.push(String::new());
                        }
                        MirrorResult::Failed {
                            version,
                            platform,
                            error,
                        } => {
                            versions.push(version.clone());
                            platforms.push(platform.to_string());
                            statuses.push("failed".to_string());
                            details.push(error.clone());
                        }
                    }
                }

                printer.print_table(
                    &["Version", "Platform", "Status", "Detail"],
                    &[versions, platforms, statuses, details],
                );
                println!("---");
            }
            println!("{total} total, {pushed} pushed, {skipped} skipped, {failed} failed");
        }
    }

    failed > 0
}
