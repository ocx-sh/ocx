// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Report type for `ocx package sbom`; a refused candidate is reported beside the matches, not
//! dropped (`adr_sbom_attestations.md` D-e). Every plain value is registry-sourced (CWE-150).

use ocx_console::{Cell, Column};
use serde::Serialize;

use crate::api::Printable;
use crate::api::data::sanitize_for_terminal;

/// Plain-format refusal head; `--format json` is never truncated.
const MAX_PLAIN_REFUSALS: usize = 20;

/// Every verified attestation a package carries, plus what was refused.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SbomListingReport {
    /// One-glance counts, so a consumer branches on a field instead of
    /// measuring an array.
    pub summary: ListingSummary,
    /// One entry per listed attestation, in listing order.
    pub attestations: Vec<SbomEntry>,
    /// Every candidate examined and refused, in listing order. Never
    /// truncated in JSON.
    pub refused: Vec<RefusedEntry>,
}

// Same vocabulary as the per-entry `verified` flag, so one word means one thing at both levels.
/// Which trust contract the whole listing was produced under.
///
/// Under `verified` an unverified row cannot occur (an unsigned attachment is
/// refused, not listed), so every entry carries a checked signature. Under
/// `unverified` nothing was checked and every entry is unverified, even one a
/// publisher signed: this run has no evidence to tell them apart.
#[derive(Debug, Serialize, schemars::JsonSchema, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum ListingVerification {
    /// Signatures were checked against the resolved policies.
    Verified,
    /// Nothing was checked; every row is unverified.
    Unverified,
}

/// Whether every examined candidate was listed.
///
/// A refusal beside a listing is a partial failure the caller is told about and
/// the run still exits 0, even when every document refused under `--summary`.
/// Only a zero-match scan exits non-zero (79), and it prints no listing.
#[derive(Debug, Serialize, schemars::JsonSchema, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum ListingStatus {
    /// Nothing was refused.
    Success,
    /// At least one candidate was refused; see `refused`.
    PartialFailure,
}

/// The counts and the status a script branches on.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ListingSummary {
    /// Whether any candidate was refused.
    pub status: ListingStatus,
    /// Which trust contract produced this listing: `verified` or `unverified`.
    pub verification: ListingVerification,
    /// `verified + unverified + refused` — every candidate the scan examined.
    pub total: usize,
    /// Attestations that passed every check.
    pub verified: usize,
    /// Documents no signature was checked for. Counted apart from
    /// `verified` so a script branches on the trust class instead of
    /// filtering the array — an unverified document is a real answer to "what
    /// SBOMs does this carry" and not a real answer to "who vouches for them".
    pub unverified: usize,
    /// Candidates examined and refused.
    pub refused: usize,
}

/// One verified attestation.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SbomEntry {
    /// predicateType. Read out of the **signed** payload when `verified`;
    /// derived from the referrer's `artifactType`
    /// otherwise, since an unsigned referrer states its type nowhere else.
    pub predicate_type: String,
    /// Whether a signature was verified over this document.
    ///
    /// `false` means the SBOM was attached raw, with no identity behind it:
    /// the registry served bytes and said what they are. The three fields
    /// below are then absent rather than empty.
    pub verified: bool,
    /// `true` when a platform-level SBOM of the **same predicateType**
    /// supersedes this index-level one. A shadowed entry stays listed under
    /// `--format json`; only the human default collapses to the preferred one.
    ///
    /// Emitted unconditionally: `false` is a *true* statement — nothing
    /// supersedes this document — so a consumer can branch on the key without
    /// first testing for its presence.
    pub shadowed: bool,
    /// The target digest. Proven bound by the signed Statement when
    /// `verified`; claimed by the referrer otherwise.
    pub subject_digest: ocx_oci::Digest,
    /// What carried the document — **not always a manifest**.
    ///
    /// Almost always the OCI referrer manifest's digest; the **layer** blob's
    /// digest in exactly one case, a verified attestation read off a cosign
    /// `sha256-<hex>.att` sidecar tag (one layer is one document), where
    /// `GET /v2/<name>/manifests/<digest>` 404s. This row carries no
    /// discriminator: to address the digest, read the same subject through
    /// `ocx package verify --attestation --format json`, whose `signatures[]`
    /// rows carry `signature_format`.
    // Only the `.att` reader yields a layer digest here: an SBOM scan never enables `discover_simplesigning`.
    pub referrer_digest: ocx_oci::Digest,
    /// Certificate SAN (identity) embedded in the Fulcio cert.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub certificate_identity: Option<String>,
    /// Certificate OIDC issuer embedded in the Fulcio cert.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub certificate_oidc_issuer: Option<String>,
    /// Rekor integrated time. Absent when no transparency record exists or its time is unrepresentable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signed_at: Option<ocx_util::time::Timestamp>,
    /// Populated only under `--summary`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<SbomSummaryOut>,
}

/// What `--summary` reports for one CycloneDX document.
#[derive(Debug, Serialize, schemars::JsonSchema)]
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

impl From<ocx_sign::sbom::SbomSummary> for SbomSummaryOut {
    fn from(summary: ocx_sign::sbom::SbomSummary) -> Self {
        Self {
            spec_version: summary.spec_version,
            serial_number: summary.serial_number,
            component_count: summary.component_count,
            top_level_component: summary.top_level_component,
        }
    }
}

/// One candidate that was examined and refused.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct RefusedEntry {
    /// The referrer's digest as the registry listed it. Absent when no
    /// well-formed digest names the candidate: a budget-stop row, or a
    /// registry listing a malformed digest.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub referrer_digest: Option<ocx_oci::Digest>,
    /// Why this candidate was refused, as prose for a human. Registry-sourced
    /// either way: several kinds quote a field read off the wire.
    pub reason: String,
    /// The same refusal as a frozen slug.
    ///
    /// Branch on this, never on `reason`: the prose is English and free to be
    /// reworded, and substring-matching it is how a consumer silently stops
    /// matching.
    // `&'static str`: only the frozen `VerifyErrorKind::kind_detail` table fits.
    pub reason_kind: &'static str,
}

impl SbomListingReport {
    pub fn new(verification: ListingVerification, attestations: Vec<SbomEntry>, refused: Vec<RefusedEntry>) -> Self {
        let verified = attestations.iter().filter(|entry| entry.verified).count();
        // Asserted, not derived: derived, an empty demanded listing would read `unverified`.
        debug_assert!(
            verification == ListingVerification::Unverified || verified == attestations.len(),
            "a demanded listing must carry no unverified rows",
        );
        let summary = ListingSummary {
            verification,
            status: if refused.is_empty() {
                ListingStatus::Success
            } else {
                ListingStatus::PartialFailure
            },
            total: attestations.len() + refused.len(),
            verified,
            // Derived, so the two counts always sum to `attestations.len()`.
            unverified: attestations.len() - verified,
            refused: refused.len(),
        };
        Self {
            summary,
            attestations,
            refused,
        }
    }

    /// The plain cells, column-major, plus the truncation trailer; a pure helper so the CWE-150
    /// neutralization is testable. Every value is sanitized, including ones that cannot yet carry a
    /// control character, so a field added later is covered.
    fn plain_rows(&self) -> [Vec<String>; 4] {
        let mut kind = Vec::new();
        let mut subject = Vec::new();
        let mut referrer = Vec::new();
        let mut detail = Vec::new();

        // Shadowed rows stay in JSON, marked; plain shows only the winner.
        for entry in self.attestations.iter().filter(|entry| !entry.shadowed) {
            kind.push(sanitize_for_terminal(&entry.predicate_type));
            subject.push(sanitize_for_terminal(&entry.subject_digest.to_short_string()));
            referrer.push(sanitize_for_terminal(&entry.referrer_digest.to_string()));
            detail.push(sanitize_for_terminal(&entry.describe_plain()));
        }

        // A fixed head plus a count: a hostile registry can list thousands of refusals.
        for refusal in self.refused.iter().take(MAX_PLAIN_REFUSALS) {
            kind.push("refused".to_string());
            subject.push(String::new());
            referrer.push(sanitize_for_terminal(&refusal.digest_label()));
            detail.push(sanitize_for_terminal(&refusal.reason));
        }
        if let Some(hidden) = self.refused.len().checked_sub(MAX_PLAIN_REFUSALS).filter(|n| *n > 0) {
            kind.push(String::new());
            subject.push(String::new());
            referrer.push(String::new());
            detail.push(format!("... and {hidden} more (see --json)"));
        }

        [kind, subject, referrer, detail]
    }
}

impl RefusedEntry {
    /// The plain `Referrer` cell: the digest, or blank when none names the candidate.
    fn digest_label(&self) -> String {
        self.referrer_digest
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default()
    }
}

impl SbomEntry {
    /// The plain detail column: identity, issuer, signed-at and, under `--summary`, the component
    /// count, joined here so `plain_rows` sanitizes each column exactly once.
    fn describe_plain(&self) -> String {
        // Keyed on `verified`, never on the signing fields, or one missing field relabels a verified document.
        let mut detail = match (self.verified, &self.certificate_identity, &self.certificate_oidc_issuer) {
            (true, Some(identity), Some(issuer)) => {
                let signed_at = self
                    .signed_at
                    .map_or_else(|| "an unknown time".to_string(), |at| at.to_string());
                format!("{identity} ({issuer}) signed {signed_at}")
            }
            (true, _, _) => "verified".to_string(),
            // Not "unsigned": under `--no-verify` a signed bundle lands here too, since nothing checked it.
            (false, _, _) => "UNVERIFIED - no signature was checked".to_string(),
        };
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
    const SCHEMA_VERSION: u32 = 2;
    const ROOT: &'static str = "SbomListingReport";

    fn print_plain(&self, data: &ocx_console::DataInterface) {
        let columns: [Column; 4] = ["Type".into(), "Subject".into(), "Referrer".into(), "Detail".into()];
        let rows = self
            .plain_rows()
            .map(|column| column.into_iter().map(Cell::from).collect::<Vec<_>>());
        data.print_table(&columns, &rows);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A full-length sha256 digest of one repeated hex character.
    fn digest(fill: char) -> ocx_oci::Digest {
        ocx_oci::Digest::Sha256(fill.to_string().repeat(64))
    }

    fn at(instant: &str) -> ocx_util::time::Timestamp {
        serde_json::from_value(serde_json::Value::from(instant)).expect("an RFC 3339 instant")
    }

    fn entry(identity: &str) -> SbomEntry {
        SbomEntry {
            predicate_type: "https://cyclonedx.org/bom".into(),
            verified: true,
            shadowed: false,
            subject_digest: digest('a'),
            referrer_digest: digest('b'),
            certificate_identity: Some(identity.into()),
            certificate_oidc_issuer: Some("https://token.actions.githubusercontent.com".into()),
            signed_at: Some(at("2026-08-19T10:00:00Z")),
            summary: None,
        }
    }

    /// The unsigned twin: same document, nothing vouching for it.
    fn unverified_entry(referrer_digest: ocx_oci::Digest) -> SbomEntry {
        SbomEntry {
            predicate_type: "https://cyclonedx.org/bom".into(),
            verified: false,
            shadowed: false,
            subject_digest: digest('a'),
            referrer_digest,
            certificate_identity: None,
            certificate_oidc_issuer: None,
            signed_at: None,
            summary: None,
        }
    }

    /// The two counts partition the entries, and the plain row for an
    /// unverified document says so where an operator reads it.
    ///
    /// The mixed fixture is synthetic — neither mode emits both classes in one
    /// listing — because the subject here is the counting arithmetic, not a
    /// reachable listing. The mode is `Unverified` and must stay so: the
    /// constructor asserts that a *demanded* listing carries no unverified row,
    /// which is the direction where a wrong summary would vouch for a document
    /// nothing checked.
    #[test]
    fn the_summary_partitions_entries_by_trust_class() {
        let report = SbomListingReport::new(
            ListingVerification::Unverified,
            vec![entry("signer@example.com"), unverified_entry(digest('c'))],
            Vec::new(),
        );

        assert_eq!(report.summary.verified, 1);
        assert_eq!(report.summary.unverified, 1);
        assert_eq!(
            report.summary.verified + report.summary.unverified,
            report.attestations.len(),
            "the two counts must partition the entries, not overlap or leak",
        );

        let [_, _, _, detail] = report.plain_rows();
        assert!(
            detail[0].contains("signer@example.com"),
            "the verified row still names its signer: {detail:?}"
        );
        assert!(
            detail[1].contains("UNVERIFIED"),
            "an unverified row must say so rather than render a blank identity: {detail:?}"
        );
    }

    /// An unverified entry omits the three signing keys rather than emitting
    /// them empty — an empty SAN reads as an identity that failed to render.
    #[test]
    fn an_unverified_entry_omits_the_signing_keys() {
        let json = serde_json::to_value(unverified_entry(digest('c'))).expect("serialize");
        assert_eq!(json["verified"], false);
        for absent in ["certificate_identity", "certificate_oidc_issuer", "signed_at"] {
            assert!(json.get(absent).is_none(), "{absent} must be absent, not empty");
        }
        // Positive control: the signed shape still carries all three, so this
        // cannot pass by the keys having been dropped everywhere.
        let signed = serde_json::to_value(entry("signer@example.com")).expect("serialize");
        assert_eq!(signed["verified"], true);
        assert_eq!(signed["certificate_identity"], "signer@example.com");
        assert_eq!(signed["signed_at"], "2026-08-19T10:00:00Z");
    }

    /// T-20. `shadowed` is emitted on **every** entry, verified or not.
    ///
    /// The mirror image of `VerificationReport`'s `signatures`, and
    /// deliberately so: an always-`false` boolean is a true statement (nothing
    /// supersedes this document), whereas an always-empty array would claim a
    /// search that never ran. A `skip_serializing_if` added here reds this.
    ///
    /// The second half pins that the key tracks the field rather than being a
    /// constant, so a predicate that merely happened to answer "keep" for
    /// `false` cannot pass.
    #[test]
    fn sbom_entry_json_shape_always_carries_shadowed() {
        for entry in [entry("you@example.com"), unverified_entry(digest('c'))] {
            let verified = entry.verified;
            let json = serde_json::to_value(entry).expect("serialize");
            let object = json.as_object().expect("entry serializes as an object");
            assert!(
                object.contains_key("shadowed"),
                "`shadowed` must be present on every entry (verified={verified}): {json}"
            );
            assert_eq!(json["shadowed"], false);
        }

        let shadowed = SbomEntry {
            shadowed: true,
            ..entry("you@example.com")
        };
        let json = serde_json::to_value(shadowed).expect("serialize");
        assert_eq!(json["shadowed"], true, "the key must track the field: {json}");
    }

    /// **C-011 rule 2.** A shadowed document stays in `--format json`, marked;
    /// only the human-readable default collapses to the preferred one.
    ///
    /// The two halves are one test on purpose: dropping a shadowed entry from
    /// the report — instead of marking it — would satisfy the plain half alone,
    /// and a renderer that ignored `shadowed` would satisfy the JSON half alone.
    #[test]
    fn a_shadowed_document_leaves_the_table_and_stays_in_json() {
        let superseded = SbomEntry {
            shadowed: true,
            referrer_digest: digest('d'),
            ..entry("you@example.com")
        };
        let report = SbomListingReport::new(
            ListingVerification::Verified,
            vec![entry("you@example.com"), superseded],
            Vec::new(),
        );

        let superseded = digest('d').to_string();
        let json = serde_json::to_value(&report).expect("serialize");
        assert_eq!(
            json["attestations"][1]["referrer_digest"], superseded,
            "a consumer that asked for machine output gets the full picture: {json}"
        );
        assert_eq!(
            json["attestations"][1]["shadowed"], true,
            "the superseded entry must be marked, not silently identical to the preferred one: {json}"
        );

        let out = rendered(&report);
        assert!(
            !out.contains(&superseded),
            "the human default collapses to the preferred document: {out:?}"
        );
        assert!(
            out.contains(&digest('b').to_string()),
            "positive control — the preferred document still renders, so the assertion above \
             cannot pass on an empty table: {out:?}"
        );
        assert_eq!(
            (report.summary.total, report.summary.verified),
            (2, 2),
            "shadowing is a rendering decision; both documents were still found",
        );
    }

    /// **C-011 rule 3.** With no platform selected the listing spans whatever
    /// subjects were read, so each row names its own — short, because the
    /// plain-format budget already spends its one full digest on `Referrer`.
    #[test]
    fn the_plain_table_names_each_rows_subject_in_short_form() {
        let subject = format!("sha256:{}", "ab".repeat(32));
        let report = SbomListingReport::new(
            ListingVerification::Verified,
            vec![SbomEntry {
                subject_digest: ocx_oci::Digest::try_from(subject.as_str()).expect("digest"),
                ..entry("you@example.com")
            }],
            Vec::new(),
        );
        let [_, rendered_subject, ..] = report.plain_rows();
        assert_eq!(
            rendered_subject,
            vec!["sha256:abababababab".to_string()],
            "the subject column carries the short form, not the 71-column one",
        );
        assert_ne!(
            rendered_subject[0], subject,
            "the full digest is 71 columns and `Referrer` already spends the one this view allows",
        );
    }

    fn refusal(referrer_digest: Option<ocx_oci::Digest>, reason: &str) -> RefusedEntry {
        RefusedEntry {
            referrer_digest,
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
        let clean = SbomListingReport::new(
            ListingVerification::Verified,
            vec![entry("you@example.com")],
            Vec::new(),
        );
        assert_eq!(clean.summary.status, ListingStatus::Success);
        assert_eq!(
            (clean.summary.total, clean.summary.verified, clean.summary.refused),
            (1, 1, 0)
        );

        let mixed = SbomListingReport::new(
            ListingVerification::Verified,
            vec![entry("you@example.com")],
            vec![refusal(Some(digest('c')), "payload type unsupported")],
        );
        assert_eq!(mixed.summary.status, ListingStatus::PartialFailure);
        assert_eq!(
            (mixed.summary.total, mixed.summary.verified, mixed.summary.refused),
            (2, 1, 1)
        );
    }

    /// The frozen `--format json` document. A change here is a wire change.
    #[test]
    fn json_document_is_the_frozen_shape() {
        let report = SbomListingReport::new(
            ListingVerification::Verified,
            vec![entry("you@example.com")],
            vec![
                refusal(Some(digest('c')), "payload type unsupported"),
                refusal(None, "budget exhausted"),
            ],
        );
        let json = serde_json::to_string(&report).expect("serialize");
        let (a, b, c) = (digest('a'), digest('b'), digest('c'));
        assert_eq!(
            json,
            [
                r#"{"summary":{"status":"partial_failure","verification":"verified","total":3,"verified":1,"unverified":0,"refused":2},"#,
                r#""attestations":[{"predicate_type":"https://cyclonedx.org/bom","verified":true,"shadowed":false,"#,
                &format!(r#""subject_digest":"{a}","referrer_digest":"{b}","certificate_identity":"you@example.com","#),
                r#""certificate_oidc_issuer":"https://token.actions.githubusercontent.com","#,
                r#""signed_at":"2026-08-19T10:00:00Z"}],"#,
                &format!(r#""refused":[{{"referrer_digest":"{c}","reason":"payload type unsupported","#),
                r#""reason_kind":"payload_type_unsupported"},"#,
                r#"{"reason":"budget exhausted","reason_kind":"payload_type_unsupported"}]}"#,
            ]
            .concat(),
        );
    }

    /// `--format json` is a machine channel and carries the verbatim value, per
    /// the contract stated on `sanitize_for_terminal`. Pinned so a future
    /// "sanitize everywhere" pass has to argue with a test rather than
    /// silently break the byte-diff guarantee.
    #[test]
    fn json_keeps_the_hostile_bytes_verbatim() {
        let report = SbomListingReport::new(
            ListingVerification::Verified,
            vec![entry("ev\u{202e}il@example.com")],
            Vec::new(),
        );
        let json = serde_json::to_string(&report).expect("serialize");
        assert!(
            json.contains('\u{202e}'),
            "json must stay byte-verbatim so a consumer can diff it: {json}"
        );
    }

    // ── CWE-150 at the one render boundary ──────────────────────────────────

    /// CSI: a certificate SAN carrying `\x1b[2J` clears the operator's screen.
    #[test]
    fn plain_neutralizes_a_csi_sequence_in_a_certificate_identity() {
        let report = SbomListingReport::new(
            ListingVerification::Verified,
            vec![entry("\u{1b}[2Jyou@example.com")],
            Vec::new(),
        );
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
            ListingVerification::Verified,
            Vec::new(),
            vec![refusal(Some(digest('d')), "predicate type mismatch: \u{202e}gpj.exe")],
        );
        let out = rendered(&report);
        assert!(!out.contains('\u{202e}'), "RLO survived into a rendered cell: {out:?}");
        assert!(out.contains("gpj.exe"), "the printable tail must remain: {out:?}");
    }

    /// A newline in a refusal reason forges an extra table row.
    #[test]
    fn plain_neutralizes_a_forged_row_in_a_refusal_reason() {
        let report = SbomListingReport::new(
            ListingVerification::Verified,
            Vec::new(),
            vec![refusal(
                Some(digest('d')),
                "refused\nverified  sha256:beef  trusted@example.com",
            )],
        );
        let out = rendered(&report);
        assert!(!out.contains('\n'), "a newline forges a report row: {out:?}");
    }

    #[test]
    fn ordinary_values_pass_through_verbatim() {
        let report = SbomListingReport::new(
            ListingVerification::Verified,
            vec![entry("you@example.com")],
            Vec::new(),
        );
        let out = rendered(&report);
        for expected in [
            "https://cyclonedx.org/bom",
            &digest('b').to_string(),
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
            .map(|n| refusal(None, &format!("payload type unsupported #{n:04}")))
            .collect();
        let report = SbomListingReport::new(ListingVerification::Verified, Vec::new(), refused);

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
            !out.contains("#0026"),
            "the 27th refusal must not reach the terminal: {out:?}"
        );

        let json = serde_json::to_string(&report).expect("serialize");
        assert!(json.contains("#0026"), "--json is never truncated");
        assert_eq!(report.summary.refused, MAX_PLAIN_REFUSALS + 7);
    }

    #[test]
    fn an_exactly_full_head_gets_no_trailer() {
        let refused: Vec<_> = (0..MAX_PLAIN_REFUSALS)
            .map(|_| refusal(None, "payload type unsupported"))
            .collect();
        let report = SbomListingReport::new(ListingVerification::Verified, Vec::new(), refused);
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
    /// column paying for a third column with none. These are the four columns
    /// `plain_rows` builds, named individually.
    #[test]
    fn every_plain_column_is_neutralized() {
        let body = module_code();
        for call in [
            "sanitize_for_terminal(&entry.predicate_type)",
            "sanitize_for_terminal(&entry.subject_digest.to_short_string())",
            "sanitize_for_terminal(&entry.referrer_digest.to_string())",
            "sanitize_for_terminal(&entry.describe_plain())",
            "sanitize_for_terminal(&refusal.digest_label())",
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
