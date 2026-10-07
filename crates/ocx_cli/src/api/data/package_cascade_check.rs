// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Report data for `ocx package cascade check`.

use ocx_console::Cell;
use ocx_package::cascade::graph::{CascadeReport, IndexFinding, Unrepairable};
use serde::Serialize;

use crate::api::Printable;

/// What `cascade check` found, one entry per package in input order.
#[derive(Serialize, schemars::JsonSchema)]
pub struct PackageCascadeCheck {
    /// One report per package, in input order: its alias states, slot rows, index findings and ignored tags.
    pub items: Vec<CascadeReport>,
    /// Packages an index source claims but has no root for yet, so the index layer compared nothing;
    /// plain only, to tell that silence from "agrees". Not serialized: the JSON key set is pinned.
    #[serde(skip)]
    pub index_layer_skipped: Vec<ocx_oci::PackageRef>,
}

impl PackageCascadeCheck {
    pub fn new(items: Vec<CascadeReport>) -> Self {
        Self {
            items,
            index_layer_skipped: Vec::new(),
        }
    }

    /// The plain table's cells, row-major, before styling, so tests can assert them without a terminal.
    fn table_rows(&self) -> Vec<[String; 5]> {
        let mut rows = Vec::new();
        for report in &self.items {
            let package = report
                .logical
                .as_ref()
                .map_or_else(|| report.identifier.to_string(), ToString::to_string);
            for row in &report.rows {
                rows.push([
                    package.clone(),
                    row.tag.to_string(),
                    ocx_oci::render_native_platform(&row.platform),
                    row.status.as_str().to_string(),
                    digest_transition(row.observed.as_deref(), row.expected.as_deref()),
                ]);
            }
            for finding in &report.index_findings {
                let (tag, status, detail) = match finding {
                    IndexFinding::Stale { tag, committed, live } => (
                        tag,
                        "index-stale",
                        format!("{} -> {}", committed.to_short_string(), live.to_short_string()),
                    ),
                    IndexFinding::NotCommitted { tag } => (tag, "index-not-committed", String::new()),
                };
                rows.push([
                    package.clone(),
                    tag.to_string(),
                    String::new(),
                    status.to_string(),
                    detail,
                ]);
            }
            for item in &report.unrepairable {
                let (tag, detail) = match item {
                    Unrepairable::ChildManifestMissing { tag, digest } => {
                        (tag, format!("child manifest gone: {}", digest.to_short_string()))
                    }
                    // Not "gone": this build cannot address the algorithm, so presence was never checked.
                    Unrepairable::ChildDigestUnaddressable { tag, digest } => {
                        (tag, format!("unaddressable digest algorithm: {}", short_digest(digest)))
                    }
                    Unrepairable::WouldEmptyIndex { tag } => (tag, "would empty the index".to_string()),
                };
                rows.push([
                    package.clone(),
                    tag.to_string(),
                    String::new(),
                    "unrepairable".to_string(),
                    detail,
                ]);
            }
        }
        rows
    }
}

impl Printable for PackageCascadeCheck {
    const SCHEMA_VERSION: u32 = 1;
    const ROOT: &'static str = "PackageCascadeCheck";

    fn print_plain(&self, data: &ocx_console::DataInterface) {
        let theme = data.theme();
        let mut columns: [Vec<Cell>; 5] = Default::default();
        for row in self.table_rows() {
            let [package, tag, platform, status, detail] = row;
            columns[0].push(Cell::from(package));
            columns[1].push(Cell::from(theme.tag(&tag)));
            columns[2].push(Cell::from(theme.tag(&platform)));
            columns[3].push(Cell::from(status));
            columns[4].push(Cell::from(theme.digest(&detail)));
        }
        // The Package column is dropped for a single-package run, where it would repeat one value.
        let headers: [ocx_console::Column; 5] = [
            "Package".into(),
            "Tag".into(),
            "Platform".into(),
            "Status".into(),
            "Detail".into(),
        ];
        let first = usize::from(self.items.len() < 2);
        data.print_table(&headers[first..], &columns[first..]);

        // `check` never writes, so its index staleness is the announce hop's to fix, then a local sync.
        for report in &self.items {
            if report.index_findings.is_empty() {
                continue;
            }
            let package = report.logical.as_ref().map_or_else(
                || report.identifier.without_digest().to_string(),
                |logical| logical.without_digest().to_string(),
            );
            data.print_hint(&stale_index_hint(&package));
            data.print_hint(&format!(
                "then refresh the local copy - run: ocx index update {package}"
            ));
        }
        for package in &self.index_layer_skipped {
            data.print_hint(&format!(
                "no index root for {} yet - the index staleness check did not run",
                package.without_digest()
            ));
        }
    }
}

/// The remediation line both cascade subcommands print when the index lags the registry; shared
/// with `package_cascade_repair.rs` so the two lines cannot drift.
/// The package precedes the flags on purpose: a hint is copy-pasted, clap accepts it, tests pin it.
#[must_use]
pub fn stale_index_hint(package: &str) -> String {
    format!("index behind the registry - run: ocx package announce {package} --refresh")
}

/// `observed -> expected` in short digests, or whichever side exists.
fn digest_transition(observed: Option<&str>, expected: Option<&str>) -> String {
    match (observed, expected) {
        (Some(observed), Some(expected)) if observed == expected => short_digest(observed),
        (Some(observed), Some(expected)) => format!("{} -> {}", short_digest(observed), short_digest(expected)),
        (Some(only), None) | (None, Some(only)) => short_digest(only),
        (None, None) => String::new(),
    }
}

/// A digest in short form, verbatim when it does not parse: an index may name an algorithm this
/// build lacks.
fn short_digest(digest: &str) -> String {
    ocx_oci::Digest::try_from(digest).map_or_else(|_| digest.to_string(), |parsed| parsed.to_short_string())
}

#[cfg(test)]
mod tests {
    use ocx_package::cascade::graph::{AliasTag, SlotRow, SlotStatus};

    use super::*;

    fn report_with(rows: Vec<SlotRow>, index_findings: Vec<IndexFinding>) -> CascadeReport {
        CascadeReport {
            identifier: ocx_oci::OciIdentifier::parse_target("registry.test/acme/cmake", ocx_oci::DEFAULT_REGISTRY)
                .unwrap(),
            logical: None,
            aliases: Default::default(),
            rows,
            index_findings,
            ignored_tags: Vec::new(),
            unrepairable: Vec::new(),
        }
    }

    fn version(text: &str) -> ocx_package::version::Version {
        ocx_package::version::Version::parse(text).unwrap()
    }

    fn slot_row(tag: &str, status: SlotStatus, observed: Option<&str>, expected: Option<&str>) -> SlotRow {
        SlotRow {
            tag: AliasTag::Version(version(tag)),
            platform: ocx_oci::native::Platform {
                os: ocx_oci::native::Os::Linux,
                architecture: ocx_oci::native::Arch::Amd64,
                variant: None,
                features: None,
                os_version: None,
                os_features: None,
            },
            status,
            observed: observed.map(str::to_string),
            expected: expected.map(str::to_string),
            source: None,
            observed_source: None,
        }
    }

    const OBSERVED: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const EXPECTED: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[test]
    fn every_slot_and_finding_becomes_one_row() {
        let check = PackageCascadeCheck::new(vec![report_with(
            vec![
                slot_row("3.28", SlotStatus::Stale, Some(OBSERVED), Some(EXPECTED)),
                slot_row("3", SlotStatus::Ok, Some(EXPECTED), Some(EXPECTED)),
            ],
            vec![IndexFinding::NotCommitted {
                tag: AliasTag::Root { variant: None },
            }],
        )]);

        let rows = check.table_rows();

        assert_eq!(rows.len(), 3, "two slots plus one index finding");
        assert_eq!(rows[0][1], "3.28");
        assert_eq!(
            rows[0][2], "linux/amd64",
            "the platform cell uses OCX's canonical os/arch grammar, not the fork's verbose Display"
        );
        assert_eq!(rows[0][3], "stale");
        assert_eq!(
            rows[0][4], "sha256:aaaaaaaaaaaa -> sha256:bbbbbbbbbbbb",
            "a stale slot shows the move it needs"
        );
        assert_eq!(rows[1][3], "ok");
        assert_eq!(
            rows[1][4], "sha256:bbbbbbbbbbbb",
            "an unchanged slot shows one digest, not a transition to itself"
        );
        assert_eq!(rows[2][1], "latest");
        assert_eq!(rows[2][3], "index-not-committed");
        assert!(rows[2][2].is_empty(), "an index finding covers no single platform");
    }

    #[test]
    fn the_two_unrepairable_child_reasons_read_differently() {
        // Both carry a digest and both land under `unrepairable`, so without a
        // word each the table cannot say which one happened - and they call
        // for opposite responses: republish the content, versus this build
        // cannot address that algorithm at all.
        let mut report = report_with(Vec::new(), Vec::new());
        report.unrepairable = vec![
            Unrepairable::ChildManifestMissing {
                tag: AliasTag::Root { variant: None },
                digest: ocx_oci::Digest::try_from(OBSERVED).unwrap(),
            },
            Unrepairable::ChildDigestUnaddressable {
                tag: AliasTag::Root { variant: None },
                digest: "blake3:0011".to_string(),
            },
        ];
        let check = PackageCascadeCheck::new(vec![report]);

        let rows = check.table_rows();

        assert_eq!(rows[0][3], "unrepairable");
        assert_eq!(rows[0][4], "child manifest gone: sha256:aaaaaaaaaaaa");
        assert_eq!(
            rows[1][4], "unaddressable digest algorithm: blake3:0011",
            "an algorithm this build cannot parse is still shown verbatim"
        );
    }

    #[test]
    fn the_platform_cell_carries_variant_and_os_features() {
        let mut row = slot_row("3.28", SlotStatus::Ok, Some(OBSERVED), Some(OBSERVED));
        row.platform = ocx_oci::native::Platform {
            os: ocx_oci::native::Os::Linux,
            architecture: ocx_oci::native::Arch::ARM64,
            variant: Some("v8".to_string()),
            features: None,
            os_version: None,
            os_features: Some(vec!["libc.glibc".to_string()]),
        };
        let check = PackageCascadeCheck::new(vec![report_with(vec![row], Vec::new())]);

        assert_eq!(
            check.table_rows()[0][2],
            "linux/arm64/v8+libc.glibc",
            "the plain table renders the same os/arch/variant+feature grammar as --platform, \
             not the fork's `( architecture: ..., os-features: ..., )` Display"
        );
    }

    #[test]
    fn a_shadowed_entry_is_a_duplicate_row() {
        let check = PackageCascadeCheck::new(vec![report_with(
            vec![slot_row("3.28", SlotStatus::Duplicate, Some(OBSERVED), Some(EXPECTED))],
            Vec::new(),
        )]);

        assert_eq!(check.table_rows()[0][3], "duplicate");
    }

    #[test]
    fn a_missing_slot_shows_only_the_expected_digest() {
        let check = PackageCascadeCheck::new(vec![report_with(
            vec![slot_row("3.28", SlotStatus::Missing, None, Some(EXPECTED))],
            Vec::new(),
        )]);

        assert_eq!(check.table_rows()[0][4], "sha256:bbbbbbbbbbbb");
    }

    // ── JSON key stability ───────────────────────────────────────────────
    //
    // The reference docs show the per-report JSON shape as representative,
    // not frozen byte-for-byte -- but the wrapper key and the report's own
    // field set are exactly what a `--format json` consumer parses, so they
    // are pinned here rather than left to drift unnoticed.

    #[test]
    fn json_top_level_is_an_items_list() {
        let check = PackageCascadeCheck::new(vec![report_with(Vec::new(), Vec::new())]);
        let value = serde_json::to_value(&check).unwrap();
        let mut keys: Vec<&str> = value
            .as_object()
            .expect("top-level JSON is an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["items"], "the whole JSON contract is this one list key");
    }

    /// Every union in the report is tagged by `type` in snake_case, and an unset digest is omitted.
    #[test]
    fn json_unions_are_type_tagged_and_unset_digests_are_omitted() {
        let mut report = report_with(
            vec![slot_row("3.28", SlotStatus::Missing, None, Some(EXPECTED))],
            vec![IndexFinding::NotCommitted {
                tag: AliasTag::Root { variant: None },
            }],
        );
        report.aliases.insert(
            AliasTag::Root { variant: None },
            ocx_package::cascade::graph::AliasState::NotAnIndex {
                digest: ocx_oci::Digest::try_from(OBSERVED).unwrap(),
            },
        );
        report.unrepairable = vec![Unrepairable::ChildDigestUnaddressable {
            tag: AliasTag::Root { variant: None },
            digest: "blake3:0011".to_string(),
        }];
        let value = serde_json::to_value(PackageCascadeCheck::new(vec![report])).unwrap();
        let item = &value["items"][0];
        assert_eq!(
            item["aliases"]["latest"],
            serde_json::json!({"type": "not_an_index", "digest": OBSERVED})
        );
        assert_eq!(
            item["index_findings"][0],
            serde_json::json!({"type": "not_committed", "tag": "latest"})
        );
        assert_eq!(
            item["unrepairable"][0],
            serde_json::json!({"type": "child_digest_unaddressable", "tag": "latest", "digest_text": "blake3:0011"})
        );
        let row = item["rows"][0].as_object().unwrap();
        assert!(
            !row.contains_key("observed"),
            "an unset digest is omitted, never null: {row:?}"
        );
        assert!(!item.as_object().unwrap().contains_key("logical"), "{item}");
    }

    #[test]
    fn the_skipped_index_layer_note_never_reaches_the_json() {
        let mut check = PackageCascadeCheck::new(vec![report_with(Vec::new(), Vec::new())]);
        check.index_layer_skipped = vec![ocx_oci::PackageRef::parse("registry.test/acme/cmake").unwrap()];

        let value = serde_json::to_value(&check).unwrap();
        let keys: Vec<&str> = value
            .as_object()
            .expect("top-level JSON is an object")
            .keys()
            .map(String::as_str)
            .collect();

        assert_eq!(
            keys,
            vec!["items"],
            "a plain-mode note must not grow the pinned key set: {value}"
        );
    }

    #[test]
    fn json_report_key_set_matches_the_documented_shape() {
        let check = PackageCascadeCheck::new(vec![report_with(
            vec![slot_row("3.28", SlotStatus::Stale, Some(OBSERVED), Some(EXPECTED))],
            vec![IndexFinding::NotCommitted {
                tag: AliasTag::Root { variant: None },
            }],
        )]);
        let value = serde_json::to_value(&check).unwrap();
        let mut keys: Vec<&str> = value["items"][0]
            .as_object()
            .expect("each report is an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "aliases",
                "identifier",
                "ignored_tags",
                "index_findings",
                "rows",
                "unrepairable",
            ],
            "pin the field set a --format json consumer parses; an unset `logical` is omitted: {value}"
        );
    }

    /// C-062 / DX-70: the remediation line ocx prints at an operator names the
    /// **positional** announce form.
    ///
    /// Asserted against the pure builder, which is why the builder exists
    /// (E-21): the four remediation strings were inline `format!` arguments to
    /// [`ocx_console::DataInterface::print_hint`], which writes the real
    /// stdout, so they were covered by **no** assertion anywhere — the nearest
    /// one, `test/tests/test_package_cascade.py:470`, checks `--tags-file` and
    /// contains no `--package` at all. Migrating them would have reded nothing,
    /// and they are the reason C-062's sweep is correctness rather than
    /// housekeeping: leaving them behind makes ocx's own output tell a user to
    /// run a form ocx warns about, and at 0.7 a form that does not exist.
    ///
    /// E-20 asked for a per-site assertion, because `check` and `repair` print
    /// this line byte-identically and a shared assertion would pass with either
    /// site un-migrated. The stub answered it one level better: there is one
    /// builder, called from both, so "check migrated but repair did not" is
    /// unrepresentable rather than merely asserted against.
    ///
    /// Red at the stub: `stale_index_hint` is `unimplemented!()`.
    /// Mutation once implemented: write `announce --package {package}` back into
    /// the builder.
    #[test]
    fn cascade_remediation_strings_use_the_positional_form() {
        let hint = stale_index_hint("acme/widget");
        assert!(
            hint.contains("ocx package announce acme/widget"),
            "the package is named positionally, flags after it: {hint}"
        );
        assert!(
            !hint.contains("--package"),
            "ocx must not tell an operator to run the spelling it deprecates: {hint}"
        );
        assert!(
            hint.contains("--refresh"),
            "a stale index is repaired by re-observing the committed set: {hint}"
        );
    }
}
