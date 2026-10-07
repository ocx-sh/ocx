// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

use ocx_console::Cell;
use serde::Serialize;

use crate::api::Printable;

/// Report emitted by `ocx [--global] patch freeze`.
///
/// Plain format: two-column table (`Kind | Count`) summarising how many
/// companions and descriptors were frozen, followed by the snapshot path.
///
/// JSON format: `{ "companions": N, "descriptors": N, "path": "..." }`.
#[derive(Serialize, schemars::JsonSchema)]
pub struct PatchFreezeReport {
    /// Number of companion packages pinned by the snapshot.
    pub companions: usize,
    /// Number of descriptor blobs pinned by the snapshot.
    pub descriptors: usize,
    /// Absolute path of the written `patches.snapshot.json` file.
    pub path: PathBuf,
}

impl PatchFreezeReport {
    pub fn new(companions: usize, descriptors: usize, path: PathBuf) -> Self {
        Self {
            companions,
            descriptors,
            path,
        }
    }
}

impl Printable for PatchFreezeReport {
    const SCHEMA_VERSION: u32 = 1;
    const ROOT: &'static str = "PatchFreezeReport";

    fn print_plain(&self, printer: &ocx_console::DataInterface) {
        let rows: [Vec<String>; 2] = [
            vec!["companions".to_owned(), "descriptors".to_owned()],
            vec![self.companions.to_string(), self.descriptors.to_string()],
        ];
        printer.print_table(
            &["Kind".into(), "Count".into()],
            &rows.map(|c| c.into_iter().map(Cell::from).collect::<Vec<_>>()),
        );
        // Tells the user where to point `OCX_PATCH_SNAPSHOT`.
        printer.print_hint(&format!("snapshot: {}", self.path.display()));
    }
}
