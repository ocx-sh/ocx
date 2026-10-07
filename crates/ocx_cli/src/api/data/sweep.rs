// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Report type for a `--tags` / `--tags-file` index sweep: each swept tag carries its
//! per-reference report (sign or attest) verbatim, so one parser reads both shapes.

use ocx_console::Cell;
use ocx_util::wire_words;
use serde::Serialize;

use crate::api::Printable;
use crate::api::data::sanitize_for_terminal;

wire_words! {
    /// What the sweep did to one tag; one vocabulary for both verbs.
    // `completed`, never `signed`: `attest` can attach an unsigned statement, which `signed` would contradict.
    #[derive(Serialize, schemars::JsonSchema, Debug, Clone, Copy, PartialEq, Eq)]
    pub enum SweptStatus {
        /// The tag's index was acted on; `report` carries the outcome.
        Completed = "completed",
        /// The tag resolved to a bare manifest, so the sweep left it alone.
        ///
        /// Not a failure, and it does not make the run exit non-zero: `push`
        /// already signed each platform manifest inline, and a tag list mixing
        /// single-platform and multi-platform packages is the normal case for a
        /// repository publishing both.
        Skipped = "skipped",
        /// The tag names the index another tag in this same sweep already acted on,
        /// so one referrer covers both and nothing was written for this tag.
        ///
        /// Not a failure, and it does not make the run exit non-zero: the tag *is*
        /// signed (or attested), by the referrer the covering tag's row reports.
        /// `message` names that tag.
        // Acting once per tag would publish N identical referrers: a referrer is filed against the digest, not a tag.
        Covered = "covered",
        /// This tag failed. The sweep carried on to the rest and the run exits
        /// non-zero at the end.
        Failed = "failed",
    }
}

/// One swept tag's row.
// Flat, not an internally-tagged enum: `flatten` inside a tagged enum inside a `flatten` changes shape silently.
#[derive(Serialize, schemars::JsonSchema)]
#[schemars(rename = "{R}SweptTag")]
pub struct SweptTagReport<R> {
    /// The tag as the caller spelled it, so the report names what was asked
    /// for rather than what it resolved to.
    pub tag: String,
    /// What the sweep did to this tag.
    pub status: SweptStatus,
    /// The per-reference report, verbatim. Present for every tag whose run
    /// produced one, which includes a `failed` row carrying a partial report:
    /// a `--signature-format both` tag where one leg landed and one did not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<R>,
    /// The error's slug, else its category: the `error.detail`, else the
    /// `error.kind`, its error document would carry. Present exactly when
    /// `status` is `failed`.
    // Lifted out of that document, the same rule `push --sbom`'s failed attestation follows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Human-readable cause, sanitized for the terminal (CWE-150). Present
    /// exactly when `status` is `failed` or `covered` — for a `covered` row it
    /// names the tag whose run wrote the referrer, not a failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl<R> SweptTagReport<R> {
    /// A tag the sweep acted on.
    pub fn completed(tag: String, report: R) -> Self {
        Self {
            tag,
            status: SweptStatus::Completed,
            report: Some(report),
            kind: None,
            message: None,
        }
    }

    /// A tag whose index another tag in the same sweep, `signed_as`, already acted on.
    pub fn covered(tag: String, signed_as: String) -> Self {
        Self {
            tag,
            status: SweptStatus::Covered,
            report: None,
            kind: None,
            message: Some(sanitize_for_terminal(&format!("same index as tag '{signed_as}'"))),
        }
    }

    /// A tag that resolved to a bare manifest.
    pub fn skipped(tag: String) -> Self {
        Self {
            tag,
            status: SweptStatus::Skipped,
            report: None,
            kind: None,
            message: None,
        }
    }

    /// A tag that failed, described the way the error envelope would describe it.
    ///
    /// `report` is `Some` for a run that produced one and still failed (a `both` tag that lost a leg).
    pub fn failed(tag: String, report: Option<R>, kind: String, message: String) -> Self {
        Self {
            tag,
            status: SweptStatus::Failed,
            report,
            kind: Some(kind),
            message: Some(sanitize_for_terminal(&message)),
        }
    }

    /// The plain table's third column for this row.
    fn detail(&self) -> String {
        match (&self.status, &self.message) {
            (SweptStatus::Failed | SweptStatus::Covered, Some(message)) => message.clone(),
            (SweptStatus::Skipped, _) => "resolves to a single manifest; push already signed it".to_string(),
            _ => String::new(),
        }
    }
}

/// What a `--tags` / `--tags-file` sweep did, one row per swept tag.
///
/// Plain format: a `Tag | Status | Detail` table, one row per tag in sweep
/// order. A partially-failed sweep still names every tag that succeeded and
/// exits non-zero; the exit code is the process's, never a report field.
#[derive(Serialize, schemars::JsonSchema)]
#[schemars(rename = "{R}Sweep")]
pub struct SweepReport<R> {
    /// One row per swept tag, in the order the tags were given.
    pub items: Vec<SweptTagReport<R>>,
}

impl<R> SweepReport<R> {
    pub fn new(items: Vec<SweptTagReport<R>>) -> Self {
        Self { items }
    }
}

/// A per-reference report a sweep aggregates; each instantiation of [`SweepReport`] is its own root.
///
/// `SweepReport<Self>` carries `Self::SCHEMA_VERSION`: the per-reference report and its sweep are one shape.
pub trait SweptReport: Printable {
    /// The `cli.json` root name of `SweepReport<Self>`.
    const SWEEP_ROOT: &'static str;
}

impl<R: SweptReport> Printable for SweepReport<R> {
    const SCHEMA_VERSION: u32 = R::SCHEMA_VERSION;
    const ROOT: &'static str = R::SWEEP_ROOT;

    fn print_plain(&self, data: &ocx_console::DataInterface) {
        let mut rows: [Vec<Cell>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        for entry in &self.items {
            rows[0].push(Cell::from(sanitize_for_terminal(&entry.tag)));
            rows[1].push(Cell::from(entry.status.as_str().to_string()));
            rows[2].push(Cell::from(entry.detail()));
        }
        data.print_table(&["Tag".into(), "Status".into(), "Detail".into()], &rows);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in for the per-reference report, so these tests measure the
    /// aggregation rather than `SignatureReport`'s own contract.
    #[derive(Serialize)]
    struct Inner {
        subject_digest: &'static str,
    }

    fn render(report: &SweepReport<Inner>) -> serde_json::Value {
        serde_json::to_value(report).expect("a sweep report serializes")
    }

    /// The aggregation contract: each row carries the per-reference report
    /// **verbatim**, one level down, so a consumer of a single-reference run
    /// parses a swept one with the same code.
    #[test]
    fn a_completed_row_carries_the_per_reference_report_verbatim() {
        let report = SweepReport::new(vec![SweptTagReport::completed(
            "3.28".to_string(),
            Inner {
                subject_digest: "sha256:aa",
            },
        )]);
        let document = render(&report);

        let row = &document["items"][0];
        assert_eq!(row["tag"], "3.28");
        assert_eq!(row["status"], "completed");
        assert_eq!(row["report"]["subject_digest"], "sha256:aa");
        assert!(row.get("kind").is_none(), "a completed row carries no failure slug");
        assert!(row.get("message").is_none(), "a completed row carries no message");
    }

    /// The root is the report itself: no envelope keys around it.
    #[test]
    fn the_document_is_the_report_with_no_envelope() {
        let report: SweepReport<Inner> = SweepReport::new(vec![SweptTagReport::skipped("9.9.9".to_string())]);
        let document = render(&report);
        let keys: Vec<&str> = document
            .as_object()
            .expect("an object root")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, ["items"]);
    }

    /// A skipped tag is a row, not an omission: the operator asked about it,
    /// and "not in the output" is indistinguishable from "the sweep never got
    /// there".
    #[test]
    fn a_skipped_row_names_the_tag_and_carries_no_report() {
        let report: SweepReport<Inner> = SweepReport::new(vec![SweptTagReport::skipped("9.9.9".to_string())]);
        let row = render(&report)["items"][0].clone();
        assert_eq!(row["status"], "skipped");
        assert_eq!(row["tag"], "9.9.9");
        assert!(row.get("report").is_none());
    }

    /// The document still names every tag after a failure — the whole point
    /// of not aborting at the first one.
    #[test]
    fn a_partially_failed_sweep_reports_every_row() {
        let report = SweepReport::new(vec![
            SweptTagReport::completed(
                "3.28".to_string(),
                Inner {
                    subject_digest: "sha256:aa",
                },
            ),
            SweptTagReport::failed(
                "3.29".to_string(),
                None,
                "not_found".to_string(),
                "no such tag".to_string(),
            ),
            SweptTagReport::completed(
                "latest".to_string(),
                Inner {
                    subject_digest: "sha256:bb",
                },
            ),
        ]);
        let document = render(&report);

        let rows = document["items"].as_array().expect("an array of rows");
        assert_eq!(rows.len(), 3, "every swept tag is a row, failure included");
        assert_eq!(rows[1]["status"], "failed");
        assert_eq!(rows[1]["kind"], "not_found");
        assert_eq!(rows[1]["message"], "no such tag");
        assert_eq!(
            rows[2]["report"]["subject_digest"], "sha256:bb",
            "the tag after the failure must still be reported",
        );
    }

    /// A run that failed one leg and landed the other is `failed` **and**
    /// carries its partial report: hiding the leg that landed would leave the
    /// operator re-signing what is already published.
    #[test]
    fn a_failed_row_may_still_carry_the_partial_report() {
        let report = SweepReport::new(vec![SweptTagReport::failed(
            "3.28".to_string(),
            Some(Inner {
                subject_digest: "sha256:aa",
            }),
            "internal".to_string(),
            "one leg did not land".to_string(),
        )]);
        let row = render(&report)["items"][0].clone();
        assert_eq!(row["status"], "failed");
        assert_eq!(row["report"]["subject_digest"], "sha256:aa");
    }

    /// A registry-sourced message reaches a terminal; control characters in it
    /// are neutralized (CWE-150) before they can be rendered.
    #[test]
    fn a_failure_message_is_neutralized_for_the_terminal() {
        let report: SweepReport<Inner> = SweepReport::new(vec![SweptTagReport::failed(
            "3.28".to_string(),
            None,
            "internal".to_string(),
            "before\u{1b}[31mafter".to_string(),
        )]);
        let row = render(&report)["items"][0].clone();
        let message = row["message"].as_str().expect("a message");
        assert!(
            !message.contains('\u{1b}'),
            "the escape must not survive into the report: {message:?}",
        );
    }

    /// The plain table renders one row per tag, in sweep order, with the same
    /// status vocabulary the JSON carries.
    #[test]
    fn the_plain_table_prints_one_row_per_tag_in_sweep_order() {
        let report: SweepReport<Inner> = SweepReport::new(vec![
            SweptTagReport::completed("3.28".to_string(), Inner { subject_digest: "x" }),
            SweptTagReport::skipped("9.9.9".to_string()),
            SweptTagReport::failed(
                "3.29".to_string(),
                None,
                "not_found".to_string(),
                "no such tag".to_string(),
            ),
        ]);
        let statuses: Vec<&str> = report.items.iter().map(|row| row.status.as_str()).collect();
        assert_eq!(statuses, ["completed", "skipped", "failed"]);

        let details: Vec<String> = report.items.iter().map(SweptTagReport::detail).collect();
        assert_eq!(details[0], "", "a completed row's detail is the report, not the table");
        assert!(
            details[1].contains("push already signed it"),
            "a skipped row must say why it was skipped: {:?}",
            details[1],
        );
        assert_eq!(details[2], "no such tag");
    }
}
