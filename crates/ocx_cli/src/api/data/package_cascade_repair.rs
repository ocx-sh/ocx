// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Report data for `ocx package cascade repair`.

use std::path::PathBuf;

use ocx_lib::cli::Cell;
use ocx_lib::package::cascade::apply::{RepairOutcome, WriteOutcome};
use ocx_lib::package::cascade::graph::{CascadeReport, PlannedWrite, Unrepairable};
use serde::Serialize;

use crate::api::Printable;

/// One package's repair run: what was wrong, what the run planned, and what
/// the registry accepted.
///
/// `planned` and `outcomes` are separate because a preview has the first and
/// not the second, and a real run's outcomes can disagree with its plan (an
/// alias refused at preflight, a write the registry rejected). Collapsing them
/// into one list would make a refusal indistinguishable from a plan that never
/// covered the alias.
#[derive(Serialize, schemars::JsonSchema)]
pub struct RepairEntry {
    pub report: CascadeReport,
    pub planned: Vec<PlannedWrite>,
    /// Empty for a preview run: nothing was attempted.
    pub outcomes: Vec<RepairOutcome>,
    /// The alias tags this package's run moved or created - the same lines
    /// written to `--announce-tags`, echoed here so a JSON consumer gets them
    /// without reading the file back.
    pub announce_tags: Vec<String>,
}

/// What `cascade repair` did, one entry per package in input order.
///
/// Plain format: one row per attempted alias write (or, for a preview, per
/// planned write), plus the follow-up announce command when this run moved
/// tags or index staleness remains.
///
/// JSON format: `{ "entries": [...], "dry_run", "announce_tags_path" }` — one
/// per-package entry carrying the finding report, the planned writes and
/// their outcomes, alongside the run-wide preview flag and the
/// `--announce-tags` destination (`null` when the flag was not passed).
#[derive(Serialize, schemars::JsonSchema)]
pub struct PackageCascadeRepair {
    pub entries: Vec<RepairEntry>,
    /// True when nothing was written because the run was a preview.
    pub dry_run: bool,
    /// Where `--announce-tags` was written, when the flag was passed. The
    /// follow-up hint can only name the file the user actually asked for.
    pub announce_tags_path: Option<PathBuf>,
    /// Packages a configured index source claims but has no root document for
    /// yet - never announced, so the staleness layer had nothing to compare
    /// against and produced no findings. Plain-mode only: an empty
    /// `index_findings` means "agrees" for every other package, and a reader
    /// deserves to be told which of the two silences this is.
    ///
    /// Not serialized. The JSON key set is what a `--format json` consumer
    /// parses and stays pinned; this is a note, not a finding.
    #[serde(skip)]
    pub index_layer_skipped: Vec<ocx_lib::oci::Identifier>,
}

impl PackageCascadeRepair {
    /// A run in which no package needed a write: every report carries its
    /// findings (possibly none) and empty write lists. The shape of both a
    /// clean run and a preview with nothing to repair.
    pub fn from_reports(reports: Vec<CascadeReport>, dry_run: bool) -> Self {
        let entries = reports
            .into_iter()
            .map(|report| RepairEntry {
                planned: Vec::new(),
                outcomes: Vec::new(),
                announce_tags: Vec::new(),
                report,
            })
            .collect();
        Self {
            entries,
            dry_run,
            announce_tags_path: None,
            index_layer_skipped: Vec::new(),
        }
    }

    /// The plain table's cells, row-major, before styling.
    ///
    /// A run that attempted writes reports its outcomes; a preview has none, so
    /// it reports the plan instead. The two never mix for one package - that is
    /// what keeps "refused" distinguishable from "never planned".
    fn table_rows(&self) -> Vec<[String; 4]> {
        let mut rows = Vec::new();
        for entry in &self.entries {
            let report = &entry.report;
            let package = report.logical.as_ref().unwrap_or(&report.identifier).to_string();
            if entry.outcomes.is_empty() {
                for planned in &entry.planned {
                    rows.push([
                        package.clone(),
                        planned.tag.to_string(),
                        "planned".to_string(),
                        format!("{} slots", planned.reasons.len()),
                    ]);
                }
                continue;
            }
            for outcome in &entry.outcomes {
                let (status, detail) = match &outcome.outcome {
                    // An unverified write still landed - the read-back
                    // disagreeing means a concurrent publisher, not a failure,
                    // so it stays a written row with the caveat in view.
                    WriteOutcome::Written {
                        digest,
                        verified,
                        dropped,
                    } => (
                        if *verified { "written" } else { "written-unverified" },
                        written_detail(digest, dropped),
                    ),
                    WriteOutcome::Refused(reason) => ("refused", refusal_detail(reason)),
                    // Nothing was written and nothing is broken: a publisher
                    // moved the tag between the gather and the write, so the
                    // plan describes a graph that no longer exists.
                    WriteOutcome::Raced { expected, live } => ("raced", raced_detail(expected.as_ref(), live.as_ref())),
                    WriteOutcome::Failed { message } => ("failed", message.clone()),
                };
                rows.push([package.clone(), outcome.tag.to_string(), status.to_string(), detail]);
            }
        }
        rows
    }

    /// True when this run put at least one alias write on the wire.
    fn wrote_anything(&self) -> bool {
        !self.dry_run
            && self.entries.iter().any(|entry| {
                entry
                    .outcomes
                    .iter()
                    .any(|outcome| matches!(outcome.outcome, WriteOutcome::Written { .. }))
            })
    }

    /// The hops left after this run, in the order they must be taken.
    ///
    /// Which publish hint applies is decided by whether this run wrote: a run
    /// that moved aliases has a tag list to hand `announce`, while a run that
    /// only *found* index staleness has none, so `announce` has to re-observe
    /// the committed tags itself. A preview gets neither - it moved nothing,
    /// so there is nothing yet to publish.
    fn print_follow_up_hints(&self, data: &ocx_lib::cli::DataInterface) {
        let wrote = self.wrote_anything();
        for entry in &self.entries {
            let report = &entry.report;
            let stale_index = !report.index_findings.is_empty();
            if !wrote && !stale_index {
                continue;
            }
            let package = report.logical.as_ref().unwrap_or(&report.identifier).without_digest();
            match (wrote, &self.announce_tags_path) {
                (true, Some(path)) => data.print_hint(&format!(
                    "publish the moved tags - run: ocx package announce --package {package} --tags-file {}",
                    path.display()
                )),
                (true, None) => data.print_hint(&format!(
                    "publish the moved tags - re-run with --announce-tags <PATH>, then: ocx package announce --package {package} --tags-file <PATH>"
                )),
                (false, _) => data.print_hint(&format!(
                    "index behind the registry - run: ocx package announce --package {package} --refresh"
                )),
            }
            // Announcing publishes the index; the local copy only learns of it
            // on the next sync, so a stale index is a two-hop follow-up.
            if stale_index {
                data.print_hint(&format!(
                    "then refresh the local copy - run: ocx index update {package}"
                ));
            }
        }
        for package in &self.index_layer_skipped {
            data.print_hint(&format!(
                "no index root for {} yet - the index staleness check did not run",
                package.without_digest()
            ));
        }
    }
}

impl Printable for PackageCascadeRepair {
    fn print_plain(&self, data: &ocx_lib::cli::DataInterface) {
        let theme = data.theme();
        let mut columns: [Vec<Cell>; 4] = Default::default();
        for row in self.table_rows() {
            let [package, tag, status, detail] = row;
            columns[0].push(Cell::from(package));
            columns[1].push(Cell::from(theme.tag(&tag)));
            columns[2].push(Cell::from(status));
            columns[3].push(Cell::from(detail));
        }
        // The Package column repeats one constant for the ordinary
        // single-package run, and a column whose every value is the same
        // string is noise the plain table pays width for.
        let headers: [ocx_lib::cli::Column; 4] = ["Package".into(), "Tag".into(), "Status".into(), "Detail".into()];
        let first = usize::from(self.entries.len() < 2);
        data.print_table(&headers[first..], &columns[first..]);

        if self.dry_run {
            data.print_hint("preview only - nothing was written to the registry");
        }

        self.print_follow_up_hints(data);
    }
}

/// A landed write's detail cell.
///
/// A write that dropped dead orphan entries did not put the planned bytes on
/// the wire, so it must not read like a clean one. The count is what the cell
/// carries - the digests themselves are unbounded and already in the JSON.
fn written_detail(digest: &ocx_lib::oci::Digest, dropped: &[String]) -> String {
    let landed = digest.to_short_string();
    if dropped.is_empty() {
        return landed;
    }
    format!(
        "{landed} (dropped {} dead orphan entr{})",
        dropped.len(),
        if dropped.len() == 1 { "y" } else { "ies" }
    )
}

/// A raced alias's detail cell: what the plan expected the tag to hold against
/// what it holds now. `-` for a side the tag did not exist on.
fn raced_detail(expected: Option<&ocx_lib::oci::Digest>, live: Option<&ocx_lib::oci::Digest>) -> String {
    let render = |digest: Option<&ocx_lib::oci::Digest>| {
        digest.map_or_else(|| "-".to_string(), ocx_lib::oci::Digest::to_short_string)
    };
    format!("{} -> {}", render(expected), render(live))
}

/// The one-line reason an alias was refused before any write.
fn refusal_detail(reason: &Unrepairable) -> String {
    match reason {
        Unrepairable::ChildManifestMissing { digest, .. } => format!("child manifest gone: {digest}"),
        // Not "gone" - nothing was observed to be missing. The algorithm is
        // one this build cannot address, so whether the child is still there
        // could not be checked at all.
        Unrepairable::ChildDigestUnaddressable { digest, .. } => {
            format!("unaddressable digest algorithm: {digest}")
        }
        Unrepairable::WouldEmptyIndex { .. } => "would empty the index".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use ocx_lib::oci;
    use ocx_lib::package::cascade::graph::AliasTag;

    use super::*;

    fn report() -> CascadeReport {
        CascadeReport {
            identifier: oci::Identifier::parse("registry.test/acme/cmake").unwrap(),
            logical: None,
            aliases: Default::default(),
            rows: Vec::new(),
            index_findings: Vec::new(),
            ignored_tags: Vec::new(),
            unrepairable: Vec::new(),
        }
    }

    fn digest() -> oci::Digest {
        oci::Digest::try_from("sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc").unwrap()
    }

    fn entry(outcomes: Vec<RepairOutcome>) -> RepairEntry {
        RepairEntry {
            report: report(),
            planned: Vec::new(),
            outcomes,
            announce_tags: Vec::new(),
        }
    }

    fn version(text: &str) -> ocx_lib::package::version::Version {
        ocx_lib::package::version::Version::parse(text).unwrap()
    }

    fn outcome(tag: &str, outcome: WriteOutcome) -> RepairOutcome {
        RepairOutcome {
            tag: AliasTag::Version(version(tag)),
            outcome,
        }
    }

    fn written(verified: bool, dropped: &[&str]) -> WriteOutcome {
        WriteOutcome::Written {
            digest: digest(),
            verified,
            dropped: dropped.iter().map(|digest| (*digest).to_string()).collect(),
        }
    }

    fn repair(entries: Vec<RepairEntry>, dry_run: bool) -> PackageCascadeRepair {
        PackageCascadeRepair {
            entries,
            dry_run,
            announce_tags_path: None,
            index_layer_skipped: Vec::new(),
        }
    }

    #[test]
    fn each_outcome_class_gets_its_own_status_word() {
        let repair = repair(
            vec![entry(vec![
                outcome("3.28", written(true, &[])),
                outcome("3", written(false, &[])),
                outcome(
                    "2",
                    WriteOutcome::Refused(Unrepairable::WouldEmptyIndex {
                        tag: AliasTag::Version(version("2")),
                    }),
                ),
                outcome(
                    "1",
                    WriteOutcome::Failed {
                        message: "registry said no".to_string(),
                    },
                ),
                outcome(
                    "0",
                    WriteOutcome::Raced {
                        expected: Some(digest()),
                        live: None,
                    },
                ),
            ])],
            false,
        );

        let rows = repair.table_rows();
        let statuses: Vec<&str> = rows.iter().map(|row| row[2].as_str()).collect();

        assert_eq!(
            statuses,
            ["written", "written-unverified", "refused", "failed", "raced"]
        );
        assert_eq!(rows[0][3], "sha256:cccccccccccc");
        assert_eq!(rows[3][3], "registry said no");
        assert_eq!(
            rows[4][3], "sha256:cccccccccccc -> -",
            "a raced row names both sides, including a tag that is now gone"
        );
        assert!(repair.wrote_anything(), "a landed write is a write");
    }

    #[test]
    fn a_write_that_dropped_dead_orphans_does_not_read_like_a_clean_one() {
        let dropped = repair(
            vec![entry(vec![outcome(
                "3.28",
                written(true, &["sha256:dead", "sha256:beef"]),
            )])],
            false,
        );
        let clean = repair(vec![entry(vec![outcome("3.28", written(true, &[]))])], false);

        assert_eq!(
            dropped.table_rows()[0][3],
            "sha256:cccccccccccc (dropped 2 dead orphan entries)"
        );
        assert_eq!(
            clean.table_rows()[0][3],
            "sha256:cccccccccccc",
            "the negative control: an ordinary write's cell carries the digest alone"
        );
    }

    #[test]
    fn a_raced_alias_is_not_a_write() {
        let repair = repair(
            vec![entry(vec![outcome(
                "3",
                WriteOutcome::Raced {
                    expected: None,
                    live: Some(digest()),
                },
            )])],
            false,
        );

        assert!(
            !repair.wrote_anything(),
            "a publisher moved the tag first, so this run put nothing on the wire"
        );
    }

    #[test]
    fn a_run_that_only_refused_did_not_write() {
        let repair = repair(
            vec![entry(vec![outcome(
                "3",
                WriteOutcome::Failed {
                    message: "nope".to_string(),
                },
            )])],
            false,
        );

        assert!(
            !repair.wrote_anything(),
            "only failures means nothing landed, so the follow-up is --refresh not --tags-file"
        );
    }

    #[test]
    fn a_preview_reports_its_plan_instead_of_outcomes() {
        let mut clean = PackageCascadeRepair::from_reports(vec![report()], true);
        clean.entries[0].planned = vec![PlannedWrite {
            tag: AliasTag::Version(version("3.28")),
            index: oci::ImageIndex {
                schema_version: 2,
                media_type: None,
                manifests: Vec::new(),
                artifact_type: None,
                annotations: None,
            },
            observed_digest: None,
            referenced_digests: Vec::new(),
            reasons: Vec::new(),
        }];

        let rows = clean.table_rows();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][2], "planned");
        assert_eq!(rows[0][3], "0 slots");
        assert!(!clean.wrote_anything(), "a preview never writes");
    }

    // ── JSON key stability ───────────────────────────────────────────────

    #[test]
    fn json_top_level_key_set_matches_the_documented_shape() {
        let repair = PackageCascadeRepair::from_reports(vec![report()], false);
        let value = serde_json::to_value(&repair).unwrap();
        let mut keys: Vec<&str> = value
            .as_object()
            .expect("top-level JSON is an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec!["announce_tags_path", "dry_run", "entries"],
            "pin the field set a --format json consumer actually parses: {value}"
        );

        let mut entry_keys: Vec<&str> = value["entries"][0]
            .as_object()
            .expect("each entry is an object")
            .keys()
            .map(String::as_str)
            .collect();
        entry_keys.sort_unstable();
        assert_eq!(entry_keys, vec!["announce_tags", "outcomes", "planned", "report"]);
    }

    #[test]
    fn the_skipped_index_layer_note_never_reaches_the_json() {
        let mut repair = PackageCascadeRepair::from_reports(vec![report()], false);
        repair.index_layer_skipped = vec![report().identifier];

        let value = serde_json::to_value(&repair).unwrap();
        let keys: Vec<&str> = value
            .as_object()
            .expect("top-level JSON is an object")
            .keys()
            .map(String::as_str)
            .collect();

        assert!(
            !keys.contains(&"index_layer_skipped"),
            "a plain-mode note must not grow the pinned key set: {value}"
        );
    }

    #[test]
    fn json_announce_tags_path_is_null_when_the_flag_was_not_passed() {
        let repair = PackageCascadeRepair::from_reports(vec![report()], false);
        let value = serde_json::to_value(&repair).unwrap();
        assert!(value["announce_tags_path"].is_null());
    }

    #[test]
    fn json_announce_tags_path_is_a_string_when_the_flag_was_passed() {
        let mut repair = PackageCascadeRepair::from_reports(vec![report()], false);
        repair.announce_tags_path = Some(PathBuf::from("/tmp/tags.txt"));
        let value = serde_json::to_value(&repair).unwrap();
        assert_eq!(value["announce_tags_path"], "/tmp/tags.txt");
    }
}
