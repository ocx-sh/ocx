// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::cli::Cell;
use ocx_lib::package::metadata::env::var::ModifierKind;
use serde::Serialize;

use crate::api::Printable;

/// Wire value for the `source` field when an entry originates from a companion
/// patch overlay (`--show-patches`). Single source of truth — avoids the
/// stringly-typed `"patch".to_string()` literal scattered across callers.
pub const SOURCE_PATCH: &str = "patch";

/// A single resolved environment variable entry, tagged with its modifier kind.
///
/// The optional `source` field is populated by the CLI when `--show-patches` is
/// enabled. It is `None` for package-native entries and `Some("patch")` for entries
/// that came from a companion overlay. The field is omitted from JSON output when
/// absent, preserving wire-format compatibility.
#[derive(Serialize)]
pub struct EnvEntry {
    pub key: String,
    pub value: String,
    #[serde(rename = "type")]
    pub kind: ModifierKind,
    /// Origin annotation for `--show-patches`. `None` = package native entry;
    /// `Some("patch")` = companion overlay entry. Skipped in JSON when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
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
/// JSON format: `{"entries": [{"key": "...", "value": "...", "type": "constant"|"path"[, "source": "patch"]}, ...]}`.
/// The optional `"source"` field is present only when `--show-patches` is passed; it is
/// omitted for package-native entries. The `entries` envelope is the canonical shape
/// shared with `ci export` so consumers can branch on a single shape and so future
/// top-level fields (e.g. `entrypoints`) can be added without breaking the wire format.
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
    fn print_plain(&self, printer: &ocx_lib::cli::DataInterface) {
        let show_source = self.entries.iter().any(|e| e.source.is_some());
        if show_source {
            let mut rows: [Vec<String>; 4] = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
            for entry in &self.entries {
                rows[0].push(entry.key.clone());
                rows[1].push(entry.kind.to_string());
                rows[2].push(entry.value.clone());
                rows[3].push(entry.source.clone().unwrap_or_default());
            }
            printer.print_table(
                &["Key".into(), "Type".into(), "Value".into(), "Source".into()],
                &rows.map(|c| c.into_iter().map(Cell::from).collect::<Vec<_>>()),
            );
        } else {
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
}
