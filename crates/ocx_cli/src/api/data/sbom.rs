// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Report type for `ocx package sbom` output.
//!
//! Named apart from the lib-side `SbomReport`, following the existing pair
//! convention (lib `SignReport` / CLI `SignatureReport`, lib `VerifyResult` /
//! CLI [`VerificationReport`](super::verification::VerificationReport)).
//!
//! Unlike `package verify`, this command reports a **list**: `sbom_one` is
//! collect-all because "which SBOMs does this artifact carry" does not have
//! one answer (`adr_sbom_attestations.md` D-e). Refusals travel beside the
//! matches — a scan that returns three attestations having refused two is the
//! observation worth acting on, and dropping the refusals makes it
//! indistinguishable from a clean three.
//!
//! # CWE-150
//!
//! Every plain-format value here is registry-sourced by construction: the
//! certificate SAN and issuer are read out of a Fulcio cert carried in a
//! bundle a registry served, `predicate_type` is read out of the signed
//! payload, `RefusedCandidate::referrer_digest` is the registry's own listing
//! string (never a parsed [`oci::Digest`]), and a refusal reason's Display
//! text embeds it. All of them route through
//! [`sanitize_for_terminal`](super::sanitize_for_terminal) in
//! [`SbomListingReport::plain_rows`], which is the single render boundary this
//! module has. `--format json` stays verbatim, matching the crate-wide
//! contract stated on [`sanitize_for_terminal`] — that is a machine channel.

use ocx_lib::cli::{Cell, Column};
use serde::Serialize;

use crate::api::Printable;
use crate::api::data::sanitize_for_terminal;

/// Plain-format refusal head, per PKG-26. `--format json` is never truncated.
const MAX_PLAIN_REFUSALS: usize = 20;

/// Every verified attestation a package carries, plus what was refused.
#[derive(Debug, Serialize)]
pub struct SbomListingReport {
    /// One-glance counts, so a consumer branches on a field instead of
    /// measuring an array (PKG-25).
    pub summary: ListingSummary,
    /// One entry per verified attestation, in listing order.
    pub entries: Vec<SbomEntry>,
    /// Every candidate examined and refused, in listing order. Never
    /// truncated in JSON.
    pub refused: Vec<RefusedEntry>,
}

/// The counts and the status a script branches on.
#[derive(Debug, Serialize)]
pub struct ListingSummary {
    /// `success` when nothing was refused, `partial_failure` otherwise.
    pub status: &'static str,
    /// Mirrors the process exit code. Always 0 here, as a posture rather than
    /// as an unreachability claim: a refusal beside a listing is a partial
    /// failure the caller is told about and still exits 0 for, and under
    /// `--summary` that includes a listing whose `entries` ended up empty
    /// because every document refused. Only the library's own zero-match scan
    /// exits non-zero — `AttestationNotFound` (79) — and it never reaches a
    /// report at all.
    pub exit_code: u8,
    /// `verified + refused` — every candidate the scan examined.
    pub total: usize,
    /// Attestations that passed every check.
    pub verified: usize,
    /// Candidates examined and refused.
    pub refused: usize,
}

/// One verified attestation.
#[derive(Debug, Serialize)]
pub struct SbomEntry {
    /// predicateType read from the **signed** payload, never an annotation.
    pub predicate_type: String,
    /// The target digest the signed Statement was proven to bind.
    pub subject_digest: String,
    /// Digest of the OCI referrer manifest carrying the verified bundle.
    pub referrer_digest: String,
    /// Certificate SAN (identity) embedded in the Fulcio cert.
    pub certificate_identity: String,
    /// Certificate OIDC issuer embedded in the Fulcio cert.
    pub certificate_oidc_issuer: String,
    /// Rekor integrated time, RFC 3339 with an explicit `Z` (PLAT-31).
    pub signed_at: String,
    /// Populated only under `--summary`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<SbomSummaryOut>,
}

/// What `--summary` reports for one CycloneDX document.
#[derive(Debug, Serialize)]
pub struct SbomSummaryOut {
    /// The document's own `specVersion`, verbatim.
    pub spec_version: String,
    /// `serialNumber`, when the document carries one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serial_number: Option<String>,
    /// Length of the top-level `components` array.
    pub component_count: usize,
    /// `metadata.component.name`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_level_component: Option<String>,
}

impl From<ocx_lib::sbom::SbomSummary> for SbomSummaryOut {
    fn from(summary: ocx_lib::sbom::SbomSummary) -> Self {
        Self {
            spec_version: summary.spec_version,
            serial_number: summary.serial_number,
            component_count: summary.component_count,
            top_level_component: summary.top_level_component,
        }
    }
}

/// One candidate that was examined and refused.
#[derive(Debug, Serialize)]
pub struct RefusedEntry {
    /// The referrer's digest, verbatim as the registry listed it.
    pub referrer_digest: String,
    /// Why this candidate was refused, as prose for a human. Registry-sourced
    /// either way: several kinds quote a field read off the wire.
    pub reason: String,
    /// The same refusal as a frozen slug (`VerifyErrorKind::kind_detail`).
    ///
    /// PKG-25: a script branches on this, never on [`Self::reason`] — the
    /// prose is English, it is free to be reworded, and substring-matching it
    /// is how a consumer silently stops matching. `&'static str` on purpose:
    /// the only values that fit are the ones the frozen table produces.
    pub reason_kind: &'static str,
}

impl SbomListingReport {
    /// Assemble a listing from already-rendered entries and refusals.
    pub fn new(entries: Vec<SbomEntry>, refused: Vec<RefusedEntry>) -> Self {
        let summary = ListingSummary {
            status: if refused.is_empty() {
                "success"
            } else {
                "partial_failure"
            },
            exit_code: 0,
            total: entries.len() + refused.len(),
            verified: entries.len(),
            refused: refused.len(),
        };
        Self {
            summary,
            entries,
            refused,
        }
    }

    /// The plain-format cells, column-major, exactly as `print_plain` renders
    /// them — plus the truncation trailer when one is due.
    ///
    /// Extracted from `print_plain` for the reason
    /// [`VerificationReport::plain_fields`](super::verification::VerificationReport)
    /// was: `Printer` writes to the real process stdout with no injectable
    /// writer, so rendered bytes cannot be captured in-process. This pure
    /// helper carries the same value list with no `Printer` dependency, which
    /// is what makes the CWE-150 neutralization assertable.
    ///
    /// **Every** value is routed through the sanitizer, including the ones
    /// whose current source cannot carry a control character. A filter applied
    /// per field has to be re-argued for every field added later, and the
    /// neutralization is identity on hex, on an ISO-8601 stamp and on an
    /// ordinary URI — pinned by `ordinary_values_pass_through_verbatim`.
    fn plain_rows(&self) -> [Vec<String>; 3] {
        let mut kind = Vec::new();
        let mut subject = Vec::new();
        let mut detail = Vec::new();

        for entry in &self.entries {
            kind.push(sanitize_for_terminal(&entry.predicate_type));
            subject.push(sanitize_for_terminal(&entry.referrer_digest));
            detail.push(sanitize_for_terminal(&entry.describe_plain()));
        }

        // PKG-26: a fixed head plus a count, never the whole fan-out. A hostile
        // registry can list thousands of refusable referrers, and the terminal
        // is where that costs an operator their scrollback; `--format json`
        // keeps every one.
        for refusal in self.refused.iter().take(MAX_PLAIN_REFUSALS) {
            kind.push("refused".to_string());
            subject.push(sanitize_for_terminal(&refusal.referrer_digest));
            detail.push(sanitize_for_terminal(&refusal.reason));
        }
        if let Some(hidden) = self.refused.len().checked_sub(MAX_PLAIN_REFUSALS).filter(|n| *n > 0) {
            kind.push(String::new());
            subject.push(String::new());
            detail.push(format!("... and {hidden} more (see --json)"));
        }

        [kind, subject, detail]
    }
}

impl SbomEntry {
    /// The plain-format detail column: identity, issuer, signed-at, and the
    /// component count when `--summary` populated one.
    ///
    /// Joined here rather than at the call site so `plain_rows` has exactly one
    /// sanitizer call per column — the count-form guard's known evasion is two
    /// sanitized values paying for a third raw one in the same expression.
    fn describe_plain(&self) -> String {
        let mut detail = format!(
            "{} ({}) signed {}",
            self.certificate_identity, self.certificate_oidc_issuer, self.signed_at
        );
        if let Some(summary) = &self.summary {
            detail.push_str(&format!(
                ", CycloneDX {} with {} component(s)",
                summary.spec_version, summary.component_count
            ));
            if let Some(top) = &summary.top_level_component {
                detail.push_str(&format!(" under {top}"));
            }
        }
        detail
    }
}

impl Printable for SbomListingReport {
    fn print_plain(&self, data: &ocx_lib::cli::DataInterface) {
        let columns: [Column; 3] = ["Type".into(), "Referrer".into(), "Detail".into()];
        let rows = self
            .plain_rows()
            .map(|column| column.into_iter().map(Cell::from).collect::<Vec<_>>());
        data.print_table(&columns, &rows);
    }

    /// Emit a success envelope:
    /// `{"schema_version":1,"command":"package sbom","exit_code":0,"data":{...}}`.
    fn print_json(&self, data: &ocx_lib::cli::DataInterface) -> anyhow::Result<()>
    where
        Self: Sized,
    {
        let json = crate::error_envelope::render_success_envelope("package sbom", self)?;
        let parsed: serde_json::Value = serde_json::from_str(&json)?;
        Ok(data.print_json(&parsed)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(identity: &str) -> SbomEntry {
        SbomEntry {
            predicate_type: "https://cyclonedx.org/bom".into(),
            subject_digest: "sha256:aaaa".into(),
            referrer_digest: "sha256:bbbb".into(),
            certificate_identity: identity.into(),
            certificate_oidc_issuer: "https://token.actions.githubusercontent.com".into(),
            signed_at: "2026-08-19T10:00:00Z".into(),
            summary: None,
        }
    }

    fn refusal(digest: &str, reason: &str) -> RefusedEntry {
        RefusedEntry {
            referrer_digest: digest.into(),
            reason: reason.into(),
            reason_kind: "payload_type_unsupported",
        }
    }

    /// Concatenate every rendered cell, so an assertion about "what reaches the
    /// terminal" covers all three columns at once.
    fn rendered(report: &SbomListingReport) -> String {
        report
            .plain_rows()
            .iter()
            .flat_map(|column| column.iter().cloned())
            .collect::<Vec<_>>()
            .join("\u{1}")
            // The joiner is itself a control character, so that a naive
            // "contains no control" assertion cannot pass by accident on an
            // empty render.
            .replace('\u{1}', "|")
    }

    // ── the shape a script branches on ──────────────────────────────────────

    #[test]
    fn a_clean_listing_is_success_and_a_refusal_makes_it_partial() {
        let clean = SbomListingReport::new(vec![entry("you@example.com")], Vec::new());
        assert_eq!(clean.summary.status, "success");
        assert_eq!(
            (clean.summary.total, clean.summary.verified, clean.summary.refused),
            (1, 1, 0)
        );

        let mixed = SbomListingReport::new(
            vec![entry("you@example.com")],
            vec![refusal("sha256:cccc", "payload type unsupported")],
        );
        assert_eq!(mixed.summary.status, "partial_failure");
        assert_eq!(
            (mixed.summary.total, mixed.summary.verified, mixed.summary.refused),
            (2, 1, 1)
        );
    }

    /// The frozen `--format json` document. A change here is a wire change.
    #[test]
    fn json_envelope_is_the_frozen_shape() {
        let report = SbomListingReport::new(
            vec![entry("you@example.com")],
            vec![refusal("sha256:cccc", "payload type unsupported")],
        );
        let json = crate::error_envelope::render_success_envelope("package sbom", &report).expect("render");
        assert_eq!(
            json,
            concat!(
                r#"{"schema_version":1,"command":"package sbom","exit_code":0,"data":{"#,
                r#""summary":{"status":"partial_failure","exit_code":0,"total":2,"verified":1,"refused":1},"#,
                r#""entries":[{"predicate_type":"https://cyclonedx.org/bom","subject_digest":"sha256:aaaa","#,
                r#""referrer_digest":"sha256:bbbb","certificate_identity":"you@example.com","#,
                r#""certificate_oidc_issuer":"https://token.actions.githubusercontent.com","#,
                r#""signed_at":"2026-08-19T10:00:00Z"}],"#,
                r#""refused":[{"referrer_digest":"sha256:cccc","reason":"payload type unsupported","#,
                r#""reason_kind":"payload_type_unsupported"}]}}"#,
            ),
        );
    }

    /// `--format json` is a machine channel and carries the verbatim value, per
    /// the contract stated on `sanitize_for_terminal`. Pinned so a future
    /// "sanitize everywhere" pass has to argue with a test rather than
    /// silently break the byte-diff guarantee.
    #[test]
    fn json_keeps_the_hostile_bytes_verbatim() {
        let report = SbomListingReport::new(vec![entry("ev\u{202e}il@example.com")], Vec::new());
        let json = crate::error_envelope::render_success_envelope("package sbom", &report).expect("render");
        assert!(
            json.contains('\u{202e}'),
            "json must stay byte-verbatim so a consumer can diff it: {json}"
        );
    }

    // ── CWE-150 at the one render boundary ──────────────────────────────────

    /// CSI: a certificate SAN carrying `\x1b[2J` clears the operator's screen.
    #[test]
    fn plain_neutralizes_a_csi_sequence_in_a_certificate_identity() {
        let report = SbomListingReport::new(vec![entry("\u{1b}[2Jyou@example.com")], Vec::new());
        let out = rendered(&report);
        assert!(!out.contains('\u{1b}'), "ESC survived into a rendered cell: {out:?}");
        assert!(
            out.contains("[2Jyou@example.com"),
            "the payload's printable tail must remain, so the operator sees the attack: {out:?}"
        );
    }

    /// Bidi: `\u{202e}` re-orders the glyphs after it, so a refusal reason can
    /// render as a digest it does not contain (Trojan Source, CVE-2021-42574).
    /// `char::is_control` returns false for it — this is the half a
    /// "simplify to is_control" edit silently re-opens.
    #[test]
    fn plain_neutralizes_a_bidi_override_in_a_refusal_reason() {
        let report = SbomListingReport::new(
            Vec::new(),
            vec![refusal(
                "sha256:\u{202e}dead",
                "predicate type mismatch: \u{202e}gpj.exe",
            )],
        );
        let out = rendered(&report);
        assert!(!out.contains('\u{202e}'), "RLO survived into a rendered cell: {out:?}");
        assert!(out.contains("gpj.exe"), "the printable tail must remain: {out:?}");
    }

    /// A newline in a refusal reason forges an extra table row.
    #[test]
    fn plain_neutralizes_a_forged_row_in_a_refusal_reason() {
        let report = SbomListingReport::new(
            Vec::new(),
            vec![refusal(
                "sha256:dead",
                "refused\nverified  sha256:beef  trusted@example.com",
            )],
        );
        let out = rendered(&report);
        assert!(!out.contains('\n'), "a newline forges a report row: {out:?}");
    }

    #[test]
    fn ordinary_values_pass_through_verbatim() {
        let report = SbomListingReport::new(vec![entry("you@example.com")], Vec::new());
        let out = rendered(&report);
        for expected in [
            "https://cyclonedx.org/bom",
            "sha256:bbbb",
            "you@example.com",
            "https://token.actions.githubusercontent.com",
            "2026-08-19T10:00:00Z",
        ] {
            assert!(out.contains(expected), "`{expected}` must survive unchanged: {out:?}");
        }
    }

    // ── PKG-26 truncation ───────────────────────────────────────────────────

    #[test]
    fn plain_truncates_the_refusal_fanout_and_json_does_not() {
        let refused: Vec<_> = (0..MAX_PLAIN_REFUSALS + 7)
            .map(|n| refusal(&format!("sha256:{n:04}"), "payload type unsupported"))
            .collect();
        let report = SbomListingReport::new(Vec::new(), refused);

        let rows = report.plain_rows();
        assert_eq!(
            rows[0].len(),
            MAX_PLAIN_REFUSALS + 1,
            "a fixed head of {MAX_PLAIN_REFUSALS} plus exactly one trailer row",
        );
        let out = rendered(&report);
        assert!(
            out.contains("... and 7 more (see --json)"),
            "the trailer must name the hidden count and where to read them: {out:?}"
        );
        assert!(
            !out.contains("sha256:0026"),
            "the 27th refusal must not reach the terminal: {out:?}"
        );

        let json = crate::error_envelope::render_success_envelope("package sbom", &report).expect("render");
        assert!(json.contains("sha256:0026"), "--json is never truncated");
        assert_eq!(report.summary.refused, MAX_PLAIN_REFUSALS + 7);
    }

    #[test]
    fn an_exactly_full_head_gets_no_trailer() {
        let refused: Vec<_> = (0..MAX_PLAIN_REFUSALS)
            .map(|n| refusal(&format!("sha256:{n:04}"), "payload type unsupported"))
            .collect();
        let report = SbomListingReport::new(Vec::new(), refused);
        assert_eq!(
            report.plain_rows()[0].len(),
            MAX_PLAIN_REFUSALS,
            "no off-by-one trailer"
        );
        assert!(!rendered(&report).contains("more (see --json)"));
    }

    // ── structural guard on the render boundary ─────────────────────────────

    /// `plain_rows` is the only place a registry-sourced value becomes terminal
    /// bytes in this module, and a *missing* sanitizer call is the defect —
    /// which no behavioural assertion above catches for a field added later.
    ///
    /// Not a count: the count form is satisfiable by two sanitizer calls on one
    /// column paying for a third column with none. These are the three columns
    /// `plain_rows` builds, named individually.
    #[test]
    fn every_plain_column_is_neutralized() {
        let body = module_code();
        for call in [
            "sanitize_for_terminal(&entry.predicate_type)",
            "sanitize_for_terminal(&entry.referrer_digest)",
            "sanitize_for_terminal(&entry.describe_plain())",
            "sanitize_for_terminal(&refusal.referrer_digest)",
            "sanitize_for_terminal(&refusal.reason)",
        ] {
            assert!(
                body.contains(call),
                "`{call}` is missing: a registry-sourced value reaches the terminal raw"
            );
        }
        for raw in ["push(entry.", "push(refusal.", "push(&entry.", "push(&refusal."] {
            assert!(
                !body.contains(raw),
                "`{raw}` would push a registry-sourced value into a column without the sanitizer"
            );
        }
    }

    /// The scan window `module_code` slices ends at the FIRST `#[cfg(test)]`,
    /// so a marker attached to anything earlier blinds every negative assertion
    /// above at once while the positive ones keep matching.
    #[test]
    fn the_scan_window_is_not_truncatable() {
        let source = include_str!("sbom.rs");
        let (_, rest) = source
            .split_once("#[cfg(test)]")
            .expect("this module has a test half by construction");
        assert!(
            rest.trim_start().starts_with("mod tests {"),
            "the first `#[cfg(test)]` must be the test module's; a marker attached to anything \
             earlier truncates the window and every negative assertion passes vacuously"
        );
    }

    fn module_code() -> String {
        include_str!("sbom.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("the module has a non-test half")
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    }
}
