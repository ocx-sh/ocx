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

/// Which trust contract the whole listing was produced under.
///
/// A script needs this to read the rows correctly: `unverified` rows mean two
/// different things depending on it. Under [`Self::Verified`] an unverified row
/// cannot occur at all (an unsigned attachment is refused, not listed), so
/// every entry carries a checked signature. Under [`Self::Unverified`] nothing
/// was checked and every entry is unverified regardless of whether a publisher
/// signed it — a signed SBOM read this way is reported exactly like an
/// unsigned one, because this run has no evidence to tell them apart.
///
/// Deliberately the same vocabulary as the per-entry `verified` flag rather
/// than the internal mode names, so one word means one thing at both levels.
#[derive(Debug, Serialize, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum ListingVerification {
    /// Signatures were checked against the resolved policies.
    Verified,
    /// Nothing was checked; every row is unverified.
    Unverified,
}

/// The counts and the status a script branches on.
#[derive(Debug, Serialize)]
pub struct ListingSummary {
    /// `success` when nothing was refused, `partial_failure` otherwise.
    pub status: &'static str,
    /// Which trust contract produced this listing. See
    /// [`ListingVerification`].
    pub verification: ListingVerification,
    /// Mirrors the process exit code. Always 0 here, as a posture rather than
    /// as an unreachability claim: a refusal beside a listing is a partial
    /// failure the caller is told about and still exits 0 for, and under
    /// `--summary` that includes a listing whose `entries` ended up empty
    /// because every document refused. Only the library's own zero-match scan
    /// exits non-zero — `AttestationNotFound` (79) — and it never reaches a
    /// report at all.
    pub exit_code: u8,
    /// `verified + unverified + refused` — every candidate the scan examined.
    pub total: usize,
    /// Attestations that passed every check.
    pub verified: usize,
    /// Documents no signature was checked for. Counted apart from
    /// [`Self::verified`] so a script branches on the trust class instead of
    /// filtering the array — an unverified document is a real answer to "what
    /// SBOMs does this carry" and not a real answer to "who vouches for them".
    pub unverified: usize,
    /// Candidates examined and refused.
    pub refused: usize,
}

/// One verified attestation.
#[derive(Debug, Serialize)]
pub struct SbomEntry {
    /// predicateType. Read out of the **signed** payload when
    /// [`Self::verified`]; derived from the referrer's `artifactType`
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
    /// Emitted unconditionally, unlike the optional fields below and unlike
    /// `VerificationReport::signatures`: `false` is a *true* statement here —
    /// nothing supersedes this document — where an omitted-while-empty array
    /// would be a claim that we had looked. A consumer can therefore branch on
    /// the key without first testing for its presence.
    pub shadowed: bool,
    /// The target digest. Proven bound by the signed Statement when
    /// [`Self::verified`]; claimed by the referrer otherwise.
    pub subject_digest: String,
    /// What carried the document — and **not always a manifest**. Almost
    /// always the OCI referrer manifest's digest; the **layer** blob's digest
    /// in exactly one case, a verified attestation read off a cosign
    /// `sha256-<hex>.att` sidecar tag, where one layer is one document and the
    /// manifest digest would name all of them at once. A consumer that
    /// addresses it as `GET /v2/<name>/manifests/<digest>` therefore 404s on
    /// that case.
    ///
    /// Narrower than the verify side's rule, and for a structural reason: an
    /// SBOM scan runs under `VerifyContentMode::Attestation`, which leaves
    /// `discover_simplesigning` false, so the `.sig` sidecar door — the one
    /// that reports a layer digest while claiming `referrers_api`, because it
    /// inherits the *listing's* discovery method — is never opened here. The
    /// only layer digest reachable is the `.att` reader's, and that door is
    /// tag-addressed by construction.
    ///
    /// This row carries neither `signature_format` nor `discovery_method`, so
    /// there is nothing here to branch on: a consumer that must address the
    /// digest reads the same subject through
    /// `ocx package verify --attestation --format json`, whose `signatures[]`
    /// rows carry the discriminator.
    pub referrer_digest: String,
    /// Certificate SAN (identity) embedded in the Fulcio cert.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub certificate_identity: Option<String>,
    /// Certificate OIDC issuer embedded in the Fulcio cert.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub certificate_oidc_issuer: Option<String>,
    /// Rekor integrated time, RFC 3339 with an explicit `Z` (PLAT-31).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signed_at: Option<String>,
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
    pub fn new(verification: ListingVerification, entries: Vec<SbomEntry>, refused: Vec<RefusedEntry>) -> Self {
        let verified = entries.iter().filter(|entry| entry.verified).count();
        // An asserted invariant, not a derivation: `verification` names the
        // mode that *ran*, so an empty demanded listing still says `verified`
        // and deriving the field from the rows would silently rename it
        // `unverified`. What must never happen is the reverse — a demanded run
        // emitting an unverified row — because the summary would then vouch for
        // a document nothing checked. True by construction today (a demanded
        // scan refuses unsigned attachments rather than listing them, and its
        // `unverified` vector is always empty), and this makes a regression
        // that quietly changes that loud in test and debug builds.
        debug_assert!(
            verification == ListingVerification::Unverified || verified == entries.len(),
            "a demanded listing must carry no unverified rows",
        );
        let summary = ListingSummary {
            verification,
            status: if refused.is_empty() {
                "success"
            } else {
                "partial_failure"
            },
            exit_code: 0,
            total: entries.len() + refused.len(),
            verified,
            // Subtraction rather than a second filter pass: the two counts
            // partition `entries` by construction, so deriving one from the
            // other is what keeps them summing to `entries.len()`.
            unverified: entries.len() - verified,
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
    fn plain_rows(&self) -> [Vec<String>; 4] {
        let mut kind = Vec::new();
        let mut subject = Vec::new();
        let mut referrer = Vec::new();
        let mut detail = Vec::new();

        // C-011: the human-readable default is the one rendering that collapses.
        // A shadowed document is superseded, not absent — `--format json` still
        // carries it, marked — so an operator reading a table gets the document
        // that wins and a script still gets the whole picture.
        //
        // The rows arrive already grouped by subject (the scan reads one subject
        // to completion before the next), which is what makes the short subject
        // digest a legend rather than a column to sort by: adjacent rows sharing
        // one value say "these describe the same object" at a glance.
        for entry in self.entries.iter().filter(|entry| !entry.shadowed) {
            kind.push(sanitize_for_terminal(&entry.predicate_type));
            subject.push(sanitize_for_terminal(&short_digest(&entry.subject_digest)));
            referrer.push(sanitize_for_terminal(&entry.referrer_digest));
            detail.push(sanitize_for_terminal(&entry.describe_plain()));
        }

        // PKG-26: a fixed head plus a count, never the whole fan-out. A hostile
        // registry can list thousands of refusable referrers, and the terminal
        // is where that costs an operator their scrollback; `--format json`
        // keeps every one.
        for refusal in self.refused.iter().take(MAX_PLAIN_REFUSALS) {
            kind.push("refused".to_string());
            subject.push(String::new());
            referrer.push(sanitize_for_terminal(&refusal.referrer_digest));
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

/// A wire digest string in its canonical short form (`sha256:` + 12 hex).
///
/// Short because the plain-format budget allows a full 71-column digest at most
/// once per view, and `Referrer` already spends it. Falls back to the verbatim
/// string when the value does not parse — the row still has to render.
fn short_digest(digest: &str) -> String {
    ocx_lib::oci::Digest::try_from(digest).map_or_else(|_| digest.to_string(), |parsed| parsed.to_short_string())
}

impl SbomEntry {
    /// The plain-format detail column: identity, issuer, signed-at, and the
    /// component count when `--summary` populated one.
    ///
    /// Joined here rather than at the call site so `plain_rows` has exactly one
    /// sanitizer call per column — the count-form guard's known evasion is two
    /// sanitized values paying for a third raw one in the same expression.
    fn describe_plain(&self) -> String {
        // An unverified row leads with what it is, because the only thing an
        // operator must not do is read it as one of the signed ones. A blank
        // identity column would read as a rendering failure instead.
        //
        // Keyed on `verified`, which IS the trust class, never on whether the
        // three signing fields happen to be populated: those are a projection
        // of the trust class, so deriving it back from them would let one
        // missing field silently relabel a verified document as unverified.
        let mut detail = match (self.verified, &self.certificate_identity, &self.certificate_oidc_issuer) {
            (true, Some(identity), Some(issuer)) => {
                let signed_at = self.signed_at.as_deref().unwrap_or("an unknown time");
                format!("{identity} ({issuer}) signed {signed_at}")
            }
            (true, _, _) => "verified".to_string(),
            // Not "attached without a signature": under --no-verify a signed
            // publisher's bundle reads out here too, and nothing distinguishes
            // it from a raw attachment because nothing checked either. What
            // was and was not done is the only claim that holds for both.
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
    fn print_plain(&self, data: &ocx_lib::cli::DataInterface) {
        let columns: [Column; 4] = ["Type".into(), "Subject".into(), "Referrer".into(), "Detail".into()];
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
            verified: true,
            shadowed: false,
            subject_digest: "sha256:aaaa".into(),
            referrer_digest: "sha256:bbbb".into(),
            certificate_identity: Some(identity.into()),
            certificate_oidc_issuer: Some("https://token.actions.githubusercontent.com".into()),
            signed_at: Some("2026-08-19T10:00:00Z".into()),
            summary: None,
        }
    }

    /// The unsigned twin: same document, nothing vouching for it.
    fn unverified_entry(referrer_digest: &str) -> SbomEntry {
        SbomEntry {
            predicate_type: "https://cyclonedx.org/bom".into(),
            verified: false,
            shadowed: false,
            subject_digest: "sha256:aaaa".into(),
            referrer_digest: referrer_digest.into(),
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
            vec![entry("signer@example.com"), unverified_entry("sha256:cccc")],
            Vec::new(),
        );

        assert_eq!(report.summary.verified, 1);
        assert_eq!(report.summary.unverified, 1);
        assert_eq!(
            report.summary.verified + report.summary.unverified,
            report.entries.len(),
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
        let json = serde_json::to_value(unverified_entry("sha256:cccc")).expect("serialize");
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
        for entry in [entry("you@example.com"), unverified_entry("sha256:cccc")] {
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
            referrer_digest: "sha256:dddd".into(),
            ..entry("you@example.com")
        };
        let report = SbomListingReport::new(
            ListingVerification::Verified,
            vec![entry("you@example.com"), superseded],
            Vec::new(),
        );

        let json = crate::error_envelope::render_success_envelope("package sbom", &report).expect("render");
        assert!(
            json.contains("sha256:dddd"),
            "a consumer that asked for machine output gets the full picture: {json}"
        );
        assert!(
            json.contains(r#""referrer_digest":"sha256:dddd","certificate_identity""#)
                || json.contains(r#""shadowed":true"#),
            "the superseded entry must be marked, not silently identical to the preferred one: {json}"
        );

        let out = rendered(&report);
        assert!(
            !out.contains("sha256:dddd"),
            "the human default collapses to the preferred document: {out:?}"
        );
        assert!(
            out.contains("sha256:bbbb"),
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
                subject_digest: subject.clone(),
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
        let clean = SbomListingReport::new(
            ListingVerification::Verified,
            vec![entry("you@example.com")],
            Vec::new(),
        );
        assert_eq!(clean.summary.status, "success");
        assert_eq!(
            (clean.summary.total, clean.summary.verified, clean.summary.refused),
            (1, 1, 0)
        );

        let mixed = SbomListingReport::new(
            ListingVerification::Verified,
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
            ListingVerification::Verified,
            vec![entry("you@example.com")],
            vec![refusal("sha256:cccc", "payload type unsupported")],
        );
        let json = crate::error_envelope::render_success_envelope("package sbom", &report).expect("render");
        assert_eq!(
            json,
            concat!(
                r#"{"schema_version":1,"command":"package sbom","exit_code":0,"data":{"#,
                r#""summary":{"status":"partial_failure","verification":"verified","exit_code":0,"total":2,"verified":1,"unverified":0,"refused":1},"#,
                r#""entries":[{"predicate_type":"https://cyclonedx.org/bom","verified":true,"shadowed":false,"subject_digest":"sha256:aaaa","#,
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
        let report = SbomListingReport::new(
            ListingVerification::Verified,
            vec![entry("ev\u{202e}il@example.com")],
            Vec::new(),
        );
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
            ListingVerification::Verified,
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
        let report = SbomListingReport::new(
            ListingVerification::Verified,
            vec![entry("you@example.com")],
            Vec::new(),
        );
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
            "sanitize_for_terminal(&short_digest(&entry.subject_digest))",
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
