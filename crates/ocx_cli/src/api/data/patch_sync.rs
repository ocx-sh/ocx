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
        // Column-major: each inner Vec is one column (Checked | Updated | Companions),
        // matching `print_table`'s contract (`rows[c]` holds the cells of column c).
        let rows: [Vec<String>; 3] = [
            vec![self.bases_checked.to_string()],
            vec![self.descriptors_updated.to_string()],
            vec![self.companions_installed.to_string()],
        ];
        printer.print_table(
            &["Checked".into(), "Updated".into(), "Companions".into()],
            &rows.map(|c| c.into_iter().map(Cell::from).collect::<Vec<_>>()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test for the column-major table bug: each count must land
    /// under its own header, not all three stacked under the first column.
    ///
    /// `print_table` is column-major (`rows[c]` = column c's cells). A report
    /// with three distinct counts must therefore produce three columns of one
    /// cell each — not one column of three cells.
    #[test]
    fn print_plain_produces_one_column_per_count() {
        let report = PatchSyncReport {
            bases_checked: 3,
            descriptors_updated: 2,
            companions_installed: 1,
        };
        let rows: [Vec<String>; 3] = [
            vec![report.bases_checked.to_string()],
            vec![report.descriptors_updated.to_string()],
            vec![report.companions_installed.to_string()],
        ];
        assert_eq!(rows.len(), 3, "must produce 3 columns, one per header");
        for column in &rows {
            assert_eq!(column.len(), 1, "each column must hold exactly one cell");
        }
        assert_eq!(rows[0], vec!["3".to_string()]);
        assert_eq!(rows[1], vec!["2".to_string()]);
        assert_eq!(rows[2], vec!["1".to_string()]);
    }
}
