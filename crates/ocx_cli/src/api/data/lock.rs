// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::Serialize;

use crate::api::Printable;

/// A single locked tool entry in the `ocx lock` report.
///
/// `binding` is the TOML key in `ocx.toml` (the local binding name);
/// `group` is `"default"` for entries from the top-level `[tools]`
/// table or the named `[group.*]` key; `digest` is the canonical
/// `registry/repo@digest` form with the advisory tag stripped.
#[derive(Serialize)]
pub struct LockEntry {
    pub binding: String,
    pub group: String,
    pub digest: String,
}

/// Report emitted by `ocx lock` after a successful resolve.
///
/// Plain format: three-column table (Binding | Group | Digest).
///
/// JSON format: array of `{ binding, group, digest }` objects.
#[derive(Serialize)]
#[serde(transparent)]
pub struct LockReport {
    entries: Vec<LockEntry>,
}

impl LockReport {
    pub fn new(entries: Vec<LockEntry>) -> Self {
        Self { entries }
    }
}

impl Printable for LockReport {
    fn print_plain(&self, printer: &ocx_lib::cli::Printer) {
        let mut rows: [Vec<String>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        for entry in &self.entries {
            rows[0].push(entry.binding.clone());
            rows[1].push(entry.group.clone());
            rows[2].push(entry.digest.clone());
        }
        printer.print_table(&["Binding", "Group", "Digest"], &rows);
    }
}
