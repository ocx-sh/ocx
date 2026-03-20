// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::fmt;
use std::path::PathBuf;

use serde::Serialize;

use crate::api::Printable;

/// Whether the resource was actually removed, purged, or was already absent.
#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum RemovedStatus {
    Removed,
    Purged,
    Absent,
}

impl fmt::Display for RemovedStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RemovedStatus::Removed => write!(f, "removed"),
            RemovedStatus::Purged => write!(f, "purged"),
            RemovedStatus::Absent => write!(f, "absent"),
        }
    }
}

/// A single uninstall or deselect result entry.
///
/// The `path` field holds the symlink that was removed (for `Removed`),
/// the object directory that was purged (for `Purged`), or is `None`
/// when the resource was already absent.
#[derive(Serialize)]
pub struct RemovedEntry {
    pub package: String,
    pub status: RemovedStatus,
    pub path: Option<PathBuf>,
}

impl RemovedEntry {
    pub fn new(package: String, status: RemovedStatus, path: Option<PathBuf>) -> Self {
        Self { package, status, path }
    }
}

/// Results of an uninstall or deselect operation.
///
/// Plain format: three-column table (Package | Status | Path).
///
/// JSON format: array of `{ package, status, path }` objects.
pub struct Removed {
    pub entries: Vec<RemovedEntry>,
}

impl Removed {
    pub fn new(entries: Vec<RemovedEntry>) -> Self {
        Self { entries }
    }
}

impl Serialize for Removed {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.entries.serialize(serializer)
    }
}

impl Printable for Removed {
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
