// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::fmt;
use std::path::PathBuf;

use ocx_lib::cli::Cell;
use ocx_lib::oci::PinnedIdentifier;
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
/// `package` is held typed rather than pre-formatted so plain and JSON can
/// render it differently: `PinnedIdentifier`'s `Serialize` is its `Display`,
/// so JSON keeps the pinned `…@sha256:<64hex>` form, while the plain table
/// drops the digest.
///
/// `path` is `Some` for cached entries (the package root directory,
/// parent of `content/` and `entrypoints/`) and `None` for `WouldFetch`
/// rows where nothing has been materialised yet. Mirrors the contract
/// documented on [`crate::api::data::paths::PathEntry`]: consumers
/// traverse into `<path>/content/` for installed files or
/// `<path>/entrypoints/` for generated launchers.
#[derive(Serialize)]
pub struct DryRunEntry {
    pub package: PinnedIdentifier,
    pub status: PullStatus,
    pub path: Option<PathBuf>,
}

impl DryRunEntry {
    pub fn new(package: PinnedIdentifier, status: PullStatus, path: Option<PathBuf>) -> Self {
        Self { package, status, path }
    }
}

/// Preview of what `ocx pull` would do without writing to the store.
///
/// Plain format: two-column table (Package | Status), the package pinned to a
/// 12-hex short digest. `path` has no column: it is populated only for `cached`
/// rows and is a dash for exactly the `would-fetch` rows this command exists to
/// surface.
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
    fn print_plain(&self, printer: &ocx_lib::cli::DataInterface) {
        let mut rows: [Vec<String>; 2] = [Vec::new(), Vec::new()];
        for entry in &self.entries {
            // A locked leaf carries no tag (`LockedTool::repository` is bare
            // registry/repo coordinates), so dropping the digest outright would
            // leave the row with no version at all. Shorten it instead: 12 hex
            // still discriminates two leaves of the same repo, at a quarter the
            // width. JSON keeps the full pin.
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
