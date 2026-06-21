// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::cli::Cell;
use ocx_lib::package::metadata::env::var::ModifierKind;
use serde::Serialize;

use crate::api::Printable;

/// A single composed environment variable entry shown by `ocx patch test`.
///
/// JSON format: `{ "key": "...", "value": "...", "type": "constant"|"path" }`.
#[derive(Serialize)]
pub struct PatchTestEntry {
    pub key: String,
    pub value: String,
    #[serde(rename = "type")]
    pub kind: ModifierKind,
}

/// Report emitted by `ocx patch test` (env-inspection mode, no script/command).
///
/// Shows which companions the descriptor matched for the base identifier and
/// the composed environment that results from overlaying them.
///
/// Plain format: a hint line naming the matched companions, followed by a
/// three-column table (`Key | Type | Value`) of composed env entries.
///
/// JSON format:
/// `{ "base": "...", "companions": ["...", ...], "entries": [{ "key", "value", "type" }, ...] }`.
#[derive(Serialize)]
pub struct PatchTestReport {
    /// The base identifier the descriptor was composed onto.
    pub base: String,
    /// Companion identifiers that matched the base under the descriptor's rules.
    pub companions: Vec<String>,
    /// Composed environment entries (base interface surface + companion overlay).
    pub entries: Vec<PatchTestEntry>,
}

impl PatchTestReport {
    /// Build a patch-test env-inspection report.
    pub fn new(base: String, companions: Vec<String>, entries: Vec<PatchTestEntry>) -> Self {
        Self {
            base,
            companions,
            entries,
        }
    }
}

impl Printable for PatchTestReport {
    fn print_plain(&self, printer: &ocx_lib::cli::DataInterface) {
        if self.companions.is_empty() {
            printer.print_hint(&format!("no companions matched '{}'", self.base));
        } else {
            printer.print_hint(&format!(
                "matched {} companion(s) for '{}': {}",
                self.companions.len(),
                self.base,
                self.companions.join(", ")
            ));
        }
        let mut rows: [Vec<String>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        for entry in &self.entries {
            rows[0].push(entry.key.clone());
            rows[1].push(entry.kind.to_string());
            rows[2].push(entry.value.clone());
        }
        printer.print_table(
            &["Key".into(), "Type".into(), "Value".into()],
            &rows.map(|c| c.into_iter().map(Cell::from).collect::<Vec<_>>()),
        );
    }
}
