// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashMap;

use ocx_console::Cell;
use serde::Serialize;

use crate::api::Printable;
use crate::api::data::sanitize_for_terminal;

/// Tag listing for one or more packages, optionally including platform or variant details.
///
/// Plain format: two-column table (Package | Tag) by default, or
/// (Package | Platform) with `--platforms`, or (Package | Variant) with `--variants`.
#[derive(Serialize, schemars::JsonSchema)]
pub struct Tags {
    /// One entry per package, sorted by package.
    pub items: Vec<TagsEntry>,
    #[serde(skip)]
    header: &'static str,
}

/// One package's listing; exactly one of `tags`, `platforms` and `variants` is present, chosen by the flag.
#[derive(Serialize, schemars::JsonSchema)]
pub struct TagsEntry {
    /// The package as given.
    pub package: String,
    /// Its tags, sorted; present without `--platforms` and `--variants`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    /// The platforms of the listed tag, sorted; present under `--platforms`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platforms: Option<Vec<ocx_oci::Platform>>,
    /// Its variant names, sorted, `""` naming the default variant; present under `--variants`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variants: Option<Vec<String>>,
}

impl TagsEntry {
    fn new(package: String) -> Self {
        Self {
            package,
            tags: None,
            platforms: None,
            variants: None,
        }
    }

    /// The listed values as the plain table spells them.
    fn values(&self) -> Vec<String> {
        match (&self.tags, &self.platforms, &self.variants) {
            (Some(values), _, _) | (_, _, Some(values)) => values.clone(),
            (_, Some(platforms), _) => platforms.iter().map(ToString::to_string).collect(),
            (None, None, None) => Vec::new(),
        }
    }
}

impl Tags {
    pub fn from_tags(packages: HashMap<String, impl IntoIterator<Item = String>>) -> Self {
        Self::sorted(packages, "Tag", |entry, values| entry.tags = Some(sorted(values)))
    }

    pub fn from_platforms(packages: HashMap<String, Vec<ocx_oci::Platform>>) -> Self {
        Self::sorted(packages, "Platform", |entry, mut values| {
            values.sort_by_cached_key(ToString::to_string);
            entry.platforms = Some(values);
        })
    }

    pub fn from_variants(packages: HashMap<String, Vec<String>>) -> Self {
        Self::sorted(packages, "Variant", |entry, values| {
            entry.variants = Some(sorted(values))
        })
    }

    /// Sorts packages so table and JSON output do not follow hash order.
    fn sorted<V>(packages: HashMap<String, V>, header: &'static str, fill: impl Fn(&mut TagsEntry, V)) -> Self {
        let mut items: Vec<TagsEntry> = packages
            .into_iter()
            .map(|(package, values)| {
                let mut entry = TagsEntry::new(package);
                fill(&mut entry, values);
                entry
            })
            .collect();
        items.sort_by(|a, b| a.package.cmp(&b.package));
        Self { items, header }
    }

    fn plain_header(&self) -> &'static str {
        self.header
    }

    /// Column-major rows, neutralized (CWE-150): names and values are index-authored.
    fn plain_rows(&self, theme: &ocx_console::Theme) -> [Vec<String>; 2] {
        let mut rows: [Vec<String>; 2] = [Vec::new(), Vec::new()];
        for entry in &self.items {
            for value in entry.values() {
                rows[0].push(sanitize_for_terminal(&entry.package));
                // Sanitize before `theme.tag`, never after, or its own ANSI is stripped instead of the attack.
                rows[1].push(theme.tag(sanitize_for_terminal(&value)));
            }
        }
        rows
    }
}

/// Sorts one value list; lexical, so the empty default variant comes first.
fn sorted(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut list: Vec<String> = values.into_iter().collect();
    list.sort();
    list
}

impl Printable for Tags {
    const SCHEMA_VERSION: u32 = 1;
    const ROOT: &'static str = "Tags";

    fn print_plain(&self, printer: &ocx_console::DataInterface) {
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

    /// The plain-spelled values one package lists.
    fn values_of(tags: &Tags, package: &str) -> Vec<String> {
        tags.items
            .iter()
            .find(|entry| entry.package == package)
            .map(TagsEntry::values)
            .expect("the package is listed")
    }

    /// [`package_map`] with every value parsed as a platform.
    fn platform_map(pairs: &[(&str, &[&str])]) -> HashMap<String, Vec<ocx_oci::Platform>> {
        package_map(pairs)
            .into_iter()
            .map(|(key, values)| {
                let platforms = values
                    .iter()
                    .map(|value| value.parse().expect("a valid platform"))
                    .collect();
                (key, platforms)
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

        // Lexical (byte) sort, not semver: "0.10" < "0.2" because '1' < '2' at the
        // third byte. Intentional — the contract is determinism, not version order.
        assert_eq!(
            values_of(&tags, "zeta"),
            ["0.1", "0.10", "0.2"],
            "inner list must be sorted"
        );
        assert_eq!(
            values_of(&tags, "mike"),
            ["1.0", "2.1", "3.2"],
            "inner list must be sorted"
        );
        assert_eq!(
            serde_json::to_value(&tags).expect("serializes")["items"][0],
            serde_json::json!({"package": "alpha", "tags": ["1.1", "9.0"]}),
            "a tag listing carries `tags` only"
        );
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

        // Lexical sort keeps the empty default variant first.
        assert_eq!(values_of(&tags, "mike"), ["", "gnu", "musl"]);
    }

    #[test]
    fn from_platforms_emits_sorted_keys_and_sorted_inner_lists() {
        // Platforms are typed, so their ordering is checked apart from the string lists.
        let packages = platform_map(&[
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

        assert_eq!(
            values_of(&tags, "mike"),
            ["darwin/arm64", "linux/amd64", "windows/amd64"],
            "inner list must be sorted"
        );
        assert_eq!(
            serde_json::to_value(&tags).expect("serializes")["items"][0],
            serde_json::json!({"package": "alpha", "platforms": [{"architecture": "arm64", "os": "linux"}]}),
            "a platform is the OCI platform object, not its flag spelling"
        );
    }

    /// A name carrying every shape the finding measured: a raw ESC (the start of
    /// a CSI sequence), a newline, a NUL, and a right-to-left override.
    const HOSTILE: &str = "ns/\u{1b}[31mev\nil\u{0}\u{202e}gnp.exe";

    /// The no-colour theme, so a row's only escapes would be ones that survived
    /// the sanitizer rather than ones the theme added.
    fn plain_theme() -> ocx_console::Theme {
        ocx_console::Theme::new(false)
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
            // A platform is parsed, so only its package name can carry the attack.
            Tags::from_platforms(platform_map(&[(HOSTILE, &["linux/amd64"])])),
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
