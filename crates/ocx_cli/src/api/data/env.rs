// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::fmt;

use ocx_lib::cli::Cell;
use ocx_lib::package::metadata::env::var::ModifierKind;
use serde::Serialize;

use crate::api::Printable;

/// Origin of a resolved environment variable entry.
///
/// `Package` is the native origin — the entry came from the package's own
/// declared metadata; native entries carry `source = None`, so JSON omits the
/// field entirely. `Patch { rule, companion }` is a companion patch overlay
/// entry (`--show-patches`) carrying its provenance: `rule` is the descriptor
/// rule glob that admitted the companion for the base, and `companion` is the
/// companion identifier whose interface projection produced the entry.
///
/// JSON shape (internally tagged on a `kind` discriminator, lowercase):
///
/// ```json
/// "source": { "kind": "patch", "rule": "<glob>", "companion": "<companion-id>" }
/// ```
///
/// The keys are exactly `kind` (always `"patch"` for an overlay entry), `rule`,
/// and `companion`. A native entry has no `source` object at all (the field is
/// skipped). Pre-1.0 this replaces the Phase-4 `"source":"patch"` string with
/// the richer provenance object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum EntrySource {
    // Never constructed — a native entry's `source` stays `None` so JSON omits
    // the field entirely. Kept as the explicit complement to `Patch` so the
    // taxonomy is total.
    #[allow(dead_code)]
    Package,
    Patch {
        rule: String,
        companion: String,
    },
}

impl fmt::Display for EntrySource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EntrySource::Package => write!(f, "package"),
            // Compact single-cell provenance for the plain `--show-patches` table.
            EntrySource::Patch { rule, companion } => write!(f, "{companion} (rule: {rule})"),
        }
    }
}

/// A single resolved environment variable entry, tagged with its modifier kind.
///
/// The optional `source` field is populated by the CLI when `--show-patches` is
/// enabled. It is `None` for package-native entries and
/// `Some(EntrySource::Patch { rule, companion })` for entries that came from a
/// companion overlay (carrying the rule + companion provenance). The field is
/// omitted from JSON output when absent.
#[derive(Serialize)]
pub struct EnvEntry {
    pub key: String,
    pub value: String,
    #[serde(rename = "type")]
    pub kind: ModifierKind,
    /// Origin annotation for `--show-patches`. `None` = package native entry;
    /// `Some(EntrySource::Patch { rule, companion })` = companion overlay entry
    /// carrying its provenance. Skipped in JSON when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<EntrySource>,
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
/// JSON format: `{"entries": [{"key": "...", "value": "...", "type": "constant"|"path"[,
/// "source": {"kind": "patch", "rule": "...", "companion": "..."}]}, ...]}`.
/// The optional `"source"` object is present only when `--show-patches` is passed and only
/// for companion overlay entries; it is omitted for package-native entries. The `entries` envelope is the canonical shape
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

/// Whether any entry carries a companion patch overlay origin. Gates the
/// plain-table Source column — extracted so the decision is unit-testable
/// without capturing `DataInterface`'s stdout writes.
fn has_patch_entry(entries: &[EnvEntry]) -> bool {
    entries
        .iter()
        .any(|e| matches!(e.source, Some(EntrySource::Patch { .. })))
}

impl Printable for EnvVars {
    fn print_plain(&self, printer: &ocx_lib::cli::DataInterface) {
        if has_patch_entry(&self.entries) {
            let mut rows: [Vec<String>; 4] = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
            for entry in &self.entries {
                rows[0].push(entry.key.clone());
                rows[1].push(entry.kind.to_string());
                rows[2].push(entry.value.clone());
                rows[3].push(entry.source.as_ref().map(|s| s.to_string()).unwrap_or_default());
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

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(key: &str, source: Option<EntrySource>) -> EnvEntry {
        EnvEntry {
            key: key.to_owned(),
            value: "value".to_owned(),
            kind: ModifierKind::Constant,
            source,
        }
    }

    fn patch_source(rule: &str, companion: &str) -> EntrySource {
        EntrySource::Patch {
            rule: rule.to_owned(),
            companion: companion.to_owned(),
        }
    }

    #[test]
    fn patch_entry_source_names_rule_and_companion() {
        let vars = EnvVars::new(vec![entry(
            "SSL_CERT_FILE",
            Some(patch_source("*", "internal.corp/certs/ca-bundle:latest")),
        )]);
        let json = serde_json::to_string(&vars).expect("serializes");
        // The provenance object names BOTH the rule glob and the companion.
        assert!(
            json.contains(r#""kind":"patch""#),
            "source must be tagged patch: {json}"
        );
        assert!(json.contains(r#""rule":"*""#), "source must name the rule glob: {json}");
        assert!(
            json.contains(r#""companion":"internal.corp/certs/ca-bundle:latest""#),
            "source must name the companion: {json}"
        );
    }

    #[test]
    fn native_entry_omits_source_field() {
        let vars = EnvVars::new(vec![entry("KEY", None)]);
        let json = serde_json::to_string(&vars).expect("serializes");
        assert!(
            !json.contains("\"source\""),
            "native entry must omit source field, got {json}"
        );
    }

    #[test]
    fn plain_table_hides_source_column_when_all_entries_native() {
        let entries = vec![entry("A", None), entry("B", None)];
        assert!(!has_patch_entry(&entries));
    }

    #[test]
    fn plain_table_shows_source_column_when_any_entry_is_patch() {
        let entries = vec![entry("A", None), entry("B", Some(patch_source("*", "corp/ca:1")))];
        assert!(has_patch_entry(&entries));
    }

    #[test]
    fn entry_source_display_names_rule_and_companion() {
        assert_eq!(
            patch_source("ocx.sh/java:*", "corp/jdk-trust:1.0").to_string(),
            "corp/jdk-trust:1.0 (rule: ocx.sh/java:*)"
        );
        assert_eq!(EntrySource::Package.to_string(), "package");
    }
}
