// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::fmt;
use std::path::PathBuf;

use ocx_console::Cell;
use ocx_oci::PinnedPackageRef;
use serde::Serialize;

use crate::api::Printable;

/// Whether a locked tool is already in the object store or would be
/// fetched on a real `ocx pull`.
#[derive(Serialize, schemars::JsonSchema, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum PullStatus {
    /// Already in the object store.
    Cached,
    /// A real `ocx pull` would download it.
    WouldFetch,
}

impl fmt::Display for PullStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PullStatus::Cached => write!(f, "cached"),
            PullStatus::WouldFetch => write!(f, "would_fetch"),
        }
    }
}

// `package` stays typed: JSON keeps the full pin while the plain table shortens it.
/// A single dry-run preview row.
///
/// `path` is the package root directory (parent of `content/` and
/// `entrypoints/`) for `cached` rows and absent for `would_fetch` rows, where
/// nothing has been materialised yet.
#[derive(Serialize, schemars::JsonSchema)]
pub struct DryRunEntry {
    /// The locked tool's pinned identifier.
    pub package: PinnedPackageRef,
    /// Whether the tool is cached or would be fetched.
    pub status: PullStatus,
    /// The package root; absent when nothing is materialised yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
}

impl DryRunEntry {
    pub fn new(package: PinnedPackageRef, status: PullStatus, path: Option<PathBuf>) -> Self {
        Self { package, status, path }
    }
}

/// Preview of what `ocx pull` would do without writing to the store.
#[derive(Serialize, schemars::JsonSchema)]
pub struct PullDryRun {
    /// One entry per locked tool, in lock-file order.
    pub items: Vec<DryRunEntry>,
}

impl PullDryRun {
    pub fn new(items: Vec<DryRunEntry>) -> Self {
        Self { items }
    }
}

impl Printable for PullDryRun {
    const SCHEMA_VERSION: u32 = 1;
    const ROOT: &'static str = "PullDryRun";

    fn print_plain(&self, printer: &ocx_console::DataInterface) {
        let mut rows: [Vec<String>; 2] = [Vec::new(), Vec::new()];
        for entry in &self.items {
            // Shortened, not dropped: a locked leaf has no tag, so the digest is its only version.
            rows[0].push(format!(
                "{}@{}",
                entry.package.without_digest(),
                entry.package.digest().to_short_string()
            ));
            rows[1].push(entry.status.to_string());
        }
        printer.print_table(
            &["Package".into(), "Status".into()],
            &rows.map(|c| c.into_iter().map(Cell::from).collect::<Vec<_>>()),
        );
    }
}
