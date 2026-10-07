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
    /// The env var name.
    pub variable: String,
    /// The descriptor rule `match` glob that admitted the companion.
    pub rule: String,
    /// The companion that produced the var.
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
#[derive(Serialize, schemars::JsonSchema)]
pub struct PatchWhyReport {
    #[serde(skip)]
    base: String,
    /// Every companion composed for `base`, including one that adds no var to this surface.
    #[serde(skip)]
    companions: Vec<String>,
    /// One entry per contributed variable; empty when no companion applies or none adds a var here.
    items: Vec<PatchWhyEntry>,
}

impl PatchWhyReport {
    pub fn new(base: String, companions: Vec<String>, items: Vec<PatchWhyEntry>) -> Self {
        Self {
            base,
            companions,
            items,
        }
    }
}

impl Printable for PatchWhyReport {
    const SCHEMA_VERSION: u32 = 1;
    const ROOT: &'static str = "PatchWhyReport";

    fn print_plain(&self, printer: &ocx_console::DataInterface) {
        if self.items.is_empty() {
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
        for entry in &self.items {
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
    fn json_shape_is_an_items_list() {
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
        assert!(
            json.starts_with(r#"{"items":["#),
            "JSON must be an items list, got {json}"
        );
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
        assert_eq!(
            json, r#"{"items":[]}"#,
            "empty provenance must serialize to an empty items list"
        );
    }
}
