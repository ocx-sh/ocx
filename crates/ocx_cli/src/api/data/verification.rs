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
/// Plain format: single "Field | Value" table.
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

impl Printable for VerificationReport {
    fn print_plain(&self, data: &ocx_lib::cli::DataInterface) {
        let fields = [
            ("Subject digest", self.subject_digest.to_string()),
            ("Referrer digest", self.referrer_digest.to_string()),
            ("Certificate identity", self.certificate_identity.clone()),
            ("Certificate OIDC issuer", self.certificate_oidc_issuer.clone()),
            ("Signed at", self.signed_at.clone()),
        ];
        let mut rows: [Vec<Cell>; 2] = [Vec::new(), Vec::new()];
        for (label, value) in fields {
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
        assert!(
            data["subject_digest"]
                .as_str()
                .is_some_and(|s| s.starts_with("sha256:"))
        );
        assert!(
            data["referrer_digest"]
                .as_str()
                .is_some_and(|s| s.starts_with("sha256:"))
        );
        assert_eq!(data["certificate_identity"], "test-signer@example.com");
        assert_eq!(data["certificate_oidc_issuer"], "https://fake-oidc.test");
        assert!(parsed.get("error").is_none(), "success branch must not carry error");
    }
}
