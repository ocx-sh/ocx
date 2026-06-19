// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::{BTreeMap, HashMap};

use ocx_lib::cli::Cell;
use serde::Serialize;

use crate::api::Printable;

/// Tag listing for one or more packages, optionally including platform or variant details.
///
/// Plain format: two-column table (Package | Tag) by default, or
/// (Package | Platform) with `--platforms`, or (Package | Variant) with `--variants`.
///
/// JSON format: object keyed by package name; values are arrays of tags, platforms, or variants.
#[derive(Serialize)]
pub struct Tags {
    #[serde(flatten)]
    pub packages: TagsData,
}

impl Tags {
    pub fn from_tags(packages: HashMap<String, impl IntoIterator<Item = String>>) -> Self {
        Self {
            packages: TagsData::Tags(into_sorted(packages)),
        }
    }

    pub fn from_platforms(packages: HashMap<String, Vec<String>>) -> Self {
        Self {
            packages: TagsData::Platforms(into_sorted(packages)),
        }
    }

    pub fn from_variants(packages: HashMap<String, Vec<String>>) -> Self {
        Self {
            packages: TagsData::Variants(into_sorted(packages)),
        }
    }
}

/// Collect a package map into a `BTreeMap` (sorted keys) with each value list
/// sorted lexically, so both the table renderer and the JSON serializer emit a
/// deterministic, reproducible order regardless of the incoming hash order.
fn into_sorted(packages: HashMap<String, impl IntoIterator<Item = String>>) -> BTreeMap<String, Vec<String>> {
    packages
        .into_iter()
        .map(|(package, values)| {
            let mut list: Vec<String> = values.into_iter().collect();
            list.sort();
            (package, list)
        })
        .collect()
}

/// Polymorphic tag payload.
#[derive(Serialize)]
#[serde(untagged)]
pub enum TagsData {
    Tags(BTreeMap<String, Vec<String>>),
    Platforms(BTreeMap<String, Vec<String>>),
    Variants(BTreeMap<String, Vec<String>>),
}

impl Printable for Tags {
    fn print_plain(&self, printer: &ocx_lib::cli::DataInterface) {
        let theme = printer.theme();
        let mut rows: [Vec<String>; 2] = [Vec::new(), Vec::new()];
        let header = match &self.packages {
            TagsData::Tags(tags) => {
                for (package, package_tags) in tags {
                    for tag in package_tags {
                        rows[0].push(package.clone());
                        rows[1].push(theme.tag(tag));
                    }
                }
                "Tag"
            }
            TagsData::Platforms(platforms) => {
                for (package, platform_list) in platforms {
                    for platform in platform_list {
                        rows[0].push(package.clone());
                        rows[1].push(theme.tag(platform));
                    }
                }
                "Platform"
            }
            TagsData::Variants(variants) => {
                for (package, variant_names) in variants {
                    for variant in variant_names {
                        rows[0].push(package.clone());
                        rows[1].push(theme.tag(variant));
                    }
                }
                "Variant"
            }
        };
        printer.print_table(
            &["Package".into(), header.into()],
            &rows.map(|c| c.into_iter().map(Cell::from).collect::<Vec<_>>()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a package map from `(key, [values])` pairs, owning every string.
    fn package_map(pairs: &[(&str, &[&str])]) -> HashMap<String, Vec<String>> {
        pairs
            .iter()
            .map(|(key, values)| {
                (
                    (*key).to_string(),
                    values.iter().map(|value| (*value).to_string()).collect(),
                )
            })
            .collect()
    }

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
    fn from_tags_emits_sorted_keys_and_sorted_inner_lists() {
        // Intentionally unsorted keys + inner lists. Fourteen keys make a
        // HashMap-order match with sorted order vanishingly unlikely, so this
        // test fails on the former HashMap representation.
        let packages = package_map(&[
            ("mike", &["3.2", "1.0", "2.1"]),
            ("alpha", &["9.0", "1.1"]),
            ("zeta", &["0.2", "0.10", "0.1"]),
            ("november", &["2.0"]),
            ("bravo", &["1.0"]),
            ("yankee", &["4.0"]),
            ("charlie", &["1.0"]),
            ("xray", &["5.0"]),
            ("delta", &["1.0"]),
            ("whiskey", &["6.0"]),
            ("echo", &["1.0"]),
            ("victor", &["7.0"]),
            ("foxtrot", &["1.0"]),
            ("uniform", &["8.0"]),
        ]);

        let tags = Tags::from_tags(packages);
        let json = serde_json::to_string_pretty(&tags).expect("serializes");

        assert_keys_ascending(
            &json,
            &[
                "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "mike", "november", "uniform", "victor",
                "whiskey", "xray", "yankee", "zeta",
            ],
        );

        let TagsData::Tags(map) = &tags.packages else {
            panic!("expected Tags variant");
        };
        // Lexical (byte) sort, not semver: "0.10" < "0.2" because '1' < '2' at the
        // third byte. Intentional — the contract is determinism, not version order.
        assert_eq!(map["zeta"], ["0.1", "0.10", "0.2"], "inner list must be sorted");
        assert_eq!(map["mike"], ["1.0", "2.1", "3.2"], "inner list must be sorted");
    }

    #[test]
    fn from_variants_emits_sorted_keys_with_empty_default_first() {
        let packages = package_map(&[
            ("mike", &["musl", "", "gnu"]),
            ("alpha", &["static"]),
            ("zeta", &[""]),
            ("november", &["x"]),
            ("bravo", &["y"]),
            ("yankee", &["z"]),
            ("charlie", &["a"]),
            ("xray", &["b"]),
            ("delta", &["c"]),
            ("whiskey", &["d"]),
            ("echo", &["e"]),
            ("victor", &["f"]),
            ("foxtrot", &["g"]),
            ("uniform", &["h"]),
        ]);

        let tags = Tags::from_variants(packages);
        let json = serde_json::to_string_pretty(&tags).expect("serializes");

        assert_keys_ascending(
            &json,
            &[
                "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "mike", "november", "uniform", "victor",
                "whiskey", "xray", "yankee", "zeta",
            ],
        );

        let TagsData::Variants(map) = &tags.packages else {
            panic!("expected Variants variant");
        };
        // Lexical sort keeps the empty default variant first.
        assert_eq!(map["mike"], ["", "gnu", "musl"]);
    }

    #[test]
    fn from_platforms_emits_sorted_keys_and_sorted_inner_lists() {
        // `from_platforms` shares `into_sorted` with the other constructors, but the
        // `Platforms` variant is type-distinct: this guards against a future revert of
        // its field to `HashMap`, which the from_tags/from_variants tests would miss.
        let packages = package_map(&[
            ("mike", &["windows/amd64", "linux/amd64", "darwin/arm64"]),
            ("alpha", &["linux/arm64"]),
            ("zeta", &["linux/amd64"]),
            ("november", &["darwin/amd64"]),
            ("bravo", &["linux/amd64"]),
            ("yankee", &["linux/arm64"]),
            ("charlie", &["linux/amd64"]),
            ("xray", &["darwin/amd64"]),
            ("delta", &["linux/amd64"]),
            ("whiskey", &["linux/arm64"]),
            ("echo", &["linux/amd64"]),
            ("victor", &["darwin/amd64"]),
            ("foxtrot", &["linux/amd64"]),
            ("uniform", &["linux/arm64"]),
        ]);

        let tags = Tags::from_platforms(packages);
        let json = serde_json::to_string_pretty(&tags).expect("serializes");

        assert_keys_ascending(
            &json,
            &[
                "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "mike", "november", "uniform", "victor",
                "whiskey", "xray", "yankee", "zeta",
            ],
        );

        let TagsData::Platforms(map) = &tags.packages else {
            panic!("expected Platforms variant");
        };
        assert_eq!(
            map["mike"],
            ["darwin/arm64", "linux/amd64", "windows/amd64"],
            "inner list must be sorted"
        );
    }
}
