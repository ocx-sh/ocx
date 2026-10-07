// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Report data for `ocx package cascade repair`.

use std::path::PathBuf;

use ocx_console::Cell;
use ocx_package::cascade::apply::{RepairOutcome, WriteOutcome};
use ocx_package::cascade::graph::{CascadeReport, PlannedWrite, Unrepairable};
use serde::Serialize;

use super::package_cascade_check::stale_index_hint;
use crate::api::Printable;

/// One package's repair run: what was wrong, what the run planned, and what
/// the registry accepted.
///
/// `planned` and `outcomes` are separate: a preview has the first and not the
/// second, and a real run's outcomes can disagree with its plan (an alias
/// refused at preflight, a write the registry rejected).
// Never collapse the two lists: a refusal would become indistinguishable from a plan that
// never covered the alias.
#[derive(Serialize, schemars::JsonSchema)]
pub struct RepairEntry {
    /// The package's findings, the same report `cascade check` prints.
    pub report: CascadeReport,
    /// The alias writes this run planned.
    pub planned: Vec<PlannedWrite>,
    /// Empty for a preview run: nothing was attempted.
    pub outcomes: Vec<RepairOutcome>,
    /// The alias tags this run left present in the registry - the same lines
    /// written to `--tags-file`, echoed here so a JSON consumer gets them
    /// without reading the file back.
    pub tags: Vec<String>,
}

/// What `cascade repair` did, one entry per package in input order.
///
/// `dry_run` and `tags_file` are run-wide.
#[derive(Serialize, schemars::JsonSchema)]
pub struct PackageCascadeRepair {
    /// One entry per package: the finding report, the planned writes, their outcomes and the `tags` this run
    /// left present.
    pub items: Vec<RepairEntry>,
    /// True when nothing was written because the run was a preview.
    pub dry_run: bool,
    /// Where `--tags-file` was written; absent when the flag was not passed.
    // The follow-up hint can only name the file the user actually asked for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags_file: Option<PathBuf>,
    /// Packages an index source claims but has no root for yet, so the index layer compared nothing;
    /// plain only, to tell that silence from "agrees". Not serialized: the JSON key set is pinned.
    #[serde(skip)]
    pub index_layer_skipped: Vec<ocx_oci::PackageRef>,
}

impl PackageCascadeRepair {
    /// A run in which no package needed a write: a clean run, or a preview with nothing to repair.
    pub fn from_reports(reports: Vec<CascadeReport>, dry_run: bool) -> Self {
        let items = reports
            .into_iter()
            .map(|report| RepairEntry {
                planned: Vec::new(),
                outcomes: Vec::new(),
                tags: Vec::new(),
                report,
            })
            .collect();
        Self {
            items,
            dry_run,
            tags_file: None,
            index_layer_skipped: Vec::new(),
        }
    }

    /// The plain table's cells, row-major: outcomes, or the plan for a preview, never mixed for one
    /// package, or "refused" reads like "never planned".
    fn table_rows(&self) -> Vec<[String; 4]> {
        let mut rows = Vec::new();
        for entry in &self.items {
            let report = &entry.report;
            let package = report
                .logical
                .as_ref()
                .map_or_else(|| report.identifier.to_string(), ToString::to_string);
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
                    // Still landed: a disagreeing read-back means a concurrent publisher, not a failure.
                    WriteOutcome::Written {
                        digest,
                        verified,
                        dropped,
                    } => (
                        if *verified { "written" } else { "written-unverified" },
                        written_detail(digest, dropped),
                    ),
                    WriteOutcome::Refused { reason } => ("refused", refusal_detail(reason)),
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
            && self.items.iter().any(|entry| {
                entry
                    .outcomes
                    .iter()
                    .any(|outcome| matches!(outcome.outcome, WriteOutcome::Written { .. }))
            })
    }

    /// The hops left after this run, in order: a run that wrote hands `announce` a tag list, one that
    /// only found staleness does not, and a preview gets neither.
    fn print_follow_up_hints(&self, data: &ocx_console::DataInterface) {
        let wrote = self.wrote_anything();
        for entry in &self.items {
            let report = &entry.report;
            let stale_index = !report.index_findings.is_empty();
            if !wrote && !stale_index {
                continue;
            }
            let package = report.logical.as_ref().map_or_else(
                || report.identifier.without_digest().to_string(),
                |logical| logical.without_digest().to_string(),
            );
            let package = package.to_string();
            match (wrote, &self.tags_file) {
                (true, Some(path)) => data.print_hint(&publish_moved_tags_hint(&package, path)),
                (true, None) => data.print_hint(&publish_moved_tags_without_file_hint(&package)),
                (false, _) => data.print_hint(&stale_index_hint(&package)),
            }
            // Announcing publishes the index; the local copy needs a sync after it.
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
    const SCHEMA_VERSION: u32 = 1;
    const ROOT: &'static str = "PackageCascadeRepair";

    fn print_plain(&self, data: &ocx_console::DataInterface) {
        let theme = data.theme();
        let mut columns: [Vec<Cell>; 4] = Default::default();
        for row in self.table_rows() {
            let [package, tag, status, detail] = row;
            columns[0].push(Cell::from(package));
            columns[1].push(Cell::from(theme.tag(&tag)));
            columns[2].push(Cell::from(status));
            columns[3].push(Cell::from(detail));
        }
        // The Package column is dropped for a single-package run, where it would repeat one value.
        let headers: [ocx_console::Column; 4] = ["Package".into(), "Tag".into(), "Status".into(), "Detail".into()];
        let first = usize::from(self.items.len() < 2);
        data.print_table(&headers[first..], &columns[first..]);

        if self.dry_run {
            data.print_hint("preview only - nothing was written to the registry");
        }

        self.print_follow_up_hints(data);
    }
}

/// The remediation line a run that moved tags prints, naming the file `--tags-file` wrote; a pure
/// builder so tests can assert it, since `print_hint` writes real stdout.
#[must_use]
fn publish_moved_tags_hint(package: &str, tags_path: &std::path::Path) -> String {
    format!(
        "publish the moved tags - run: ocx package announce {package} --tags-file {}",
        tags_path.display()
    )
}

/// The remediation line for moved tags without a `--tags-file`, so the follow-up needs two commands.
#[must_use]
fn publish_moved_tags_without_file_hint(package: &str) -> String {
    format!(
        "publish the moved tags - re-run with --tags-file <PATH>, then: \
         ocx package announce {package} --tags-file <PATH>"
    )
}

/// A landed write's detail cell; a write that dropped orphan entries says so, with a count.
fn written_detail(digest: &ocx_oci::Digest, dropped: &[String]) -> String {
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

/// A raced alias's detail cell: expected against live, `-` for a side where the tag did not exist.
fn raced_detail(expected: Option<&ocx_oci::Digest>, live: Option<&ocx_oci::Digest>) -> String {
    let render =
        |digest: Option<&ocx_oci::Digest>| digest.map_or_else(|| "-".to_string(), ocx_oci::Digest::to_short_string);
    format!("{} -> {}", render(expected), render(live))
}

/// The one-line reason an alias was refused before any write.
fn refusal_detail(reason: &Unrepairable) -> String {
    match reason {
        Unrepairable::ChildManifestMissing { digest, .. } => format!("child manifest gone: {digest}"),
        // Not "gone": this build cannot address the algorithm, so presence was never checked.
        Unrepairable::ChildDigestUnaddressable { digest, .. } => {
            format!("unaddressable digest algorithm: {digest}")
        }
        Unrepairable::WouldEmptyIndex { .. } => "would empty the index".to_string(),
    }
}

#[cfg(test)]
mod tests {

    use ocx_package::cascade::graph::AliasTag;

    use super::*;

    fn report() -> CascadeReport {
        CascadeReport {
            identifier: ocx_oci::OciIdentifier::parse_target("registry.test/acme/cmake", ocx_oci::DEFAULT_REGISTRY)
                .unwrap(),
            logical: None,
            aliases: Default::default(),
            rows: Vec::new(),
            index_findings: Vec::new(),
            ignored_tags: Vec::new(),
            unrepairable: Vec::new(),
        }
    }

    fn digest() -> ocx_oci::Digest {
        ocx_oci::Digest::try_from("sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc").unwrap()
    }

    fn entry(outcomes: Vec<RepairOutcome>) -> RepairEntry {
        RepairEntry {
            report: report(),
            planned: Vec::new(),
            outcomes,
            tags: Vec::new(),
        }
    }

    fn version(text: &str) -> ocx_package::version::Version {
        ocx_package::version::Version::parse(text).unwrap()
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

    fn repair(items: Vec<RepairEntry>, dry_run: bool) -> PackageCascadeRepair {
        PackageCascadeRepair {
            items,
            dry_run,
            tags_file: None,
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
                    WriteOutcome::Refused {
                        reason: Unrepairable::WouldEmptyIndex {
                            tag: AliasTag::Version(version("2")),
                        },
                    },
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
        clean.items[0].planned = vec![PlannedWrite {
            tag: AliasTag::Version(version("3.28")),
            index: ocx_oci::ImageIndex {
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
            vec!["dry_run", "items"],
            "pin the field set a --format json consumer actually parses; no `--tags-file`, no key: {value}"
        );

        let mut entry_keys: Vec<&str> = value["items"][0]
            .as_object()
            .expect("each entry is an object")
            .keys()
            .map(String::as_str)
            .collect();
        entry_keys.sort_unstable();
        assert_eq!(
            entry_keys,
            vec!["outcomes", "planned", "report", "tags"],
            "one vocabulary per document: the entry's tag list is `tags`, and the run-wide \
             destination beside it is `tags_file` - never `announce_tags` next to either"
        );
    }

    #[test]
    fn json_outcomes_are_type_tagged_and_a_refusal_nests_its_reason() {
        let refused = WriteOutcome::Refused {
            reason: Unrepairable::WouldEmptyIndex {
                tag: AliasTag::Version(version("2")),
            },
        };
        assert_eq!(
            serde_json::to_value(&refused).unwrap(),
            serde_json::json!({"type": "refused", "reason": {"type": "would_empty_index", "tag": "2"}})
        );
        let raced = WriteOutcome::Raced {
            expected: None,
            live: Some(digest()),
        };
        assert_eq!(
            serde_json::to_value(&raced).unwrap(),
            serde_json::json!({"type": "raced", "live": digest().to_string()}),
            "the side that never held the tag is omitted, never null"
        );
    }

    #[test]
    fn the_skipped_index_layer_note_never_reaches_the_json() {
        let mut repair = PackageCascadeRepair::from_reports(vec![report()], false);
        repair.index_layer_skipped = vec![ocx_oci::PackageRef::parse("registry.test/acme/cmake").unwrap()];

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
    fn json_tags_file_is_null_when_the_flag_was_not_passed() {
        let repair = PackageCascadeRepair::from_reports(vec![report()], false);
        let value = serde_json::to_value(&repair).unwrap();
        assert!(value["tags_file"].is_null());
    }

    #[test]
    fn json_tags_file_is_a_string_when_the_flag_was_passed() {
        let mut repair = PackageCascadeRepair::from_reports(vec![report()], false);
        repair.tags_file = Some(PathBuf::from("/tmp/tags.txt"));
        let value = serde_json::to_value(&repair).unwrap();
        assert_eq!(value["tags_file"], "/tmp/tags.txt");
    }

    /// C-062 / DX-70: repair's own two remediation lines name the **positional**
    /// announce form.
    ///
    /// The `repair` half of `cascade_remediation_strings_use_the_positional_form`
    /// (whose `check` half lives beside `stale_index_hint`, the third builder,
    /// which repair calls rather than duplicates). Split across two test
    /// functions because both builders here are private to this module, and
    /// widening their visibility to co-locate one test would trade a real
    /// encapsulation for a cosmetic one.
    ///
    /// Each builder gets its own assertion (E-20): asserting one and trusting
    /// the other passes with a site un-migrated.
    ///
    /// Red at the stub: both builders are `unimplemented!()`.
    /// Mutation once implemented: write `announce --package {package}` back into
    /// either builder — the other stays green and exactly one row reds.
    #[test]
    fn cascade_repair_remediation_strings_use_the_positional_form() {
        let with_file = publish_moved_tags_hint("acme/widget", std::path::Path::new("tags.txt"));
        assert!(
            with_file.contains("ocx package announce acme/widget"),
            "the package is named positionally, flags after it: {with_file}"
        );
        assert!(
            !with_file.contains("--package"),
            "ocx must not tell an operator to run the spelling it deprecates: {with_file}"
        );
        assert!(
            with_file.contains("--tags-file tags.txt"),
            "the follow-up names the file --tags-file actually wrote: {with_file}"
        );

        let without_file = publish_moved_tags_without_file_hint("acme/widget");
        assert!(
            without_file.contains("ocx package announce acme/widget"),
            "the package is named positionally here too: {without_file}"
        );
        assert!(
            !without_file.contains("--package"),
            "ocx must not tell an operator to run the spelling it deprecates: {without_file}"
        );
        assert!(
            without_file.contains("re-run with --tags-file <PATH>"),
            "with no destination the follow-up is two commands, and this is the first: {without_file}"
        );
    }
}
