// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::cli::Cell;
use serde::Serialize;

use crate::api::Printable;

/// A single env var an infrastructure-patch companion contributes to a base.
///
/// `variable` is the env var name, `rule` is the descriptor rule `match` glob
/// that admitted the companion for the base, and `companion` is the companion
/// identifier whose interface projection produced the var.
#[derive(Serialize)]
pub struct PatchWhyEntry {
    pub variable: String,
    pub rule: String,
    pub companion: String,
}

impl PatchWhyEntry {
    pub fn new(variable: String, rule: String, companion: String) -> Self {
        Self {
            variable,
            rule,
            companion,
        }
    }
}

/// Report emitted by `ocx patch why <base>`.
///
/// Names, for every env var a companion overlay contributes to `base`, the
/// descriptor rule glob that matched and the companion identifier that
/// produced it. Empty when no `[patches]` tier is configured, or when a
/// `[patches]` tier is configured but no companion contributes a var to this
/// base — both are a clean "no patches apply" result, not an error.
///
/// Plain format: a `Variable | Rule | Companion` table, one row per
/// contributed var. An empty result prints a one-line "no patches apply to
/// `<base>`" hint instead of an empty table.
///
/// JSON format: a bare array of `{ "variable", "rule", "companion" }`
/// objects (`[]` when no patches apply).
pub struct PatchWhyReport {
    base: String,
    entries: Vec<PatchWhyEntry>,
}

impl PatchWhyReport {
    pub fn new(base: String, entries: Vec<PatchWhyEntry>) -> Self {
        Self { base, entries }
    }
}

impl Serialize for PatchWhyReport {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.entries.serialize(serializer)
    }
}

impl Printable for PatchWhyReport {
    fn print_plain(&self, printer: &ocx_lib::cli::DataInterface) {
        if self.entries.is_empty() {
            printer.print_hint(&format!("no patches apply to '{}'", self.base));
            return;
        }
        let mut rows: [Vec<String>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        for entry in &self.entries {
            rows[0].push(entry.variable.clone());
            rows[1].push(entry.rule.clone());
            rows[2].push(entry.companion.clone());
        }
        printer.print_table(
            &["Variable".into(), "Rule".into(), "Companion".into()],
            &rows.map(|c| c.into_iter().map(Cell::from).collect::<Vec<_>>()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_shape_is_bare_array() {
        let report = PatchWhyReport::new(
            "ocx.sh/java:21".to_owned(),
            vec![PatchWhyEntry::new(
                "JAVA_TRUST".to_owned(),
                "ocx.sh/java:*".to_owned(),
                "corp/jdk-trust:1.0".to_owned(),
            )],
        );
        let json = serde_json::to_string(&report).expect("serializes");
        assert!(json.starts_with('['), "JSON must be a bare array, got {json}");
        assert!(
            json.contains(r#""variable":"JAVA_TRUST""#),
            "names the variable: {json}"
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
    fn empty_report_serializes_to_empty_array() {
        let report = PatchWhyReport::new("ocx.sh/cmake:3".to_owned(), Vec::new());
        let json = serde_json::to_string(&report).expect("serializes");
        assert_eq!(json, "[]", "empty provenance must serialize to an empty array");
    }
}
