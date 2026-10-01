// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_console::Cell;
use serde::Serialize;

use crate::api::Printable;

/// A single env var an infrastructure-patch companion contributes to a base.
///
/// `variable` is the env var name, `rule` is the descriptor rule `match` glob
/// that admitted the companion for the base, and `companion` is the identifier
/// of the companion that produced the var.
#[derive(Serialize, schemars::JsonSchema)]
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

/// `ocx patch why <base>`: each env var a companion contributes to `base`, with the matching rule
/// and companion. Empty is never an error: either no companion applies, or those that apply add
/// nothing to this surface.
pub struct PatchWhyReport {
    base: String,
    /// Every companion composed for `base`, including one that adds no var to this surface.
    companions: Vec<String>,
    entries: Vec<PatchWhyEntry>,
}

impl PatchWhyReport {
    pub fn new(base: String, companions: Vec<String>, entries: Vec<PatchWhyEntry>) -> Self {
        Self {
            base,
            companions,
            entries,
        }
    }
}

impl Serialize for PatchWhyReport {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.entries.serialize(serializer)
    }
}

impl Printable for PatchWhyReport {
    fn print_plain(&self, printer: &ocx_console::DataInterface) {
        if self.entries.is_empty() {
            if self.companions.is_empty() {
                printer.print_hint(&format!("no patches apply to '{}'", self.base));
            } else {
                printer.print_hint(&format!(
                    "companions apply to '{}' but add no variable on this surface: {}",
                    self.base,
                    self.companions.join(", ")
                ));
            }
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

// Transparent `Serialize`: the schema is the bare entry array; `base` never reaches JSON.
impl schemars::JsonSchema for PatchWhyReport {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "PatchWhyReport".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        <Vec<PatchWhyEntry>>::json_schema(generator)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_shape_is_bare_array() {
        let report = PatchWhyReport::new(
            "ocx.sh/java:21".to_owned(),
            vec!["corp/jdk-trust:1.0".to_owned()],
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
        let report = PatchWhyReport::new("ocx.sh/cmake:3".to_owned(), Vec::new(), Vec::new());
        let json = serde_json::to_string(&report).expect("serializes");
        assert_eq!(json, "[]", "empty provenance must serialize to an empty array");
    }
}
