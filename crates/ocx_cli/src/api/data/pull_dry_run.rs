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

// `package` stays typed: JSON keeps the full pin while the plain table shortens it.
/// A single dry-run preview row.
///
/// `package` is the pinned `…@sha256:<64hex>` identifier. `path` is the
/// package root directory (parent of `content/` and `entrypoints/`) for
/// `cached` rows, and `null` for `would-fetch` rows where nothing has been
/// materialised yet; traverse into `<path>/content/` for installed files or
/// `<path>/entrypoints/` for generated launchers.
#[derive(Serialize, schemars::JsonSchema)]
pub struct DryRunEntry {
    pub package: PinnedPackageRef,
    pub status: PullStatus,
    pub path: Option<PathBuf>,
}

impl DryRunEntry {
    pub fn new(package: PinnedPackageRef, status: PullStatus, path: Option<PathBuf>) -> Self {
        Self { package, status, path }
    }
}

/// Preview of what `ocx pull` would do without writing to the store, in lock-file order.
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
    fn print_plain(&self, printer: &ocx_console::DataInterface) {
        let mut rows: [Vec<String>; 2] = [Vec::new(), Vec::new()];
        for entry in &self.entries {
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

// Transparent `Serialize`: the schema is the bare entry array.
impl schemars::JsonSchema for PullDryRun {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "PullDryRun".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        <Vec<DryRunEntry>>::json_schema(generator)
    }
}
