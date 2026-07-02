// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::cli::Cell;
use ocx_lib::package::metadata::env::var::ModifierKind;
use serde::Serialize;

use crate::api::Printable;
use crate::api::data::env::EntrySource;

/// A single composed environment variable entry shown by `ocx patch test`.
///
/// JSON format: `{ "key": "...", "value": "...", "type": "constant"|"path"[,
/// "source": { "kind": "patch", "rule": "...", "companion": "..." }] }`. The
/// optional `source` object is present only for companion overlay entries and
/// names the rule glob + companion that produced the entry; base-native entries
/// omit it. Shares [`EntrySource`] with `--show-patches`.
#[derive(Serialize)]
pub struct PatchTestEntry {
    pub key: String,
    pub value: String,
    #[serde(rename = "type")]
    pub kind: ModifierKind,
    /// Provenance for a companion overlay entry; `None` for base-native entries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<EntrySource>,
}

/// Report emitted by `ocx patch test` (env-inspection mode, no script/command).
///
/// Shows which companions the descriptor matched for the base identifier and
/// the composed environment that results from overlaying them.
///
/// Plain format: a hint line naming the matched companions, followed by a table
/// of composed env entries (`Key | Type | Value`, plus a `Source` column naming
/// the rule glob + companion when any overlay entry is present).
///
/// JSON format:
/// `{ "base": "...", "companions": ["...", ...], "entries": [{ "key", "value", "type"[,
/// "source": { "kind": "patch", "rule", "companion" }] }, ...] }`.
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

/// Whether any entry carries companion overlay provenance. Gates the plain-table
/// `Source` column so a base with no overlay renders the plain three-column form.
fn has_overlay_entry(entries: &[PatchTestEntry]) -> bool {
    entries.iter().any(|e| e.source.is_some())
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
        if has_overlay_entry(&self.entries) {
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

    fn entry(key: &str, source: Option<EntrySource>) -> PatchTestEntry {
        PatchTestEntry {
            key: key.to_owned(),
            value: "value".to_owned(),
            kind: ModifierKind::Constant,
            source,
        }
    }

    #[test]
    fn overlay_entry_source_names_rule_and_companion() {
        let report = PatchTestReport::new(
            "ocx.sh/java:21".to_owned(),
            vec!["corp/jdk-trust:1.0".to_owned()],
            vec![
                entry("NATIVE_VAR", None),
                entry(
                    "JAVA_TRUST",
                    Some(EntrySource::Patch {
                        rule: "ocx.sh/java:*".to_owned(),
                        companion: "corp/jdk-trust:1.0".to_owned(),
                    }),
                ),
            ],
        );
        let json = serde_json::to_string(&report).expect("serializes");
        // The overlay entry's source names BOTH the rule glob and the companion.
        assert!(
            json.contains(r#""kind":"patch""#),
            "overlay source tagged patch: {json}"
        );
        assert!(
            json.contains(r#""rule":"ocx.sh/java:*""#),
            "names the rule glob: {json}"
        );
        assert!(
            json.contains(r#""companion":"corp/jdk-trust:1.0""#),
            "names the companion: {json}"
        );
    }

    #[test]
    fn native_entry_omits_source() {
        let report = PatchTestReport::new("ocx.sh/cmake:3".to_owned(), Vec::new(), vec![entry("PATH", None)]);
        let json = serde_json::to_string(&report).expect("serializes");
        assert!(!json.contains("\"source\""), "native entry omits source: {json}");
        assert!(!has_overlay_entry(&report.entries));
    }
}
