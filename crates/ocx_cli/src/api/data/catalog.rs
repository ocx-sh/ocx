// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashMap;

use serde::Serialize;

use ocx_console::Cell;

use crate::api::Printable;
use crate::api::data::sanitize_for_terminal;

/// Repository catalog listing, optionally including tags per repository.
///
/// Plain format: one-column table (Repository) without tags, or two-column
/// table (Repository | Tag) when tags are included.
#[derive(Serialize, schemars::JsonSchema)]
pub struct Catalog {
    /// One entry per repository: in registry order without `--with-tags`, sorted by name with it.
    pub items: Vec<CatalogEntry>,
    #[serde(skip)]
    with_tags: bool,
}

/// One repository of the catalog.
#[derive(Serialize, schemars::JsonSchema)]
pub struct CatalogEntry {
    /// The repository name.
    pub repository: String,
    /// The repository's tags, sorted; present only when tags were requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}

impl Catalog {
    pub fn without_tags(repositories: Vec<String>) -> Self {
        let items = repositories
            .into_iter()
            .map(|repository| CatalogEntry { repository, tags: None })
            .collect();
        Self {
            items,
            with_tags: false,
        }
    }

    pub fn with_tags(tags: HashMap<String, Vec<String>>) -> Self {
        // Sorted, or output order follows the incoming hash order.
        let mut items: Vec<CatalogEntry> = tags
            .into_iter()
            .map(|(repository, mut repository_tags)| {
                repository_tags.sort();
                CatalogEntry {
                    repository,
                    tags: Some(repository_tags),
                }
            })
            .collect();
        items.sort_by(|a, b| a.repository.cmp(&b.repository));
        Self { items, with_tags: true }
    }

    /// The plain table's column-major rows, neutralized (CWE-150) since names and tags are
    /// registry-authored; the second is empty without tags.
    fn plain_rows(&self) -> [Vec<String>; 2] {
        let mut rows: [Vec<String>; 2] = [Vec::new(), Vec::new()];
        for entry in &self.items {
            match &entry.tags {
                None => rows[0].push(sanitize_for_terminal(&entry.repository)),
                Some(tags) => {
                    for tag in tags {
                        rows[0].push(sanitize_for_terminal(&entry.repository));
                        rows[1].push(sanitize_for_terminal(tag));
                    }
                }
            }
        }
        rows
    }
}

impl Printable for Catalog {
    const SCHEMA_VERSION: u32 = 1;
    const ROOT: &'static str = "Catalog";

    fn print_plain(&self, printer: &ocx_console::DataInterface) {
        let headers: &[ocx_console::Column] = if self.with_tags {
            &["Repository".into(), "Tag".into()]
        } else {
            &["Repository".into()]
        };
        printer.print_table(
            headers,
            &self
                .plain_rows()
                .map(|c| c.into_iter().map(Cell::from).collect::<Vec<_>>()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Assert the quoted keys appear in ascending byte order in the raw JSON.
    ///
    /// Scans the raw string rather than re-parsing: `serde_json::Value` stores
    /// objects in a `BTreeMap` and would re-sort keys, hiding a `HashMap`-order
    /// regression.
    fn assert_keys_ascending(json: &str, keys: &[&str]) {
        let positions: Vec<usize> = keys
            .iter()
            .map(|key| {
                json.find(&format!("\"{key}\""))
                    .unwrap_or_else(|| panic!("key {key:?} missing from output:\n{json}"))
            })
            .collect();
        let mut sorted = positions.clone();
        sorted.sort_unstable();
        assert_eq!(positions, sorted, "keys not in ascending order:\n{json}");
    }

    #[test]
    fn with_tags_emits_sorted_keys_and_sorted_inner_lists() {
        // Intentionally unsorted repository keys + tag lists. Fourteen keys make
        // a HashMap-order match with sorted order vanishingly unlikely, so this
        // test fails on the former HashMap representation.
        let tags: HashMap<String, Vec<String>> = [
            ("mike", vec!["3.2", "1.0", "2.1"]),
            ("alpha", vec!["9.0", "1.1"]),
            ("zeta", vec!["0.2", "0.10", "0.1"]),
            ("november", vec!["2.0"]),
            ("bravo", vec!["1.0"]),
            ("yankee", vec!["4.0"]),
            ("charlie", vec!["1.0"]),
            ("xray", vec!["5.0"]),
            ("delta", vec!["1.0"]),
            ("whiskey", vec!["6.0"]),
            ("echo", vec!["1.0"]),
            ("victor", vec!["7.0"]),
            ("foxtrot", vec!["1.0"]),
            ("uniform", vec!["8.0"]),
        ]
        .into_iter()
        .map(|(key, values)| (key.to_string(), values.into_iter().map(String::from).collect()))
        .collect();

        let catalog = Catalog::with_tags(tags);
        let json = serde_json::to_string_pretty(&catalog).expect("serializes");

        assert_keys_ascending(
            &json,
            &[
                "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "mike", "november", "uniform", "victor",
                "whiskey", "xray", "yankee", "zeta",
            ],
        );

        let tags_of = |name: &str| {
            catalog
                .items
                .iter()
                .find(|entry| entry.repository == name)
                .and_then(|entry| entry.tags.clone())
                .expect("the repository is listed with tags")
        };
        // Lexical (byte) sort, not semver: "0.10" < "0.2" because '1' < '2' at the
        // third byte. Intentional — the contract is determinism, not version order.
        assert_eq!(tags_of("zeta"), ["0.1", "0.10", "0.2"], "inner list must be sorted");
        assert_eq!(tags_of("mike"), ["1.0", "2.1", "3.2"], "inner list must be sorted");
    }

    /// A name carrying every shape the finding measured: a raw ESC (the start of
    /// a CSI sequence), a newline, a NUL, and a right-to-left override.
    const HOSTILE: &str = "ns/\u{1b}[31mev\nil\u{0}\u{202e}gnp.exe";

    #[test]
    fn every_plain_row_is_neutralized() {
        // Behavioural, against the rows `print_table` receives: both table
        // shapes, both columns. A count of sanitizer calls in the source would
        // pass just as well with one raw `.push(` offset by one sanitizer call
        // that is not a row push, and would miss `.extend(...)` entirely.
        let both_shapes = [
            Catalog::without_tags(vec![HOSTILE.to_string()]),
            Catalog::with_tags(HashMap::from([(HOSTILE.to_string(), vec![HOSTILE.to_string()])])),
        ];
        for catalog in both_shapes {
            for column in catalog.plain_rows() {
                for cell in column {
                    assert!(
                        !cell
                            .chars()
                            .any(|c| c.is_control() || crate::api::data::is_bidi_control(c)),
                        "catalog row {cell:?} reached the terminal unneutralized"
                    );
                }
            }
        }
    }

    #[test]
    fn an_ordinary_catalog_passes_through_verbatim() {
        // The neutralization must be invisible for every name a registry
        // legitimately serves, or it silently rewrites the listing.
        let catalog = Catalog::without_tags(vec!["kitware/cmake".to_string(), "ns/pkg".to_string()]);
        assert_eq!(catalog.plain_rows()[0], ["kitware/cmake", "ns/pkg"]);
    }

    #[test]
    fn json_is_an_items_list_with_tags_only_when_requested() {
        let bare = serde_json::to_value(Catalog::without_tags(vec!["ns/pkg".to_string()])).expect("serializes");
        assert_eq!(bare, serde_json::json!({"items": [{"repository": "ns/pkg"}]}));
        let tagged = serde_json::to_value(Catalog::with_tags(HashMap::from([(
            "ns/pkg".to_string(),
            vec!["2.0".to_string(), "1.0".to_string()],
        )])))
        .expect("serializes");
        assert_eq!(
            tagged,
            serde_json::json!({"items": [{"repository": "ns/pkg", "tags": ["1.0", "2.0"]}]})
        );
    }
}
