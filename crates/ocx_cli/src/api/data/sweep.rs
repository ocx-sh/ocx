// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Report type for a `--tags` / `--tags-file` index sweep.
//!
//! Generic over the per-reference report — [`SignatureReport`] for
//! `ocx package sign`, [`AttestationReport`] for `ocx package attest` — because
//! the sweep adds nothing to what either already says about one reference. It
//! **aggregates** them: each swept tag carries that tag's own report verbatim,
//! so a consumer parsing a single-reference run parses a swept one with the
//! same code, one level down.
//!
//! [`SignatureReport`]: crate::api::data::signature::SignatureReport
//! [`AttestationReport`]: crate::api::data::attestation::AttestationReport

use ocx_lib::cli::{Cell, ExitCode};
use serde::Serialize;

use crate::api::Printable;
use crate::api::data::sanitize_for_terminal;

/// What the sweep did to one tag.
///
/// One vocabulary for both verbs. `completed` rather than `signed` on purpose:
/// `ocx package attest` can attach an unsigned statement when the run has no
/// signing material at all, and a status that claimed `signed` there would
/// contradict the very report it sits next to.
#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SweptStatus {
    /// The tag's index was acted on; `report` carries the outcome.
    Completed,
    /// The tag resolved to a bare manifest, so the sweep left it alone.
    ///
    /// Not a failure, and it does not make the run exit non-zero: `push`
    /// already signed each platform manifest inline, and a tag list mixing
    /// single-platform and multi-platform packages is the normal case for a
    /// repository publishing both.
    Skipped,
    /// The tag names the index another tag in this same sweep already acted on,
    /// so one referrer covers both and nothing was written for this tag.
    ///
    /// Not a failure, and it does not make the run exit non-zero: the tag *is*
    /// signed (or attested), by the referrer the covering tag's row reports.
    /// `message` names that tag. A cascade release points several tags at one
    /// index, and a referrer is filed against the subject digest, never against
    /// a tag — so acting once per tag would publish N identical referrers, and
    /// a second sweep N more.
    Covered,
    /// This tag failed. The sweep carried on to the rest and the run exits
    /// non-zero at the end.
    Failed,
}

impl SweptStatus {
    /// The word the plain table prints — the same word the JSON carries, so a
    /// reader meets one vocabulary in both renderings.
    fn label(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Skipped => "skipped",
            Self::Covered => "covered",
            Self::Failed => "failed",
        }
    }
}

/// One swept tag's row.
///
/// Deliberately flat rather than an internally-tagged enum: `report` is itself
/// a struct that would have to be `flatten`ed into the variant, and serde's
/// `flatten` inside a tagged enum inside a `flatten` is a shape that silently
/// changes as those attributes compose. A `status` field plus three optionals
/// is the same information with none of that.
#[derive(Serialize)]
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
    /// The JSON error envelope's per-variant slug for this tag's failure,
    /// falling back to its frozen category for errors outside the sign and
    /// verify taxonomies. Present exactly when `status` is `failed`.
    ///
    /// Lifted out of the envelope this error would have rendered on its own,
    /// so the value a script reads here is the value it reads there — the same
    /// rule `push --sbom`'s failed attestation follows.
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

    /// A tag whose index another tag in the same sweep already acted on.
    ///
    /// `signed_as` is that tag. Reported rather than dropped: a caller who
    /// passed five tags must still see what became of all five, and the row
    /// points at the one carrying the referrer.
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

    /// A tag that failed, described the way the error envelope would describe
    /// it.
    ///
    /// `report` is `Some` for a run that produced one and still failed — a
    /// `--signature-format both` tag that lost one leg. Hiding the leg that
    /// landed behind the leg that did not would leave the operator re-signing
    /// what is already published.
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
/// order. JSON format:
/// `{"schema_version":1,"command":"<command>","exit_code":<code>,
/// "data":{"tags":[{"tag","status","report"?,"kind"?,"message"?}]}}`.
///
/// The envelope's `exit_code` is the code the process returns, which for a
/// partially-failed sweep is non-zero while the document still names every tag
/// that succeeded — the whole point of not aborting at the first failure.
#[derive(Serialize)]
pub struct SweepReport<R> {
    /// One row per swept tag, in the order the tags were given.
    pub tags: Vec<SweptTagReport<R>>,
    /// The command name the JSON envelope carries. Not part of `data`.
    #[serde(skip)]
    command: &'static str,
    /// The code the process will exit with. Not part of `data` — it is
    /// envelope, not report.
    #[serde(skip)]
    exit_code: ExitCode,
}

impl<R> SweepReport<R> {
    /// Build the report for `command`'s sweep, exiting with `exit_code`.
    pub fn new(command: &'static str, tags: Vec<SweptTagReport<R>>, exit_code: ExitCode) -> Self {
        Self {
            tags,
            command,
            exit_code,
        }
    }
}

impl<R: Serialize> Printable for SweepReport<R> {
    fn print_plain(&self, data: &ocx_lib::cli::DataInterface) {
        // Column-major, like every other `print_table` caller: one `Vec<Cell>`
        // per column.
        let mut rows: [Vec<Cell>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        for entry in &self.tags {
            rows[0].push(Cell::from(sanitize_for_terminal(&entry.tag)));
            rows[1].push(Cell::from(entry.status.label().to_string()));
            rows[2].push(Cell::from(entry.detail()));
        }
        data.print_table(&["Tag".into(), "Status".into(), "Detail".into()], &rows);
    }

    fn print_json(&self, data: &ocx_lib::cli::DataInterface) -> anyhow::Result<()>
    where
        Self: Sized,
    {
        let json = crate::error_envelope::render_envelope_with_exit_code(self.command, self, self.exit_code)?;
        let parsed: serde_json::Value = serde_json::from_str(&json)?;
        Ok(data.print_json(&parsed)?)
    }
}

#[cfg(test)]
mod tests {
    //! The sweep envelope's shape, checked on the rendered document rather
    //! than on the struct: `DataInterface` writes straight to the process's
    //! stdout, so `print_json`'s bytes cannot be captured in-process.

    use super::*;

    /// A stand-in for the per-reference report, so these tests measure the
    /// aggregation rather than `SignatureReport`'s own contract.
    #[derive(Serialize)]
    struct Inner {
        subject_digest: &'static str,
    }

    fn render(report: &SweepReport<Inner>) -> serde_json::Value {
        let json = crate::error_envelope::render_envelope_with_exit_code(report.command, report, report.exit_code)
            .expect("render envelope");
        serde_json::from_str(&json).expect("valid json")
    }

    /// The aggregation contract: each row carries the per-reference report
    /// **verbatim**, one level down, so a consumer of a single-reference run
    /// parses a swept one with the same code.
    #[test]
    fn a_completed_row_carries_the_per_reference_report_verbatim() {
        let report = SweepReport::new(
            "package sign",
            vec![SweptTagReport::completed(
                "3.28".to_string(),
                Inner {
                    subject_digest: "sha256:aa",
                },
            )],
            ExitCode::Success,
        );
        let envelope = render(&report);

        assert_eq!(envelope["command"], "package sign");
        assert_eq!(envelope["exit_code"], 0);
        let row = &envelope["data"]["tags"][0];
        assert_eq!(row["tag"], "3.28");
        assert_eq!(row["status"], "completed");
        assert_eq!(row["report"]["subject_digest"], "sha256:aa");
        assert!(row["kind"].is_null(), "a completed row carries no failure slug");
        assert!(row["message"].is_null(), "a completed row carries no message");
    }

    /// A skipped tag is a row, not an omission: the operator asked about it,
    /// and "not in the output" is indistinguishable from "the sweep never got
    /// there".
    #[test]
    fn a_skipped_row_names_the_tag_and_carries_no_report() {
        let report: SweepReport<Inner> = SweepReport::new(
            "package sign",
            vec![SweptTagReport::skipped("9.9.9".to_string())],
            ExitCode::Success,
        );
        let row = render(&report)["data"]["tags"][0].clone();
        assert_eq!(row["status"], "skipped");
        assert_eq!(row["tag"], "9.9.9");
        assert!(row["report"].is_null());
    }

    /// The envelope's `exit_code` is the code the process returns, and the
    /// document still names every tag — the whole point of not aborting at the
    /// first failure.
    #[test]
    fn a_partially_failed_sweep_reports_every_row_under_a_non_zero_exit_code() {
        let report = SweepReport::new(
            "package sign",
            vec![
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
            ],
            ExitCode::NotFound,
        );
        let envelope = render(&report);

        assert_eq!(envelope["exit_code"], 79);
        let rows = envelope["data"]["tags"].as_array().expect("an array of rows");
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
        let report = SweepReport::new(
            "package sign",
            vec![SweptTagReport::failed(
                "3.28".to_string(),
                Some(Inner {
                    subject_digest: "sha256:aa",
                }),
                "internal".to_string(),
                "one leg did not land".to_string(),
            )],
            ExitCode::Failure,
        );
        let row = render(&report)["data"]["tags"][0].clone();
        assert_eq!(row["status"], "failed");
        assert_eq!(row["report"]["subject_digest"], "sha256:aa");
    }

    /// A registry-sourced message reaches a terminal; control characters in it
    /// are neutralized (CWE-150) before they can be rendered.
    #[test]
    fn a_failure_message_is_neutralized_for_the_terminal() {
        let report: SweepReport<Inner> = SweepReport::new(
            "package sign",
            vec![SweptTagReport::failed(
                "3.28".to_string(),
                None,
                "internal".to_string(),
                "before\u{1b}[31mafter".to_string(),
            )],
            ExitCode::Failure,
        );
        let row = render(&report)["data"]["tags"][0].clone();
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
        let report: SweepReport<Inner> = SweepReport::new(
            "package sign",
            vec![
                SweptTagReport::completed("3.28".to_string(), Inner { subject_digest: "x" }),
                SweptTagReport::skipped("9.9.9".to_string()),
                SweptTagReport::failed(
                    "3.29".to_string(),
                    None,
                    "not_found".to_string(),
                    "no such tag".to_string(),
                ),
            ],
            ExitCode::NotFound,
        );
        let statuses: Vec<&str> = report.tags.iter().map(|row| row.status.label()).collect();
        assert_eq!(statuses, ["completed", "skipped", "failed"]);

        let details: Vec<String> = report.tags.iter().map(SweptTagReport::detail).collect();
        assert_eq!(details[0], "", "a completed row's detail is the report, not the table");
        assert!(
            details[1].contains("push already signed it"),
            "a skipped row must say why it was skipped: {:?}",
            details[1],
        );
        assert_eq!(details[2], "no such tag");
    }
}
