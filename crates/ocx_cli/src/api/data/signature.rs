// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Report type for `ocx package sign` output.
//!
//! Renders the subject + referrer descriptors the sign pipeline produced,
//! plus the cert identity/issuer embedded in the signing cert. This lets
//! downstream tools (CI verification, human review) confirm exactly what
//! was signed and by whom without re-fetching the bundle.

// Consumed by `command/package_sign.rs` in Phase 5.

use ocx_lib::cli::Cell;
use ocx_lib::oci;
use serde::Serialize;

use crate::api::Printable;

/// Summary of a successful keyless signing operation.
///
/// Plain format: single "Field | Value" table listing the subject digest,
/// bundle digest, referrer digest, platform, signer, cert identity, and cert
/// OIDC issuer (one row per field — `Printable` single-table rule honored).
///
/// JSON format: `{ identifier, subject_digest, bundle_digest, referrer_digest,
/// platform, signer, certificate_identity, certificate_oidc_issuer }`.
///
/// `bundle_digest` and `referrer_digest` are distinct and not interchangeable:
/// the former is the SHA-256 of the Sigstore bundle v0.3 protobuf blob (the
/// layer content; what Rekor's inclusion proof covers), while the latter is
/// the SHA-256 of the OCI referrer manifest JSON (what the Referrers API
/// returns). Consumers routinely need one or the other.
///
/// `signer` is the signing mechanism used; for Slice 1 this is always
/// `"keyless-fulcio"`.
#[derive(Serialize)]
pub struct SignatureReport {
    /// User-facing identifier string that was signed (echoes the CLI arg).
    pub identifier: String,
    /// Digest of the subject manifest that the bundle signs.
    pub subject_digest: oci::Digest,
    /// Digest of the Sigstore bundle blob (the referrer's layer content).
    pub bundle_digest: oci::Digest,
    /// Digest of the published OCI referrer manifest wrapping the bundle.
    pub referrer_digest: oci::Digest,
    /// Platform that was signed (e.g., `linux/amd64`).
    pub platform: String,
    /// Signing mechanism used (C-S1-1 contract field). Always `"keyless-fulcio"` in Slice 1.
    pub signer: String,
    /// Certificate SAN (identity) embedded in the Fulcio cert.
    pub certificate_identity: String,
    /// Certificate OIDC issuer URL embedded in the Fulcio cert.
    pub certificate_oidc_issuer: String,
}

impl SignatureReport {
    pub fn new(
        identifier: String,
        subject_digest: oci::Digest,
        bundle_digest: oci::Digest,
        referrer_digest: oci::Digest,
        platform: &oci::Platform,
        certificate_identity: String,
        certificate_oidc_issuer: String,
    ) -> Self {
        Self {
            identifier,
            subject_digest,
            bundle_digest,
            referrer_digest,
            platform: platform.to_string(),
            signer: "keyless-fulcio".to_string(),
            certificate_identity,
            certificate_oidc_issuer,
        }
    }
}

impl Printable for SignatureReport {
    fn print_plain(&self, data: &ocx_lib::cli::DataInterface) {
        let fields = [
            ("Identifier", self.identifier.clone()),
            ("Subject digest", self.subject_digest.to_string()),
            ("Bundle digest", self.bundle_digest.to_string()),
            ("Referrer digest", self.referrer_digest.to_string()),
            ("Platform", self.platform.clone()),
            ("Signer", self.signer.clone()),
            ("Certificate identity", self.certificate_identity.clone()),
            ("Certificate OIDC issuer", self.certificate_oidc_issuer.clone()),
        ];
        let mut rows: [Vec<Cell>; 2] = [Vec::new(), Vec::new()];
        for (label, value) in fields {
            rows[0].push(Cell::from(label.to_string()));
            rows[1].push(Cell::from(value));
        }
        data.print_table(&["Field".into(), "Value".into()], &rows);
    }

    /// Emit a C-S1-1 success envelope:
    /// `{"schema_version":1,"command":"package sign","exit_code":0,"data":{...}}`.
    fn print_json(&self, data: &ocx_lib::cli::DataInterface) -> anyhow::Result<()>
    where
        Self: Sized,
    {
        let json = crate::error_envelope::render_success_envelope("package sign", self)?;
        let parsed: serde_json::Value = serde_json::from_str(&json)?;
        Ok(data.print_json(&parsed)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_report() -> SignatureReport {
        SignatureReport::new(
            "registry.example/pkg:1.0".into(),
            ocx_lib::oci::Digest::Sha256("a".repeat(64)),
            ocx_lib::oci::Digest::Sha256("b".repeat(64)),
            ocx_lib::oci::Digest::Sha256("c".repeat(64)),
            &"linux/amd64".parse().expect("platform"),
            "signer@example.com".into(),
            "https://accounts.google.com".into(),
        )
    }

    #[test]
    fn json_output_contains_c_s1_1_envelope() {
        // Capture stdout via a custom test: we call render_success_envelope directly
        // since Printer writes to stdout (not a buffer). This unit test validates
        // that print_json delegates through render_success_envelope correctly.
        let report = sample_report();
        let json = crate::error_envelope::render_success_envelope("package sign", &report).expect("render ok");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(parsed["schema_version"], 1);
        assert_eq!(parsed["command"], "package sign");
        assert_eq!(parsed["exit_code"], 0);
        let data = &parsed["data"];
        assert_eq!(data["identifier"], "registry.example/pkg:1.0");
        assert!(
            data["subject_digest"]
                .as_str()
                .is_some_and(|s| s.starts_with("sha256:"))
        );
        assert!(data["bundle_digest"].as_str().is_some_and(|s| s.starts_with("sha256:")));
        assert!(
            data["referrer_digest"]
                .as_str()
                .is_some_and(|s| s.starts_with("sha256:"))
        );
        // C-S1-1 contract: platform must serialize as a plain string (e.g. "linux/amd64").
        assert_eq!(data["platform"], "linux/amd64", "data[platform] must be a plain string");
        assert_eq!(data["signer"], "keyless-fulcio");
    }
}
