// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::package::metadata::env::var::ModifierKind;
use serde::Serialize;

use crate::api::Printable;

/// A single resolved environment variable entry, tagged with its modifier kind.
#[derive(Serialize)]
pub struct EnvEntry {
    pub key: String,
    pub value: String,
    #[serde(rename = "type")]
    pub kind: ModifierKind,
}

/// Resolved environment variables for one or more packages, in declaration order.
///
/// Each entry carries its [`ModifierKind`] so callers can apply the correct operation:
/// - [`ModifierKind::Constant`] — replace any existing value for this key.
/// - [`ModifierKind::Path`]     — prepend to any existing value using the platform path separator.
///
/// An ordered list (rather than type-keyed maps) preserves declaration order, allows multiple
/// entries per key with different kinds, and naturally accommodates future modifier types.
///
/// JSON format: `{"entries": [{"key": "...", "value": "...", "type": "constant"|"path"}, ...]}`.
/// The `entries` envelope is the canonical shape shared with `ci export` so consumers
/// can branch on a single shape and so future top-level fields (e.g. `entrypoints`)
/// can be added without breaking the wire format.
#[derive(Serialize)]
pub struct EnvVars {
    pub entries: Vec<EnvEntry>,
}

impl EnvVars {
    pub fn new(entries: Vec<EnvEntry>) -> Self {
        Self { entries }
    }
}

impl Printable for EnvVars {
    fn print_plain(&self, printer: &ocx_lib::cli::Printer) {
        let mut rows: [Vec<String>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        for entry in &self.entries {
            rows[0].push(entry.key.clone());
            rows[1].push(entry.kind.to_string());
            rows[2].push(entry.value.clone());
        }
        printer.print_table(&["Key", "Type", "Value"], &rows);
    }
}
