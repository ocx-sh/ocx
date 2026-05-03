// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::fmt;
use std::path::PathBuf;

use serde::Serialize;

use crate::api::Printable;

/// Whether a locked tool is already in the object store or would be
/// fetched on a real `ocx pull`.
#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
pub enum PullStatus {
    Cached,
    WouldFetch,
}

impl fmt::Display for PullStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PullStatus::Cached => write!(f, "cached"),
            PullStatus::WouldFetch => write!(f, "would-fetch"),
        }
    }
}

/// A single dry-run preview row.
///
/// `path` is `Some` for cached entries (pointing at the existing
/// `content/` directory in the store) and `None` for `WouldFetch` rows
/// where no path exists yet.
#[derive(Serialize)]
pub struct DryRunEntry {
    pub package: String,
    pub status: PullStatus,
    pub path: Option<PathBuf>,
}

impl DryRunEntry {
    pub fn new(package: String, status: PullStatus, path: Option<PathBuf>) -> Self {
        Self { package, status, path }
    }
}

/// Preview of what `ocx pull` would do without writing to the store.
///
/// Plain format: three-column table (Package | Status | Path).
///
/// JSON format: array of `{ package, status, path }` objects, preserving
/// lock-file order.
pub struct PullDryRun {
    pub entries: Vec<DryRunEntry>,
}

impl PullDryRun {
    pub fn new(entries: Vec<DryRunEntry>) -> Self {
        Self { entries }
    }
}

impl Serialize for PullDryRun {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.entries.serialize(serializer)
    }
}

impl Printable for PullDryRun {
    fn print_plain(&self, printer: &ocx_lib::cli::Printer) {
        let mut rows: [Vec<String>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        for entry in &self.entries {
            rows[0].push(entry.package.clone());
            rows[1].push(entry.status.to_string());
            rows[2].push(
                entry
                    .path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or("-".into()),
            );
        }
        printer.print_table(&["Package", "Status", "Path"], &rows);
    }
}
