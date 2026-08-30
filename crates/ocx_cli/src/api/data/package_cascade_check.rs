// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Report data for `ocx package cascade check`.

use ocx_lib::cli::Cell;
use ocx_lib::oci;
use ocx_lib::package::cascade::graph::{CascadeReport, IndexFinding, SlotStatus, Unrepairable};
use serde::Serialize;

use crate::api::Printable;

/// What `cascade check` found, one entry per package in input order.
///
/// Holds the library's report values as they are rather than re-modelling
/// them: the finding vocabulary belongs to the algebra that produced it, and a
/// second copy of the same fields here would be a second thing to keep in
/// step.
///
/// Plain format: one row per (tag, platform) slot, plus one row per index
/// finding and per unrepairable item, folded into the same table (they are
/// findings about the same graph, not tables of their own).
///
/// JSON format: `{ "reports": [...] }` — one library report object per
/// package, each carrying that package's alias states, slot rows, index
/// findings and ignored tags.
#[derive(Serialize)]
pub struct PackageCascadeCheck {
    pub reports: Vec<CascadeReport>,
    /// Packages a configured index source claims but has no root document for
    /// yet - never announced, so the staleness layer had nothing to compare
    /// against and produced no findings. Plain-mode only: an empty
    /// `index_findings` means "agrees" for every other package, and a reader
    /// deserves to be told which of the two silences this is.
    ///
    /// Not serialized. The JSON key set is what a `--format json` consumer
    /// parses and stays pinned; this is a note, not a finding.
    #[serde(skip)]
    pub index_layer_skipped: Vec<oci::Identifier>,
}

impl PackageCascadeCheck {
    pub fn new(reports: Vec<CascadeReport>) -> Self {
        Self {
            reports,
            index_layer_skipped: Vec::new(),
        }
    }

    /// The plain table's cells, row-major, before styling.
    ///
    /// Split out from [`Printable::print_plain`] so the row set is assertable
    /// without a terminal: what belongs in the table (every slot, plus the
    /// index and unrepairable findings folded in as their own rows) is the part
    /// worth pinning, and styling is not.
    fn table_rows(&self) -> Vec<[String; 5]> {
        let mut rows = Vec::new();
        for report in &self.reports {
            let package = report.logical.as_ref().unwrap_or(&report.identifier).to_string();
            for row in &report.rows {
                rows.push([
                    package.clone(),
                    row.tag.to_string(),
                    oci::render_native_platform(&row.platform),
                    slot_status_label(row.status).to_string(),
                    digest_transition(row.observed.as_deref(), row.expected.as_deref()),
                ]);
            }
            // The index layer and the unrepairable set are findings about the
            // same graph, so they are rows of the same table rather than
            // tables of their own; they carry no platform, which is what the
            // empty cell says.
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
                        (tag, format!("child manifest gone: {}", short_digest(digest)))
                    }
                    // Not "gone" - nothing was observed to be missing. The
                    // algorithm is one this build cannot address, so whether
                    // the child is still there could not be checked at all.
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
    fn print_plain(&self, data: &ocx_lib::cli::DataInterface) {
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
        // The Package column repeats one constant for the ordinary
        // single-package run, and a column whose every value is the same
        // string is noise the plain table pays width for.
        let headers: [ocx_lib::cli::Column; 5] = [
            "Package".into(),
            "Tag".into(),
            "Platform".into(),
            "Status".into(),
            "Detail".into(),
        ];
        let first = usize::from(self.reports.len() < 2);
        data.print_table(&headers[first..], &columns[first..]);

        // `check` never writes, so index staleness it found is always the
        // announce hop's to fix - the repair-side `--tags-file` form has
        // no file to name here. The local copy is a second hop after that:
        // announcing publishes the index, it does not sync this machine's.
        for report in &self.reports {
            if report.index_findings.is_empty() {
                continue;
            }
            let package = report.logical.as_ref().unwrap_or(&report.identifier).without_digest();
            data.print_hint(&format!(
                "index behind the registry - run: ocx package announce --package {package} --refresh"
            ));
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

/// The wire spelling of a slot status, matching its `Serialize` form so a
/// reader sees the same word in both output modes.
fn slot_status_label(status: SlotStatus) -> &'static str {
    match status {
        SlotStatus::Ok => "ok",
        SlotStatus::Missing => "missing",
        SlotStatus::Stale => "stale",
        SlotStatus::Orphan => "orphan",
        SlotStatus::Duplicate => "duplicate",
    }
}

/// `observed -> expected` in short digests, collapsing to whichever side exists
/// when the slot is missing on one of them.
fn digest_transition(observed: Option<&str>, expected: Option<&str>) -> String {
    match (observed, expected) {
        (Some(observed), Some(expected)) if observed == expected => short_digest(observed),
        (Some(observed), Some(expected)) => format!("{} -> {}", short_digest(observed), short_digest(expected)),
        (Some(only), None) | (None, Some(only)) => short_digest(only),
        (None, None) => String::new(),
    }
}

/// A wire digest string in its canonical short form. Falls back to the verbatim
/// string when it does not parse - an index may legitimately name an algorithm
/// this build does not implement, and the report still has to show it.
fn short_digest(digest: &str) -> String {
    oci::Digest::try_from(digest).map_or_else(|_| digest.to_string(), |parsed| parsed.to_short_string())
}

#[cfg(test)]
mod tests {
    use ocx_lib::package::cascade::graph::{AliasTag, SlotRow};

    use super::*;

    fn report_with(rows: Vec<SlotRow>, index_findings: Vec<IndexFinding>) -> CascadeReport {
        CascadeReport {
            identifier: oci::Identifier::parse("registry.test/acme/cmake").unwrap(),
            logical: None,
            aliases: Default::default(),
            rows,
            index_findings,
            ignored_tags: Vec::new(),
            unrepairable: Vec::new(),
        }
    }

    fn version(text: &str) -> ocx_lib::package::version::Version {
        ocx_lib::package::version::Version::parse(text).unwrap()
    }

    fn slot_row(tag: &str, status: SlotStatus, observed: Option<&str>, expected: Option<&str>) -> SlotRow {
        SlotRow {
            tag: AliasTag::Version(version(tag)),
            platform: oci::native::Platform {
                os: oci::native::Os::Linux,
                architecture: oci::native::Arch::Amd64,
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
                digest: OBSERVED.to_string(),
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
        row.platform = ocx_lib::oci::native::Platform {
            os: ocx_lib::oci::native::Os::Linux,
            architecture: ocx_lib::oci::native::Arch::ARM64,
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
    fn json_top_level_is_a_reports_wrapper() {
        let check = PackageCascadeCheck::new(vec![report_with(Vec::new(), Vec::new())]);
        let value = serde_json::to_value(&check).unwrap();
        let mut keys: Vec<&str> = value
            .as_object()
            .expect("top-level JSON is an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["reports"], "the whole JSON contract is this one wrapper key");
    }

    #[test]
    fn the_skipped_index_layer_note_never_reaches_the_json() {
        let mut check = PackageCascadeCheck::new(vec![report_with(Vec::new(), Vec::new())]);
        check.index_layer_skipped = vec![oci::Identifier::parse("registry.test/acme/cmake").unwrap()];

        let value = serde_json::to_value(&check).unwrap();
        let keys: Vec<&str> = value
            .as_object()
            .expect("top-level JSON is an object")
            .keys()
            .map(String::as_str)
            .collect();

        assert_eq!(
            keys,
            vec!["reports"],
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
        let mut keys: Vec<&str> = value["reports"][0]
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
                "logical",
                "rows",
                "unrepairable",
            ],
            "pin the field set a --format json consumer actually parses: {value}"
        );
    }
}
