// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::cli::Cell;
use serde::Serialize;

use crate::api::Printable;

/// Report emitted by `ocx [--global] patch sync`.
///
/// Plain format: a single-row summary table (`Checked | Updated | Companions`)
/// showing how many bases were checked, descriptors updated, and companions
/// installed.
///
/// JSON format: `{ "bases_checked": N, "descriptors_updated": N, "companions_installed": N }`.
#[derive(Serialize)]
pub struct PatchSyncReport {
    /// Number of installed bases (plus global root) that were checked.
    pub bases_checked: usize,
    /// Number of descriptor blobs where the upstream digest advanced.
    pub descriptors_updated: usize,
    /// Number of companion packages installed or re-installed.
    ///
    /// Always 0 in Phase 5C — companion counting is deferred to Phase 6.
    // TODO(Phase 6): wire companion-install counting through sync_patches.
    pub companions_installed: usize,
}

impl PatchSyncReport {
    /// Build a sync report from the library-layer [`ocx_lib::package_manager::PatchSyncReport`].
    pub fn new(inner: ocx_lib::package_manager::PatchSyncReport) -> Self {
        Self {
            bases_checked: inner.bases_checked,
            descriptors_updated: inner.descriptors_updated,
            companions_installed: inner.companions_installed,
        }
    }
}

impl Printable for PatchSyncReport {
    fn print_plain(&self, printer: &ocx_lib::cli::DataInterface) {
        let rows: [Vec<String>; 1] = [vec![
            self.bases_checked.to_string(),
            self.descriptors_updated.to_string(),
            self.companions_installed.to_string(),
        ]];
        printer.print_table(
            &["Checked".into(), "Updated".into(), "Companions".into()],
            &rows.map(|c| c.into_iter().map(Cell::from).collect::<Vec<_>>()),
        );
    }
}
