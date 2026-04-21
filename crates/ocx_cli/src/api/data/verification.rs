// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Report type for `ocx verify` output.
//!
//! Renders the subject + referrer digests and the cert identity/issuer that
//! the Sigstore bundle asserts. Includes the Rekor-integrated signing time
//! and the `cert_expired_but_tlog_valid` flag so consumers can observe the
//! "cert expired yet SET witnesses pre-expiry signing" success path.

// Consumed by `command/verify.rs` in Phase 5.

use ocx_lib::oci;
use serde::Serialize;

use crate::api::Printable;

/// Summary of a successful Sigstore verification.
///
/// Plain format: single "Field | Value" table.
///
/// JSON format: `{ identifier, subject_digest, referrer_digest, certificate_identity,
/// certificate_oidc_issuer, signed_at, cert_expired_but_tlog_valid }`.
#[derive(Serialize)]
pub struct VerificationReport {
    /// User-facing identifier string that was verified (echoes the CLI arg).
    pub identifier: String,
    /// Digest of the subject manifest that the bundle was verified against.
    pub subject_digest: oci::Digest,
    /// Digest of the bundle referrer manifest that carried the signature.
    pub referrer_digest: oci::Digest,
    /// Certificate SAN (identity) embedded in the Fulcio cert — matches the
    /// user's `--certificate-identity`.
    pub certificate_identity: String,
    /// Certificate OIDC issuer URL embedded in the Fulcio cert — matches the
    /// user's `--certificate-oidc-issuer`.
    pub certificate_oidc_issuer: String,
    /// Rekor-integrated signing time (UTC epoch seconds).
    pub signed_at: u64,
    /// When `true`, the signing cert had expired by verification time, but
    /// the Rekor SET attests it was integrated pre-expiry (success path).
    pub cert_expired_but_tlog_valid: bool,
}

impl VerificationReport {
    #[allow(dead_code)] // Phase 5c consumer
    pub fn new(
        identifier: String,
        subject_digest: oci::Digest,
        referrer_digest: oci::Digest,
        certificate_identity: String,
        certificate_oidc_issuer: String,
        signed_at: u64,
        cert_expired_but_tlog_valid: bool,
    ) -> Self {
        Self {
            identifier,
            subject_digest,
            referrer_digest,
            certificate_identity,
            certificate_oidc_issuer,
            signed_at,
            cert_expired_but_tlog_valid,
        }
    }
}

impl Printable for VerificationReport {
    fn print_plain(&self, printer: &ocx_lib::cli::Printer) {
        let fields = [
            ("Identifier", self.identifier.clone()),
            ("Subject digest", self.subject_digest.to_string()),
            ("Referrer digest", self.referrer_digest.to_string()),
            ("Certificate identity", self.certificate_identity.clone()),
            ("Certificate OIDC issuer", self.certificate_oidc_issuer.clone()),
            ("Signed at (epoch s)", self.signed_at.to_string()),
            ("Cert expired, tlog valid", self.cert_expired_but_tlog_valid.to_string()),
        ];
        let mut rows: [Vec<String>; 2] = [Vec::new(), Vec::new()];
        for (label, value) in fields {
            rows[0].push(label.to_string());
            rows[1].push(value);
        }
        printer.print_table(&["Field", "Value"], &rows);
    }

    /// Emit a C-S1-1 success envelope:
    /// `{"schema_version":1,"command":"verify","exit_code":0,"data":{...}}`.
    fn print_json(&self, printer: &ocx_lib::cli::Printer) -> anyhow::Result<()>
    where
        Self: Sized,
    {
        let json = crate::error_envelope::render_success_envelope("verify", self)?;
        let parsed: serde_json::Value = serde_json::from_str(&json)?;
        Ok(printer.print_json(&parsed)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_report() -> VerificationReport {
        VerificationReport::new(
            "registry.example/pkg:1.0".into(),
            ocx_lib::oci::Digest::Sha256("a".repeat(64)),
            ocx_lib::oci::Digest::Sha256("c".repeat(64)),
            "signer@example.com".into(),
            "https://accounts.google.com".into(),
            1_700_000_000,
            false,
        )
    }

    #[test]
    fn json_output_contains_c_s1_1_envelope() {
        // Validate that print_json routes through render_success_envelope and
        // produces the C-S1-1 frozen envelope shape.
        let report = sample_report();
        let json = crate::error_envelope::render_success_envelope("verify", &report).expect("render ok");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(parsed["schema_version"], 1);
        assert_eq!(parsed["command"], "verify");
        assert_eq!(parsed["exit_code"], 0);
        let data = &parsed["data"];
        assert_eq!(data["identifier"], "registry.example/pkg:1.0");
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
        assert_eq!(data["signed_at"], 1_700_000_000);
        assert_eq!(data["cert_expired_but_tlog_valid"], false);
    }
}
