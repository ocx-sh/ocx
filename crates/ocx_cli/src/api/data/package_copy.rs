// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::fmt;

use ocx_lib::{
    cli::Cell,
    publisher::{CopyOutcome, Disposition},
};
use serde::Serialize;

use crate::api::Printable;
use crate::api::data::sanitize_for_terminal;

/// Whether this run copied or only planned.
///
/// Typed rather than a pre-formatted `String`, so `Serialize` emits a token a
/// script matches on while `Display` and [`CopyStatus::action`] stay free to
/// read like English (`subsystem-cli-api.md`, "Typed Enums Over Strings").
#[derive(Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CopyStatus {
    Copied,
    Planned,
}

impl CopyStatus {
    /// The cargo-style verb that opens the stderr status line.
    fn action(self) -> &'static str {
        match self {
            Self::Copied => "Copied",
            Self::Planned => "Planned",
        }
    }
}

impl fmt::Display for CopyStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Copied => "copied",
            Self::Planned => "planned",
        })
    }
}

/// What became of the repository description.
///
/// Reported rather than left to a stderr warning: `--format json` is how a CI
/// job finds out whether the catalog page travelled, and a warning is not a
/// field. `None` — the flag was not passed — serializes as `null`, so the key
/// is always there to branch on.
#[derive(Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum DescriptionOutcome {
    /// Pulled from the source and pushed to the target.
    Copied,
    /// The source publishes none, so there was nothing to copy. Not a failure:
    /// a description is repository-level prose and legitimately absent.
    Absent,
    /// `--dry-run` was in force, so the description was not read or written.
    SkippedDryRun,
}

impl fmt::Display for DescriptionOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Copied => "copied",
            Self::Absent => "none at the source",
            Self::SkippedDryRun => "skipped under --dry-run",
        })
    }
}

/// Result of `ocx package copy`.
///
/// Plain format: one table, one row per platform — what the target now offers
/// and how it got there — and a single stderr status line for the receipt
/// (tags written, blob traffic, description). The per-platform breakdown is the
/// point and the only thing on stdout: a promotion that moved one platform into
/// a target offering three is a legitimate outcome and a serious mistake, and
/// only the row list tells them apart.
///
/// **The `Digest` column means two things, and the `Result` column says which.**
/// On an `added`, `replaced` or `unchanged` row it is the digest this copy put
/// there. On a `kept (not in source)` row it is the digest the target already
/// had and this copy never touched.
///
/// Under `--dry-run` the rows read `would add` / `would replace` in plain
/// output only. The JSON `disposition` keeps its stable slug either way, and
/// the top-level `status` is what distinguishes a plan from a promotion.
///
/// JSON format:
/// `{ "source", "target", "status", "platforms": [{ "platform", "digest",
/// "disposition" }], "cascade_tags_written", "keep_tags_written",
/// "referrers_copied", "blobs": { "present", "mounted", "uploaded" },
/// "description" }`.
#[derive(Serialize)]
pub struct CopyReport {
    /// The source reference as given.
    pub source: String,
    /// The resolved target reference.
    pub target: String,
    /// `copied`, or `planned` under `--dry-run`.
    pub status: CopyStatus,
    /// One row per platform the target offers after this copy, including any it
    /// already had that the source does not ship.
    pub platforms: Vec<CopiedPlatformRow>,
    /// Rolling tags written in addition to the target's own tag.
    pub cascade_tags_written: Vec<String>,
    /// Digest-named `__ocx.keep.<algorithm>-<hex>` tags written, one per distinct
    /// manifest.
    pub keep_tags_written: Vec<String>,
    /// Referrer manifests carried over — signatures, SBOMs, attestations.
    pub referrers_copied: usize,
    pub blobs: BlobSummary,
    /// What became of the repository description, or `null` when
    /// `--description` was not passed.
    pub description: Option<DescriptionOutcome>,
}

/// What became of one platform.
#[derive(Serialize)]
pub struct CopiedPlatformRow {
    pub platform: String,
    /// The leaf digest the target serves for this platform. For a
    /// `kept-not-in-source` row this is the digest it already had.
    pub digest: String,
    /// Typed, so JSON carries `added` / `unchanged` / `replaced` /
    /// `kept-not-in-source` while the table renders the prose.
    pub disposition: Disposition,
}

/// Blob traffic, summed over every platform.
#[derive(Serialize)]
pub struct BlobSummary {
    /// Already at the target — nothing transferred.
    pub present: usize,
    /// Mounted from another repository in the same registry.
    pub mounted: usize,
    /// Downloaded from the source and uploaded to the target.
    pub uploaded: usize,
}

impl CopyReport {
    pub fn from_outcome(outcome: CopyOutcome, description: Option<DescriptionOutcome>) -> Self {
        Self {
            source: outcome.source.to_string(),
            target: outcome.target.to_string(),
            status: if outcome.dry_run {
                CopyStatus::Planned
            } else {
                CopyStatus::Copied
            },
            platforms: outcome
                .platforms
                .iter()
                .map(|row| CopiedPlatformRow {
                    platform: row.platform.to_string(),
                    digest: row.digest.to_string(),
                    disposition: row.disposition,
                })
                .collect(),
            cascade_tags_written: outcome.cascade_tags,
            keep_tags_written: outcome.keep_tags,
            referrers_copied: outcome.referrers,
            blobs: BlobSummary {
                present: outcome.blobs.present,
                mounted: outcome.blobs.mounted,
                uploaded: outcome.blobs.uploaded,
            },
            description,
        }
    }

    /// The cargo-style verb for the stderr status line.
    pub fn action(&self) -> &'static str {
        self.status.action()
    }

    /// The receipt: what this copy wrote and what it moved.
    ///
    /// stderr, not stdout — a receipt is a step-along-the-way diagnostic, and
    /// the single-table rule leaves stdout to the per-platform rows. Every
    /// interpolated value except the counts came off a wire document (the
    /// target's own tag list supplies the rolling tags), so the whole line is
    /// neutralized before it reaches a terminal.
    pub fn summary(&self) -> String {
        let cascade = if self.cascade_tags_written.is_empty() {
            "no cascade tags".to_string()
        } else {
            format!(
                "cascade tags {}",
                self.cascade_tags_written
                    .iter()
                    .map(|tag| sanitize_for_terminal(tag))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        let mut line = format!(
            "{}: {} platform(s), {cascade}, {} keep tag(s), {} referrer(s); \
             blobs {} present, {} mounted, {} uploaded",
            sanitize_for_terminal(&self.target),
            self.platforms.len(),
            self.keep_tags_written.len(),
            self.referrers_copied,
            self.blobs.present,
            self.blobs.mounted,
            self.blobs.uploaded,
        );
        if let Some(description) = self.description {
            line.push_str(&format!("; description {description}"));
        }
        line
    }

    /// The prose for one row's `Result` cell.
    ///
    /// A plan has not added or replaced anything, and `added` is indicative past
    /// tense — so under `Planned` the two write dispositions become `would add`
    /// and `would replace`. `unchanged` and `kept (not in source)` describe the
    /// target as it already is and read correctly either way.
    ///
    /// Plain output only. The JSON `disposition` slug never changes.
    fn result_cell(&self, disposition: Disposition) -> String {
        match (self.status, disposition) {
            (CopyStatus::Planned, Disposition::Added) => "would add".to_string(),
            (CopyStatus::Planned, Disposition::Replaced) => "would replace".to_string(),
            (_, other) => other.to_string(),
        }
    }

    /// The three plain columns, exactly as `print_table` will write them.
    ///
    /// Split out from [`Printable::print_plain`] so the neutralization and the
    /// dry-run vocabulary are assertable without a terminal — the same seam
    /// `api/data/index.rs` uses.
    fn plain_rows(&self) -> [Vec<String>; 3] {
        [
            self.platforms
                .iter()
                .map(|row| sanitize_for_terminal(&row.platform))
                .collect(),
            self.platforms
                .iter()
                .map(|row| sanitize_for_terminal(&row.digest))
                .collect(),
            self.platforms
                .iter()
                .map(|row| self.result_cell(row.disposition))
                .collect(),
        ]
    }
}

impl Printable for CopyReport {
    fn print_plain(&self, data: &ocx_lib::cli::DataInterface) {
        data.print_table(
            &["Platform".into(), "Digest".into(), "Result".into()],
            &self
                .plain_rows()
                .map(|column| column.into_iter().map(Cell::from).collect::<Vec<_>>()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::data::is_bidi_control;

    /// Every shape the CWE-150 finding measured: a raw ESC opening a CSI
    /// sequence, a newline, a NUL, and a right-to-left override.
    const HOSTILE: &str = "linux/\u{1b}[31mam\nd64\u{0}\u{202e}";

    fn row(platform: &str, disposition: Disposition) -> CopiedPlatformRow {
        CopiedPlatformRow {
            platform: platform.to_string(),
            digest: format!("sha256:{}", "a".repeat(64)),
            disposition,
        }
    }

    fn report(status: CopyStatus, platforms: Vec<CopiedPlatformRow>) -> CopyReport {
        CopyReport {
            source: "dev.example.com/acme/tool:1.4.2".to_string(),
            target: "prod.example.com/acme/tool:1.4.2".to_string(),
            status,
            platforms,
            cascade_tags_written: Vec::new(),
            keep_tags_written: Vec::new(),
            referrers_copied: 0,
            blobs: BlobSummary {
                present: 0,
                mounted: 0,
                uploaded: 0,
            },
            description: None,
        }
    }

    /// The frozen wire vocabulary. `test/tests/test_package_copy.py` matches on
    /// these exact strings, so a rename here is a contract break, not a tidy-up.
    #[test]
    fn the_serialized_vocabulary_is_the_one_scripts_match_on() {
        assert_eq!(serde_json::to_string(&CopyStatus::Copied).unwrap(), r#""copied""#);
        assert_eq!(serde_json::to_string(&CopyStatus::Planned).unwrap(), r#""planned""#);

        assert_eq!(serde_json::to_string(&Disposition::Added).unwrap(), r#""added""#);
        assert_eq!(
            serde_json::to_string(&Disposition::Unchanged).unwrap(),
            r#""unchanged""#
        );
        assert_eq!(serde_json::to_string(&Disposition::Replaced).unwrap(), r#""replaced""#);
        assert_eq!(
            serde_json::to_string(&Disposition::KeptNotInSource).unwrap(),
            r#""kept-not-in-source""#
        );

        assert_eq!(
            serde_json::to_string(&DescriptionOutcome::Copied).unwrap(),
            r#""copied""#
        );
        assert_eq!(
            serde_json::to_string(&DescriptionOutcome::Absent).unwrap(),
            r#""absent""#
        );
        assert_eq!(
            serde_json::to_string(&DescriptionOutcome::SkippedDryRun).unwrap(),
            r#""skipped-dry-run""#
        );
    }

    /// A plan has not added anything, so plain output says `would add`. The
    /// slug a consumer matches on must not move with it — that is the whole
    /// reason the row holds a typed `Disposition` rather than rendered prose.
    #[test]
    fn a_dry_run_says_would_in_prose_and_keeps_the_slug_in_json() {
        let planned = report(
            CopyStatus::Planned,
            vec![
                row("linux/amd64", Disposition::Added),
                row("linux/arm64", Disposition::Replaced),
                row("darwin/arm64", Disposition::Unchanged),
                row("windows/amd64", Disposition::KeptNotInSource),
            ],
        );
        assert_eq!(
            planned.plain_rows()[2],
            vec!["would add", "would replace", "unchanged", "kept (not in source)"],
            "a plan must not report writes in the past tense"
        );
        let json = serde_json::to_string(&planned).unwrap();
        assert!(json.contains(r#""disposition":"added""#), "{json}");
        assert!(json.contains(r#""disposition":"replaced""#), "{json}");
        assert!(json.contains(r#""status":"planned""#), "{json}");

        // Control: the identical rows under `Copied` render the plain
        // vocabulary unchanged, so the rewrite above is conditional on the
        // status and not on the disposition.
        let copied = report(
            CopyStatus::Copied,
            vec![
                row("linux/amd64", Disposition::Added),
                row("linux/arm64", Disposition::Replaced),
            ],
        );
        assert_eq!(copied.plain_rows()[2], vec!["added", "replaced"]);
    }

    /// Every platform string is read off a source index — a wire document — and
    /// every cascade tag off the target's own tag list. CWE-150 / SEC-34.
    #[test]
    fn registry_text_is_neutralized_before_it_reaches_the_terminal() {
        let mut hostile = report(CopyStatus::Copied, vec![row(HOSTILE, Disposition::Added)]);
        hostile.cascade_tags_written = vec![HOSTILE.to_string()];
        hostile.target = format!("prod.example.com/acme/{HOSTILE}");

        for column in hostile.plain_rows() {
            for cell in column {
                assert!(
                    !cell.chars().any(|c| c.is_control() || is_bidi_control(c)),
                    "a table cell reached the terminal with an active character: {cell:?}"
                );
            }
        }
        let summary = hostile.summary();
        assert!(
            !summary.chars().any(|c| c.is_control() || is_bidi_control(c)),
            "the status line reached the terminal with an active character: {summary:?}"
        );

        // Control: the same render leaves an ordinary platform and tag verbatim,
        // so the assertions above cannot pass by emptying the output.
        let mut ordinary = report(CopyStatus::Copied, vec![row("linux/amd64", Disposition::Added)]);
        ordinary.cascade_tags_written = vec!["1.4".to_string()];
        assert_eq!(ordinary.plain_rows()[0], vec!["linux/amd64"]);
        assert!(
            ordinary.summary().contains("cascade tags 1.4"),
            "{}",
            ordinary.summary()
        );
    }

    /// The receipt names what was written; stdout keeps the one table.
    #[test]
    fn the_summary_carries_the_receipt_the_second_table_used_to() {
        let mut full = report(
            CopyStatus::Copied,
            vec![
                row("linux/amd64", Disposition::Added),
                row("linux/arm64", Disposition::Added),
            ],
        );
        full.cascade_tags_written = vec!["1.4".to_string(), "latest".to_string()];
        full.keep_tags_written = vec!["__ocx.keep.sha256-aaaa".to_string()];
        full.referrers_copied = 3;
        full.blobs = BlobSummary {
            present: 5,
            mounted: 2,
            uploaded: 1,
        };
        full.description = Some(DescriptionOutcome::Copied);

        let summary = full.summary();
        for needle in [
            "prod.example.com/acme/tool:1.4.2",
            "2 platform(s)",
            "cascade tags 1.4, latest",
            "1 keep tag(s)",
            "3 referrer(s)",
            "5 present",
            "2 mounted",
            "1 uploaded",
            "description copied",
        ] {
            assert!(summary.contains(needle), "summary lost {needle:?}: {summary}");
        }
        assert_eq!(full.action(), "Copied");
        assert_eq!(report(CopyStatus::Planned, Vec::new()).action(), "Planned");
    }
}
