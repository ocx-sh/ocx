// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Report type for `ocx package verify` output.

use ocx_console::Cell;
use ocx_sign::sign::SignatureFormat;
use ocx_sign::verify::DiscoveryMethod;
use ocx_trust::key_ref::KeyBackendKind;
use serde::Serialize;

use crate::api::Printable;
use crate::api::data::sanitize_for_terminal;

// A plain rendering must sanitize every field: a registry-served SAN can carry an OSC 52
// clipboard write (CWE-150). JSON-only today; `serde_json` escapes control characters.
/// One discovered, verified signature.
///
/// The per-signature view of the flat `VerificationReport` fields, plus which
/// wire shape carried it, how it was found, and what produced it.
// Enum values reuse the signing library's serde slugs, so one word never means two things.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SignatureEntry {
    /// Which cosign wire shape carried this signature.
    pub signature_format: SignatureFormat,
    /// How the signature was found.
    pub discovery_method: DiscoveryMethod,
    /// What produced it: `keyless`, `file`, or a key-backend scheme.
    pub key_backend: KeyBackendKind,
    /// Digest of the referrer manifest or sidecar layer carrying it.
    pub referrer_digest: ocx_oci::Digest,
    /// Certificate SAN (identity) embedded in the Fulcio cert. Absent under a
    /// key — a legal shape, not malformed input. Registry-served, so it is
    /// untrusted input.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub certificate_identity: Option<String>,
    /// Certificate OIDC issuer embedded in the Fulcio cert. Absent under a key.
    /// Registry-served, so it is untrusted input.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub certificate_oidc_issuer: Option<String>,
    /// Rekor `integratedTime`.
    ///
    /// Certificate validity is judged against this instant, never against
    /// wall-clock now: a Fulcio certificate is valid for about ten minutes, and
    /// this timestamp is the only proof the signature happened inside that
    /// window. Absent when no transparency record exists (key mode without a
    /// Rekor upload) or its time is unrepresentable; both must be visible
    /// rather than inferred.
    // A wall-clock check fails every captured keyless fixture: its certificate has expired.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signed_at: Option<ocx_util::time::Timestamp>,
    /// Rekor log index, the dedup key when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rekor_log_index: Option<u64>,
}

/// Summary of a successful Sigstore verification.
///
/// The flat fields describe the passing signature, `signatures[0]`. Digests
/// are always full in JSON; plain output shortens `referrer_digest` to 12 hex.
#[derive(Serialize, schemars::JsonSchema)]
pub struct VerificationReport {
    /// Digest of the subject manifest whose signature was verified.
    pub subject_digest: ocx_oci::Digest,
    /// What carried the verified signature — **not always a manifest**.
    ///
    /// The OCI referrer manifest's digest for a Sigstore bundle; the **layer**
    /// blob's digest for a cosign simplesigning signature, however it was
    /// found (one layer is one signature). Discriminate on
    /// `signatures[].signature_format`, never `discovery_method`: a
    /// simplesigning sidecar found via the Referrers API still reports a layer
    /// digest. Addressable as `GET /v2/<name>/manifests/<digest>` only under
    /// `signature_format == "bundle"`.
    pub referrer_digest: ocx_oci::Digest,
    /// Certificate SAN (identity) embedded in the Fulcio cert. Absent under a key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub certificate_identity: Option<String>,
    /// Certificate OIDC issuer embedded in the Fulcio cert. Absent under a key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub certificate_oidc_issuer: Option<String>,
    /// Rekor integrated time of the signature entry. Absent when no
    /// transparency record exists (a key without a Rekor upload) or its time
    /// is unrepresentable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signed_at: Option<ocx_util::time::Timestamp>,
    /// Every verified signature discovered for the subject, the passing one
    /// first; never empty, since a verification that passed has at least one.
    pub signatures: Vec<SignatureEntry>,
}

impl VerificationReport {
    /// Report the `passing` signature, first in `signatures` and repeated by the flat fields, then `others`.
    pub fn new(subject_digest: ocx_oci::Digest, passing: SignatureEntry, others: Vec<SignatureEntry>) -> Self {
        let mut report = Self {
            subject_digest,
            referrer_digest: passing.referrer_digest.clone(),
            certificate_identity: passing.certificate_identity.clone(),
            certificate_oidc_issuer: passing.certificate_oidc_issuer.clone(),
            signed_at: passing.signed_at,
            signatures: vec![passing],
        };
        report.signatures.extend(others);
        report
    }
}

impl VerificationReport {
    /// The (label, value) rows `print_plain` renders; only `subject_digest` stays a full digest.
    ///
    /// Split out because `Printer` has no injectable writer, so tests pin the rows here.
    /// Every value is sanitized: the certificate fields are registry-served attacker input (CWE-150).
    fn plain_fields(&self) -> [(&'static str, String); 5] {
        [
            (
                "Subject digest",
                sanitize_for_terminal(&self.subject_digest.to_string()),
            ),
            (
                "Referrer digest",
                sanitize_for_terminal(&self.referrer_digest.to_short_string()),
            ),
            (
                "Certificate identity",
                sanitize_for_terminal(self.certificate_identity.as_deref().unwrap_or_default()),
            ),
            (
                "Certificate OIDC issuer",
                sanitize_for_terminal(self.certificate_oidc_issuer.as_deref().unwrap_or_default()),
            ),
            (
                "Signed at",
                sanitize_for_terminal(&self.signed_at.map(|at| at.to_string()).unwrap_or_default()),
            ),
        ]
    }
}

impl Printable for VerificationReport {
    const SCHEMA_VERSION: u32 = 2;
    const ROOT: &'static str = "VerificationReport";

    fn print_plain(&self, data: &ocx_console::DataInterface) {
        let mut rows: [Vec<Cell>; 2] = [Vec::new(), Vec::new()];
        for (label, value) in self.plain_fields() {
            rows[0].push(Cell::from(label.to_string()));
            rows[1].push(Cell::from(value));
        }
        data.print_table(&["Field".into(), "Value".into()], &rows);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::data::is_bidi_control;

    fn at(instant: &str) -> ocx_util::time::Timestamp {
        serde_json::from_value(serde_json::Value::from(instant)).expect("an RFC 3339 instant")
    }

    fn sample_signature() -> SignatureEntry {
        SignatureEntry {
            signature_format: SignatureFormat::Bundle,
            discovery_method: DiscoveryMethod::ReferrersApi,
            key_backend: KeyBackendKind::Keyless,
            referrer_digest: ocx_oci::Digest::Sha256("b".repeat(64)),
            certificate_identity: Some("test-signer@example.com".into()),
            certificate_oidc_issuer: Some("https://fake-oidc.test".into()),
            signed_at: Some(at("2026-04-19T12:00:00Z")),
            rekor_log_index: Some(42),
        }
    }

    fn sample_report() -> VerificationReport {
        VerificationReport::new(ocx_oci::Digest::Sha256("a".repeat(64)), sample_signature(), Vec::new())
    }

    #[test]
    fn json_output_is_the_unwrapped_report() {
        let document = serde_json::to_value(sample_report()).expect("serialize");
        for envelope_key in ["schema_version", "command", "exit_code", "data", "error"] {
            assert!(
                document.get(envelope_key).is_none(),
                "{envelope_key} leaked: {document}"
            );
        }
        // JSON keeps every digest full, unlike plain mode — a shortened 12-hex
        // form also satisfies `starts_with("sha256:")`, so exact equality is
        // required to pin that JSON never shortens (see
        // `print_plain_shortens_referrer_digest_but_not_subject` for the
        // plain-mode counterpart).
        assert_eq!(document["subject_digest"], format!("sha256:{}", "a".repeat(64)));
        assert_eq!(document["referrer_digest"], format!("sha256:{}", "b".repeat(64)));
        assert_eq!(document["certificate_identity"], "test-signer@example.com");
        assert_eq!(document["certificate_oidc_issuer"], "https://fake-oidc.test");
        assert_eq!(document["signed_at"], "2026-04-19T12:00:00Z");
    }

    /// `signatures` is always present and leads with the passing signature
    /// the flat fields repeat; a verification that passed has at least one.
    #[test]
    fn signatures_is_always_present_and_leads_with_the_passing_one() {
        let other = SignatureEntry {
            referrer_digest: ocx_oci::Digest::Sha256("d".repeat(64)),
            ..sample_signature()
        };
        let document = serde_json::to_value(VerificationReport::new(
            ocx_oci::Digest::Sha256("a".repeat(64)),
            sample_signature(),
            vec![other],
        ))
        .expect("serialize");
        let signatures = document["signatures"].as_array().expect("signatures is an array");
        assert_eq!(signatures.len(), 2);
        assert_eq!(signatures[0]["referrer_digest"], document["referrer_digest"]);
        assert_ne!(signatures[1]["referrer_digest"], document["referrer_digest"]);

        let single = serde_json::to_value(sample_report()).expect("serialize");
        assert_eq!(single["signatures"].as_array().map(Vec::len), Some(1), "{single}");
    }

    /// Under a key without a Rekor upload the flat certificate and time fields
    /// are absent, not empty strings.
    #[test]
    fn a_key_mode_verification_omits_the_certificate_and_time() {
        let key_mode = SignatureEntry {
            key_backend: KeyBackendKind::File,
            certificate_identity: None,
            certificate_oidc_issuer: None,
            signed_at: None,
            rekor_log_index: None,
            ..sample_signature()
        };
        let document = serde_json::to_value(VerificationReport::new(
            ocx_oci::Digest::Sha256("a".repeat(64)),
            key_mode,
            Vec::new(),
        ))
        .expect("serialize");
        for absent in ["certificate_identity", "certificate_oidc_issuer", "signed_at"] {
            assert!(document.get(absent).is_none(), "{absent} must be absent: {document}");
        }
    }

    /// The per-signature row spells its three vocabularies with the library's
    /// frozen serde slugs, and omits the optional fields it does not carry.
    #[test]
    fn signature_entry_json_shape() {
        let value = serde_json::to_value(sample_signature()).expect("serialize");
        assert_eq!(value["signature_format"], "bundle");
        assert_eq!(value["discovery_method"], "referrers_api");
        assert_eq!(value["key_backend"], "keyless");
        assert_eq!(value["referrer_digest"], format!("sha256:{}", "b".repeat(64)));
        assert_eq!(value["certificate_identity"], "test-signer@example.com");
        assert_eq!(value["signed_at"], "2026-04-19T12:00:00Z");
        assert_eq!(value["rekor_log_index"], 42);

        // Key mode carries no certificate and, without a Rekor upload, no
        // transparency record: those keys are absent rather than null, so a
        // consumer distinguishes "not applicable" from "failed to render".
        let key_mode = SignatureEntry {
            key_backend: KeyBackendKind::File,
            certificate_identity: None,
            certificate_oidc_issuer: None,
            signed_at: None,
            rekor_log_index: None,
            ..sample_signature()
        };
        let value = serde_json::to_value(key_mode).expect("serialize");
        let object = value.as_object().expect("entry serializes as an object");
        assert_eq!(value["key_backend"], "file");
        for absent in [
            "certificate_identity",
            "certificate_oidc_issuer",
            "signed_at",
            "rekor_log_index",
        ] {
            assert!(!object.contains_key(absent), "{absent} must be absent, not null");
        }
    }

    /// `signatures[]` is **JSON-only** — the whole CWE-150 answer, and smaller
    /// and stricter than wiring a sanitizer.
    ///
    /// A registry-served SAN can embed `\x1b]52;c;<b64>\x07` and set the
    /// operator's clipboard, so the struct note requires *every* field of a row
    /// — the typed ones included — to pass `sanitize_for_terminal` the moment a
    /// row reaches a plain-text table. Not rendering it at all is what makes
    /// that requirement vacuous today. Pinned behaviourally: adding rows to the
    /// plain table reds this rather than silently shipping unsanitized
    /// registry-controlled bytes to a terminal.
    #[test]
    fn plain_output_never_renders_the_signatures_array() {
        let bare = sample_report();
        let mut populated = sample_report();
        populated.signatures.push(sample_signature());
        populated.signatures.push(SignatureEntry {
            certificate_identity: Some("\u{1b}]52;c;cGF5bG9hZA==\u{7}".into()),
            ..sample_signature()
        });

        assert_eq!(
            bare.plain_fields().len(),
            populated.plain_fields().len(),
            "a signature row must not add a plain-mode field",
        );
        assert_eq!(
            bare.plain_fields(),
            populated.plain_fields(),
            "the plain table must be identical whether or not signatures[] carries rows",
        );
        // And the hostile row really is in the report — otherwise the equality
        // above would be comparing two identical arrays and proving nothing.
        assert_eq!(populated.signatures.len(), 3);
    }

    /// `print_plain` shortens `referrer_digest` to 12 hex (only `subject_digest`
    /// earns a full `sha256:<64hex>` row) — smoke-checks the table renders
    /// without panic.
    #[test]
    fn print_plain_smoke() {
        let report = sample_report();
        let data = ocx_console::DataInterface::new(ocx_console::Printer::new(false, false));
        report.print_plain(&data);
    }

    /// Pins the plain-mode digest-shortening contract on the actual
    /// `(label, value)` pairs `print_plain` renders: `subject_digest` stays
    /// full (it is the answer) and `referrer_digest` shortens to 12 hex.
    #[test]
    fn print_plain_shortens_referrer_digest_but_not_subject() {
        let report = sample_report();
        let fields = report.plain_fields();

        let value_for = |label: &str| -> String {
            fields
                .iter()
                .find(|(field_label, _)| *field_label == label)
                .map(|(_, value)| value.clone())
                .unwrap_or_else(|| panic!("missing field {label:?} in plain_fields()"))
        };

        let full_subject = format!("sha256:{}", "a".repeat(64));
        let full_referrer = format!("sha256:{}", "b".repeat(64));

        assert_eq!(
            value_for("Subject digest"),
            full_subject,
            "subject digest must stay full"
        );
        assert_eq!(
            value_for("Referrer digest"),
            report.referrer_digest.to_short_string(),
            "referrer digest must shorten to 12 hex"
        );
        assert_ne!(
            value_for("Referrer digest"),
            full_referrer,
            "referrer digest must not render the full 64-hex form"
        );
    }

    // ── CWE-150 — terminal neutralization at the print site ──────────────────

    // Corpus note: one test per attack class, because the classes fail
    // differently — a filter that strips CSI but not a bidi override passes
    // any test written from a single example (SEC-34). They are separate
    // named tests rather than `#[rstest] #[case(...)]` rows because `rstest`
    // is not a workspace dependency; a `for` loop over an array would abort
    // at the first failure and report one opaque name, which is the property
    // TEST-04 protects.

    /// A report whose certificate fields carry `hostile`, rendered to the
    /// exact `(label, value)` pairs `print_plain` writes.
    fn rendered_with(hostile: &str) -> Vec<String> {
        let passing = SignatureEntry {
            certificate_identity: Some(hostile.to_string()),
            certificate_oidc_issuer: Some(hostile.to_string()),
            ..sample_signature()
        };
        let report = VerificationReport::new(ocx_oci::Digest::Sha256("a".repeat(64)), passing, Vec::new());
        report.plain_fields().into_iter().map(|(_, value)| value).collect()
    }

    /// Asserts no cell reaches the terminal carrying an active sequence.
    fn assert_neutralized(hostile: &str) {
        for cell in rendered_with(hostile) {
            assert!(
                !cell.chars().any(|c| c.is_control() || is_bidi_control(c)),
                "verify row {cell:?} reached the terminal unneutralized"
            );
        }
    }

    /// Same check, plus the literal codepoint the payload injected.
    ///
    /// `assert_neutralized` alone asks `is_bidi_control` whether the output is
    /// clean, and the sanitizer filters on that same function -- so narrowing
    /// its range would leave every bidi case green while a raw override
    /// reaches the terminal. The literal is an oracle the production code
    /// cannot move.
    fn assert_neutralized_without(hostile: &str, injected: char) {
        assert_neutralized(hostile);
        for cell in rendered_with(hostile) {
            assert!(
                !cell.contains(injected),
                "verify row {cell:?} still carries {injected:?}"
            );
        }
    }

    #[test]
    fn csi_colour_in_a_certificate_field_is_neutralized() {
        assert_neutralized("\u{1b}[31mtrusted@example.com");
    }

    #[test]
    fn osc8_hyperlink_in_a_certificate_field_is_neutralized() {
        assert_neutralized("\u{1b}]8;;https://evil.test\u{7}trusted@example.com\u{1b}]8;;\u{7}");
    }

    #[test]
    fn osc52_clipboard_write_in_a_certificate_field_is_neutralized() {
        // The measured failure: a hostile registry serves a bundle whose cert
        // SAN embeds an OSC 52, and `ocx package verify` sets the operator's
        // system clipboard from anything that can write to the tty.
        assert_neutralized("\u{1b}]52;c;ZXZpbA==\u{7}");
    }

    #[test]
    fn bidi_override_in_a_certificate_field_is_neutralized() {
        // Trojan Source (CVE-2021-42574): the identity renders as text it does
        // not contain, so an operator reading the row approves the wrong signer.
        assert_neutralized_without("\u{202e}moc.elpmaxe@rengis", '\u{202e}');
    }

    #[test]
    fn bidi_isolate_in_a_certificate_field_is_neutralized() {
        assert_neutralized_without("\u{2066}signer@example.com\u{2069}", '\u{2066}');
    }

    #[test]
    fn newline_in_a_certificate_field_cannot_forge_a_report_row() {
        assert_neutralized("signer@example.com\nCertificate OIDC issuer | https://accounts.google.com");
    }

    #[test]
    fn nul_in_a_certificate_field_is_neutralized() {
        assert_neutralized("signer\u{0}@example.com");
    }

    #[test]
    fn zero_width_and_bom_are_stripped_like_every_other_invisible() {
        // Inverted from the WP9a-era decision that scoped the sanitizer to
        // terminal-active characters only. Invisible is not harmless here:
        // `you@exam\u{200b}ple.com` renders pixel-identical to the identity a
        // reader believes they approved, so SEC-34's set includes them. Pinned
        // per payload so the two surfaces cannot diverge, and so this payload
        // never grows a second hand-rolled filter (SEC-31).
        for (invisible, visible) in [
            ("a\u{200d}b", "ab"),
            ("\u{feff}signer@example.com", "signer@example.com"),
        ] {
            let rows = rendered_with(invisible);
            assert!(
                !rows.iter().any(|cell| cell.contains(invisible)),
                "{invisible:?} survived the sanitizer; got {rows:?}"
            );
            // Positive control: the visible remainder must still arrive, or a
            // sanitizer that dropped the whole field would pass the line above.
            assert!(
                rows.iter().any(|cell| cell == visible),
                "the visible remainder {visible:?} did not reach the report; got {rows:?}"
            );
        }
    }

    #[test]
    fn ordinary_values_pass_through_verbatim() {
        // The other half of the neutralization: it must be invisible for every
        // value `ocx` itself produces, or the report is silently rewriting its
        // own data. This is what licenses routing the typed digest and
        // timestamp fields through the same call as the free-text ones.
        let report = sample_report();
        let expected = [
            format!("sha256:{}", "a".repeat(64)),
            report.referrer_digest.to_short_string(),
            "test-signer@example.com".to_string(),
            "https://fake-oidc.test".to_string(),
            "2026-04-19T12:00:00Z".to_string(),
        ];
        let rendered: Vec<String> = report.plain_fields().into_iter().map(|(_, value)| value).collect();
        assert_eq!(rendered, expected, "neutralization must be identity on our own values");
    }

    #[test]
    fn json_keeps_the_certificate_identity_verbatim() {
        // `--format json` is a machine channel, not a terminal: a consumer
        // comparing the identity against its own policy needs the real bytes,
        // and `serde_json` escapes the C0 range by specification so the raw
        // ESC never reaches a terminal through this path either.
        let hostile = "\u{1b}]52;c;ZXZpbA==\u{7}signer@example.com";
        let passing = SignatureEntry {
            certificate_identity: Some(hostile.to_string()),
            ..sample_signature()
        };
        let report = VerificationReport::new(ocx_oci::Digest::Sha256("a".repeat(64)), passing, Vec::new());
        let json = serde_json::to_string(&report).expect("render ok");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(
            parsed["certificate_identity"], hostile,
            "JSON must carry the identity verbatim, not the display form"
        );
        assert!(
            !json.contains('\u{1b}'),
            "serde_json must have escaped the ESC rather than emitting it raw"
        );
    }
}
