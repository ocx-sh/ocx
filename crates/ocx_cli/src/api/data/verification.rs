// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Report type for `ocx verify` output (C-S1-1 frozen shape).
//!
//! Renders the subject identifier, bundle digest, signature count, and per-signature
//! fields (format, discovery method, certificate info, Rekor entry) as mandated by
//! ADR §C-S1-1.
//!
//! Plain format: single table with rows for `Subject`, `Bundle digest`,
//! `Signature count`, then per-signature: `Format`, `Discovery`, `Cert issuer`,
//! `Cert SAN`, `Cert not before`, `Cert not after`, `Rekor log index`,
//! `Rekor integrated time`, `Rekor log id`.
//!
//! JSON format: `{ subject, bundle_digest, signature_count, signatures: [{...}] }`.

use ocx_lib::cli::Cell;
use ocx_lib::oci;
use serde::Serialize;

use crate::api::Printable;

/// Certificate fields from the Fulcio cert embedded in a Sigstore bundle.
#[derive(Serialize)]
pub struct VerificationCertificate {
    /// OIDC issuer URL embedded in the Fulcio cert.
    pub issuer: String,
    /// Subject Alternative Name (identity) embedded in the Fulcio cert.
    pub san: String,
    /// Certificate not-before time (ISO-8601 UTC, e.g. `"2026-04-19T11:55:00Z"`).
    pub not_before: String,
    /// Certificate not-after time (ISO-8601 UTC, e.g. `"2026-04-19T12:05:00Z"`).
    pub not_after: String,
}

/// Rekor transparency-log entry fields for a single signature.
#[derive(Serialize)]
pub struct VerificationRekor {
    /// Rekor log index for the entry.
    pub log_index: u64,
    /// Rekor integrated time (ISO-8601 UTC, e.g. `"2026-04-19T12:00:00Z"`).
    pub integrated_time: String,
    /// Rekor log identifier (hex public key digest of the log).
    pub log_id: String,
}

/// A single verified signature entry.
#[derive(Serialize)]
pub struct VerificationSignature {
    /// Sigstore bundle format (e.g. `"sigstore-bundle-v0.3"`).
    pub signature_format: String,
    /// How the bundle was found (e.g. `"referrers-api"`).
    pub discovery_method: String,
    /// Certificate fields from the Fulcio cert.
    pub certificate: VerificationCertificate,
    /// Rekor transparency-log entry fields.
    pub rekor: VerificationRekor,
}

/// Summary of a successful Sigstore verification (C-S1-1 frozen shape).
///
/// Plain format: single table with rows for `Subject`, `Bundle digest`,
/// `Signature count`, then per-signature rows for `Format`, `Discovery`,
/// `Cert issuer`, `Cert SAN`, `Cert not before`, `Cert not after`,
/// `Rekor log index`, `Rekor integrated time`, `Rekor log id`.
///
/// JSON format: `{ subject, bundle_digest, signature_count, signatures: [{...}] }`.
#[derive(Serialize)]
pub struct VerificationReport {
    /// User-facing identifier string that was verified (echoes the CLI arg).
    pub subject: String,
    /// Digest of the bundle referrer manifest carrying the signature.
    pub bundle_digest: oci::Digest,
    /// Number of valid signatures found (Slice 1: always 1).
    pub signature_count: u64,
    /// Per-signature detail (Slice 1: exactly one entry).
    pub signatures: Vec<VerificationSignature>,
}

impl VerificationReport {
    /// Construct a [`VerificationReport`] for Slice 1 (single signature, referrers-API discovery).
    ///
    /// Defaults: `signature_format = "sigstore-bundle-v0.3"`, `discovery_method = "referrers-api"`,
    /// `signature_count = 1`.
    #[allow(dead_code)] // Phase 5c consumer
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        subject: String,
        bundle_digest: oci::Digest,
        cert_issuer: String,
        cert_san: String,
        cert_not_before: String,
        cert_not_after: String,
        rekor_log_index: u64,
        rekor_integrated_time: String,
        rekor_log_id: String,
    ) -> Self {
        Self {
            subject,
            bundle_digest,
            signature_count: 1,
            signatures: vec![VerificationSignature {
                signature_format: "sigstore-bundle-v0.3".to_string(),
                discovery_method: "referrers-api".to_string(),
                certificate: VerificationCertificate {
                    issuer: cert_issuer,
                    san: cert_san,
                    not_before: cert_not_before,
                    not_after: cert_not_after,
                },
                rekor: VerificationRekor {
                    log_index: rekor_log_index,
                    integrated_time: rekor_integrated_time,
                    log_id: rekor_log_id,
                },
            }],
        }
    }
}

impl Printable for VerificationReport {
    fn print_plain(&self, data: &ocx_lib::cli::DataInterface) {
        let mut labels: Vec<String> = Vec::new();
        let mut values: Vec<String> = Vec::new();

        // Summary rows
        labels.push("Subject".to_string());
        values.push(self.subject.clone());

        labels.push("Bundle digest".to_string());
        values.push(self.bundle_digest.to_string());

        labels.push("Signature count".to_string());
        values.push(self.signature_count.to_string());

        // Per-signature rows (single block for Slice 1)
        for sig in &self.signatures {
            labels.push("Format".to_string());
            values.push(sig.signature_format.clone());

            labels.push("Discovery".to_string());
            values.push(sig.discovery_method.clone());

            labels.push("Cert issuer".to_string());
            values.push(sig.certificate.issuer.clone());

            labels.push("Cert SAN".to_string());
            values.push(sig.certificate.san.clone());

            labels.push("Cert not before".to_string());
            values.push(sig.certificate.not_before.clone());

            labels.push("Cert not after".to_string());
            values.push(sig.certificate.not_after.clone());

            labels.push("Rekor log index".to_string());
            values.push(sig.rekor.log_index.to_string());

            labels.push("Rekor integrated time".to_string());
            values.push(sig.rekor.integrated_time.clone());

            labels.push("Rekor log id".to_string());
            values.push(sig.rekor.log_id.clone());
        }

        let rows: [Vec<Cell>; 2] = [
            labels.into_iter().map(Cell::from).collect(),
            values.into_iter().map(Cell::from).collect(),
        ];
        data.print_table(&["Field".into(), "Value".into()], &rows);
    }

    /// Emit a C-S1-1 success envelope:
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
            "ocx.sh/cmake:3.28".into(),
            ocx_lib::oci::Digest::Sha256("c2d0".repeat(16)),
            "https://token.actions.githubusercontent.com".into(),
            "https://github.com/org/repo/.github/workflows/build.yml@refs/heads/main".into(),
            "2026-04-19T11:55:00Z".into(),
            "2026-04-19T12:05:00Z".into(),
            98765432,
            "2026-04-19T12:00:00Z".into(),
            "abcdef1234567890".into(),
        )
    }

    #[test]
    fn json_output_contains_c_s1_1_envelope() {
        // Validate that print_json routes through render_success_envelope and
        // produces the C-S1-1 frozen envelope shape.
        let report = sample_report();
        let json = crate::error_envelope::render_success_envelope("package verify", &report).expect("render ok");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(parsed["schema_version"], 1);
        // ADR §C-S1-1: command field carries the canonical space-separated CLI command,
        // matching `canonical_command_name` in `app.rs` (mirrors sign side's "package sign").
        assert_eq!(parsed["command"], "package verify");
        assert_eq!(parsed["exit_code"], 0);
        let data = &parsed["data"];
        assert_eq!(data["subject"], "ocx.sh/cmake:3.28");
        assert!(
            data["bundle_digest"].as_str().is_some_and(|s| s.starts_with("sha256:")),
            "bundle_digest must be sha256:... string"
        );
        assert_eq!(data["signature_count"], 1);
        let sigs = data["signatures"].as_array().expect("signatures is array");
        assert_eq!(sigs.len(), 1, "Slice 1: exactly one signature");
        let sig = &sigs[0];
        assert_eq!(sig["signature_format"], "sigstore-bundle-v0.3");
        assert_eq!(sig["discovery_method"], "referrers-api");
        assert_eq!(
            sig["certificate"]["san"],
            "https://github.com/org/repo/.github/workflows/build.yml@refs/heads/main"
        );
        assert_eq!(
            sig["certificate"]["issuer"],
            "https://token.actions.githubusercontent.com"
        );
        assert_eq!(sig["certificate"]["not_before"], "2026-04-19T11:55:00Z");
        assert_eq!(sig["certificate"]["not_after"], "2026-04-19T12:05:00Z");
        assert_eq!(sig["rekor"]["log_index"], 98765432_u64);
        assert_eq!(
            sig["rekor"]["integrated_time"], "2026-04-19T12:00:00Z",
            "Rekor integrated_time must be ISO-8601 string, not epoch u64"
        );
        assert_eq!(sig["rekor"]["log_id"], "abcdef1234567890");
    }
}
