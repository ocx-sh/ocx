// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::fmt;
use std::path::PathBuf;

use ocx_console::Cell;
use serde::Serialize;

use crate::api::Printable;

/// Whether the resource was actually removed, purged, or was already absent.
#[derive(Serialize, schemars::JsonSchema, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum RemovedStatus {
    /// The symlink was removed.
    Removed,
    /// The package's object directory was deleted.
    Purged,
    /// Nothing was there to remove.
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
/// The `path` field holds the symlink that was removed (for `removed`) or the
/// object directory that was purged (for `purged`), and is absent when the
/// resource was already absent.
#[derive(Serialize, schemars::JsonSchema)]
pub struct RemovedEntry {
    /// The package as requested.
    pub package: String,
    /// What happened to it.
    pub status: RemovedStatus,
    /// What was removed; absent when nothing was.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
}

impl RemovedEntry {
    pub fn new(package: String, status: RemovedStatus, path: Option<PathBuf>) -> Self {
        Self { package, status, path }
    }
}

/// Results of an uninstall or deselect; `path` gets no plain column since it names something now gone.
#[derive(Serialize, schemars::JsonSchema)]
pub struct Removed {
    /// One entry per package, in request order.
    pub items: Vec<RemovedEntry>,
}

impl Removed {
    pub fn new(items: Vec<RemovedEntry>) -> Self {
        Self { items }
    }
}

impl Printable for Removed {
    const SCHEMA_VERSION: u32 = 1;
    const ROOT: &'static str = "Removed";

    fn print_plain(&self, printer: &ocx_console::DataInterface) {
        let mut rows: [Vec<String>; 2] = [Vec::new(), Vec::new()];
        for entry in &self.items {
            rows[0].push(entry.package.clone());
            rows[1].push(entry.status.to_string());
        }
        printer.print_table(
            &["Package".into(), "Status".into()],
            &rows.map(|c| c.into_iter().map(Cell::from).collect::<Vec<_>>()),
        );
    }
}
