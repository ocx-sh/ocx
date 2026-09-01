// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::{BTreeMap, HashMap};

use ocx_lib::cli::Cell;
use serde::Serialize;

use crate::api::Printable;
use crate::api::data::sanitize_for_terminal;

/// Tag listing for one or more packages, optionally including platform or variant details.
///
/// Plain format: two-column table (Package | Tag) by default, or
/// (Package | Platform) with `--platforms`, or (Package | Variant) with `--variants`.
///
/// JSON format: object keyed by package name; values are arrays of tags, platforms, or variants.
#[derive(Serialize, schemars::JsonSchema)]
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
#[derive(Serialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum TagsData {
    Tags(BTreeMap<String, Vec<String>>),
    Platforms(BTreeMap<String, Vec<String>>),
    Variants(BTreeMap<String, Vec<String>>),
}

impl Tags {
    /// The plain table's second-column header — the only thing the three
    /// [`TagsData`] variants disagree about.
    fn plain_header(&self) -> &'static str {
        match &self.packages {
            TagsData::Tags(_) => "Tag",
            TagsData::Platforms(_) => "Platform",
            TagsData::Variants(_) => "Variant",
        }
    }

    /// The plain table's column-major rows, already neutralized (CWE-150).
    ///
    /// Package names and tag/platform/variant values are read off a source's
    /// index documents, so they are foreign-authored. Split out of
    /// [`Printable::print_plain`] so a hostile fixture can be asserted on the
    /// rows themselves rather than on a count of sanitizer calls in the source.
    ///
    /// `theme` is taken as an argument because the second column is themed: the
    /// sanitizer runs on the value going **in**, never on `theme.tag`'s output,
    /// which is the theme's own ANSI and would be stripped instead of the
    /// attack. A test passes the plain theme so the rows carry no escapes of
    /// their own.
    fn plain_rows(&self, theme: &ocx_lib::cli::Theme) -> [Vec<String>; 2] {
        let mut rows: [Vec<String>; 2] = [Vec::new(), Vec::new()];
        let (TagsData::Tags(packages) | TagsData::Platforms(packages) | TagsData::Variants(packages)) = &self.packages;
        for (package, values) in packages {
            for value in values {
                rows[0].push(sanitize_for_terminal(package));
                rows[1].push(theme.tag(sanitize_for_terminal(value)));
            }
        }
        rows
    }
}

impl Printable for Tags {
    fn print_plain(&self, printer: &ocx_lib::cli::DataInterface) {
        printer.print_table(
            &["Package".into(), self.plain_header().into()],
            &self
                .plain_rows(&printer.theme())
                .map(|c| c.into_iter().map(Cell::from).collect::<Vec<_>>()),
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

    /// A name carrying every shape the finding measured: a raw ESC (the start of
    /// a CSI sequence), a newline, a NUL, and a right-to-left override.
    const HOSTILE: &str = "ns/\u{1b}[31mev\nil\u{0}\u{202e}gnp.exe";

    /// The no-colour theme, so a row's only escapes would be ones that survived
    /// the sanitizer rather than ones the theme added.
    fn plain_theme() -> ocx_lib::cli::Theme {
        ocx_lib::cli::Theme::new(false)
    }

    #[test]
    fn every_plain_row_is_neutralized() {
        // Behavioural, against the rows `print_table` receives, across all three
        // variants — they share one row builder, and a variant added later that
        // does not is exactly what this must catch. A count of sanitizer calls
        // in the source would pass with one raw `.push(` offset by one sanitizer
        // call that is not a row push, and would miss `.extend(...)` entirely.
        let hostile = || package_map(&[(HOSTILE, &[HOSTILE])]);
        let all_variants = [
            Tags::from_tags(hostile()),
            Tags::from_platforms(hostile()),
            Tags::from_variants(hostile()),
        ];
        for tags in all_variants {
            for column in tags.plain_rows(&plain_theme()) {
                for cell in column {
                    assert!(
                        !cell
                            .chars()
                            .any(|c| c.is_control() || crate::api::data::is_bidi_control(c)),
                        "tag row {cell:?} reached the terminal unneutralized"
                    );
                }
            }
        }
    }

    #[test]
    fn an_ordinary_listing_passes_through_verbatim() {
        // The neutralization must be invisible for every name OCX itself
        // produces, or it silently rewrites the listing.
        let tags = Tags::from_tags(package_map(&[("kitware/cmake", &["3.31.0", "latest"])]));
        let rows = tags.plain_rows(&plain_theme());
        assert_eq!(rows[0], ["kitware/cmake", "kitware/cmake"]);
        assert_eq!(rows[1], ["3.31.0", "latest"]);
    }
}
