// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::Serialize;

use super::env::EnvEntry;
use crate::api::Printable;

/// Environment variables exported to a CI system.
///
/// Plain format: three-column table (Key | Value | Type).
///
/// JSON format: `{"entries": [{"key": "...", "value": "...", "type": "constant"|"path"}, ...]}`.
/// Shares the canonical `entries` envelope with `env` so consumers can branch on a single
/// shape and so future top-level fields can be added without breaking the wire format.
#[derive(Serialize)]
pub struct CiExported {
    entries: Vec<EnvEntry>,
}

impl CiExported {
    pub fn new(entries: Vec<EnvEntry>) -> Self {
        Self { entries }
    }
}

impl Printable for CiExported {
    fn print_plain(&self, printer: &ocx_lib::cli::Printer) {
        let mut rows: [Vec<String>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        for entry in &self.entries {
            rows[0].push(entry.key.clone());
            rows[1].push(entry.value.clone());
            rows[2].push(entry.kind.to_string());
        }
        printer.print_table(&["Key", "Value", "Type"], &rows);
    }
}
