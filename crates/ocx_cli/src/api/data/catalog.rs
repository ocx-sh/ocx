// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::{BTreeMap, HashMap};

use serde::Serialize;

use ocx_lib::cli::Cell;

use crate::api::Printable;

/// Repository catalog listing, optionally including tags per repository.
///
/// Plain format: one-column table (Repository) without tags, or two-column
/// table (Repository | Tag) when tags are included.
///
/// JSON format: array of repository names without tags, or object keyed by
/// repository name with tag arrays as values.
#[derive(Serialize)]
pub struct Catalog {
    pub repositories: CatalogData,
}

impl Catalog {
    pub fn new(repositories: CatalogData) -> Self {
        Self { repositories }
    }

    pub fn without_tags(repositories: Vec<String>) -> Self {
        Self::new(CatalogData::WithoutTags(repositories))
    }

    pub fn with_tags(tags: HashMap<String, Vec<String>>) -> Self {
        // Sort repository keys and each tag list so the table and JSON outputs
        // are reproducible regardless of the incoming hash order.
        let sorted = tags
            .into_iter()
            .map(|(repository, mut repository_tags)| {
                repository_tags.sort();
                (repository, repository_tags)
            })
            .collect();
        Self::new(CatalogData::WithTags(sorted))
    }
}

/// Polymorphic catalog payload: either a plain list of repository names or a
/// map of repository names to their tags.
#[derive(Serialize)]
#[serde(untagged)]
pub enum CatalogData {
    WithoutTags(Vec<String>),
    WithTags(BTreeMap<String, Vec<String>>),
}

impl Printable for Catalog {
    fn print_plain(&self, printer: &ocx_lib::cli::DataInterface) {
        let mut rows: [Vec<String>; 2] = [Vec::new(), Vec::new()];
        match &self.repositories {
            CatalogData::WithoutTags(repos) => {
                for repo in repos {
                    rows[0].push(repo.clone());
                }
                printer.print_table(
                    &["Repository".into()],
                    &rows.map(|c| c.into_iter().map(Cell::from).collect::<Vec<_>>()),
                );
            }
            CatalogData::WithTags(tags) => {
                for (repo, repo_tags) in tags {
                    for tag in repo_tags {
                        rows[0].push(repo.clone());
                        rows[1].push(tag.clone());
                    }
                }
                printer.print_table(
                    &["Repository".into(), "Tag".into()],
                    &rows.map(|c| c.into_iter().map(Cell::from).collect::<Vec<_>>()),
                );
            }
        }
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

        let CatalogData::WithTags(map) = &catalog.repositories else {
            panic!("expected WithTags variant");
        };
        // Lexical (byte) sort, not semver: "0.10" < "0.2" because '1' < '2' at the
        // third byte. Intentional — the contract is determinism, not version order.
        assert_eq!(map["zeta"], ["0.1", "0.10", "0.2"], "inner list must be sorted");
        assert_eq!(map["mike"], ["1.0", "2.1", "3.2"], "inner list must be sorted");
    }
}
