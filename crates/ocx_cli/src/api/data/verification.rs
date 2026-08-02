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
    fn plain_fields(&self) -> [(&'static str, String); 5] {
        [
            ("Subject digest", self.subject_digest.to_string()),
            ("Referrer digest", self.referrer_digest.to_short_string()),
            ("Certificate identity", self.certificate_identity.clone()),
            ("Certificate OIDC issuer", self.certificate_oidc_issuer.clone()),
            ("Signed at", self.signed_at.clone()),
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
}
