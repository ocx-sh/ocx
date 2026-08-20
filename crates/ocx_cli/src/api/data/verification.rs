// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Report type for `ocx package verify` output.
//!
//! Renders the verified subject + referrer digests and the certificate identity
//! and issuer that the signature attests. The flat shape is the slice-1
//! acceptance contract (`test/tests/test_verify.py`): a single verified
//! signature per invocation. A future multi-signature slice can add a
//! `signatures[]` array without breaking these top-level fields.

use ocx_lib::cli::Cell;
use ocx_lib::oci;
use serde::Serialize;

use crate::api::Printable;
use crate::api::data::sanitize_for_terminal;

/// Summary of a successful Sigstore verification.
///
/// Plain format: single "Field | Value" table listing the subject digest,
/// referrer digest, cert identity, cert OIDC issuer, and signed-at timestamp.
/// `subject_digest` is the answer (what was verified) and renders full;
/// `referrer_digest` shortens to 12 hex — a full `sha256:<64hex>` earns its
/// row only once per view. Both stay full in JSON.
///
/// JSON format: `{ subject_digest, referrer_digest, certificate_identity,
/// certificate_oidc_issuer, signed_at }`.
#[derive(Serialize)]
pub struct VerificationReport {
    /// Digest of the subject manifest whose signature was verified.
    pub subject_digest: oci::Digest,
    /// Digest of the OCI referrer manifest carrying the verified bundle.
    pub referrer_digest: oci::Digest,
    /// Certificate SAN (identity) embedded in the Fulcio cert.
    pub certificate_identity: String,
    /// Certificate OIDC issuer embedded in the Fulcio cert.
    pub certificate_oidc_issuer: String,
    /// Rekor integrated time (ISO-8601 UTC) of the signature entry.
    pub signed_at: String,
}

impl VerificationReport {
    /// Construct a verification report.
    pub fn new(
        subject_digest: oci::Digest,
        referrer_digest: oci::Digest,
        certificate_identity: String,
        certificate_oidc_issuer: String,
        signed_at: String,
    ) -> Self {
        Self {
            subject_digest,
            referrer_digest,
            certificate_identity,
            certificate_oidc_issuer,
            signed_at,
        }
    }
}

impl VerificationReport {
    /// The (label, value) pairs `print_plain` renders, in display order.
    ///
    /// Extracted from `print_plain` so the digest-shortening contract can be
    /// pinned by a unit test: `Printer` writes directly to the real process
    /// stdout with no injectable writer (see `data_interface.rs`), so
    /// `print_plain`'s rendered bytes cannot be captured in-process. This pure
    /// helper carries the same field list with no `Printer` dependency.
    /// Every value is neutralized for the terminal (CWE-150), not only the two
    /// obviously-foreign ones. `certificate_identity` and
    /// `certificate_oidc_issuer` are read out of a Fulcio certificate carried
    /// in a bundle a *registry* served, so on the verify path they are attacker
    /// input by construction — a SAN embedding `\x1b]52;c;<b64>\x07` sets the
    /// operator's clipboard. The digests and the timestamp are typed values
    /// that cannot carry a control character, and are routed anyway: a filter
    /// applied per field has to be re-argued for every field added later, and
    /// the neutralization is identity on hex and on an ISO-8601 stamp — pinned
    /// by `ordinary_values_pass_through_verbatim`.
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
                sanitize_for_terminal(&self.certificate_identity),
            ),
            (
                "Certificate OIDC issuer",
                sanitize_for_terminal(&self.certificate_oidc_issuer),
            ),
            ("Signed at", sanitize_for_terminal(&self.signed_at)),
        ]
    }
}

impl Printable for VerificationReport {
    fn print_plain(&self, data: &ocx_lib::cli::DataInterface) {
        // `subject_digest` is the answer (what was verified) and stays full;
        // `referrer_digest` shortens to 12 hex so only one full
        // sha256:<64hex> earns its row (subsystem-cli-api.md "Plain-Mode
        // Column Budget"). Both remain full in JSON.
        let mut rows: [Vec<Cell>; 2] = [Vec::new(), Vec::new()];
        for (label, value) in self.plain_fields() {
            rows[0].push(Cell::from(label.to_string()));
            rows[1].push(Cell::from(value));
        }
        data.print_table(&["Field".into(), "Value".into()], &rows);
    }

    /// Emit a success envelope:
    /// `{"schema_version":1,"command":"package verify","exit_code":0,"data":{...}}`.
    fn print_json(&self, data: &ocx_lib::cli::DataInterface) -> anyhow::Result<()>
    where
        Self: Sized,
    {
        let json = crate::error_envelope::render_success_envelope("package verify", self)?;
        let parsed: serde_json::Value = serde_json::from_str(&json)?;
        Ok(data.print_json(&parsed)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::data::is_bidi_control;

    fn sample_report() -> VerificationReport {
        VerificationReport::new(
            ocx_lib::oci::Digest::Sha256("a".repeat(64)),
            ocx_lib::oci::Digest::Sha256("b".repeat(64)),
            "test-signer@example.com".into(),
            "https://fake-oidc.test".into(),
            "2026-04-19T12:00:00Z".into(),
        )
    }

    #[test]
    fn json_output_matches_acceptance_contract() {
        // Flat shape pinned by test/tests/test_verify.py::test_verify_success_envelope_golden_shape.
        let report = sample_report();
        let json = crate::error_envelope::render_success_envelope("package verify", &report).expect("render ok");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(parsed["schema_version"], 1);
        assert_eq!(parsed["command"], "package verify");
        assert_eq!(parsed["exit_code"], 0);
        let data = &parsed["data"];
        // JSON keeps every digest full, unlike plain mode — a shortened 12-hex
        // form also satisfies `starts_with("sha256:")`, so exact equality is
        // required to pin that JSON never shortens (see
        // `print_plain_shortens_referrer_digest_but_not_subject` for the
        // plain-mode counterpart).
        assert_eq!(data["subject_digest"], format!("sha256:{}", "a".repeat(64)));
        assert_eq!(data["referrer_digest"], format!("sha256:{}", "b".repeat(64)));
        assert_eq!(data["certificate_identity"], "test-signer@example.com");
        assert_eq!(data["certificate_oidc_issuer"], "https://fake-oidc.test");
        assert!(parsed.get("error").is_none(), "success branch must not carry error");
    }

    /// `print_plain` shortens `referrer_digest` to 12 hex (only `subject_digest`
    /// earns a full `sha256:<64hex>` row) — smoke-checks the table renders
    /// without panic.
    #[test]
    fn print_plain_smoke() {
        let report = sample_report();
        let data = ocx_lib::cli::DataInterface::new(ocx_lib::cli::Printer::new(false, false));
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

    /// A report whose every free-text field carries `hostile`, rendered to the
    /// exact `(label, value)` pairs `print_plain` writes.
    fn rendered_with(hostile: &str) -> Vec<String> {
        let report = VerificationReport::new(
            ocx_lib::oci::Digest::Sha256("a".repeat(64)),
            ocx_lib::oci::Digest::Sha256("b".repeat(64)),
            hostile.to_string(),
            hostile.to_string(),
            hostile.to_string(),
        );
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
        let report = VerificationReport::new(
            ocx_lib::oci::Digest::Sha256("a".repeat(64)),
            ocx_lib::oci::Digest::Sha256("b".repeat(64)),
            hostile.to_string(),
            "https://fake-oidc.test".into(),
            "2026-04-19T12:00:00Z".into(),
        );
        let json = crate::error_envelope::render_success_envelope("package verify", &report).expect("render ok");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(
            parsed["data"]["certificate_identity"], hostile,
            "JSON must carry the identity verbatim, not the display form"
        );
        assert!(
            !json.contains('\u{1b}'),
            "serde_json must have escaped the ESC rather than emitting it raw"
        );
    }
}
