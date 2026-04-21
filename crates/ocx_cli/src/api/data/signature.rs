// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Report type for `ocx package sign` output.
//!
//! Renders the subject + referrer descriptors the sign pipeline produced,
//! plus the cert identity/issuer embedded in the signing cert. This lets
//! downstream tools (CI verification, human review) confirm exactly what
//! was signed and by whom without re-fetching the bundle.

// Consumed by `command/package_sign.rs` in Phase 5.
#![allow(dead_code)]

use ocx_lib::oci;
use serde::Serialize;

use crate::api::Printable;

/// Summary of a successful keyless signing operation.
///
/// Plain format: single "Field | Value" table listing the subject digest,
/// bundle digest, referrer digest, cert identity, and cert OIDC issuer (one
/// row per field — `Printable` single-table rule honored).
///
/// JSON format: `{ identifier, subject_digest, bundle_digest, referrer_digest,
/// certificate_identity, certificate_oidc_issuer }`.
///
/// `bundle_digest` and `referrer_digest` are distinct and not interchangeable:
/// the former is the SHA-256 of the Sigstore bundle v0.3 protobuf blob (the
/// layer content; what Rekor's inclusion proof covers), while the latter is
/// the SHA-256 of the OCI referrer manifest JSON (what the Referrers API
/// returns). Consumers routinely need one or the other.
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
    /// Certificate SAN (identity) embedded in the Fulcio cert.
    pub certificate_identity: String,
    /// Certificate OIDC issuer URL embedded in the Fulcio cert.
    pub certificate_oidc_issuer: String,
}

impl SignatureReport {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        identifier: String,
        subject_digest: oci::Digest,
        bundle_digest: oci::Digest,
        referrer_digest: oci::Digest,
        certificate_identity: String,
        certificate_oidc_issuer: String,
    ) -> Self {
        Self {
            identifier,
            subject_digest,
            bundle_digest,
            referrer_digest,
            certificate_identity,
            certificate_oidc_issuer,
        }
    }
}

impl Printable for SignatureReport {
    fn print_plain(&self, printer: &ocx_lib::cli::Printer) {
        let fields = [
            ("Identifier", self.identifier.clone()),
            ("Subject digest", self.subject_digest.to_string()),
            ("Bundle digest", self.bundle_digest.to_string()),
            ("Referrer digest", self.referrer_digest.to_string()),
            ("Certificate identity", self.certificate_identity.clone()),
            ("Certificate OIDC issuer", self.certificate_oidc_issuer.clone()),
        ];
        let mut rows: [Vec<String>; 2] = [Vec::new(), Vec::new()];
        for (label, value) in fields {
            rows[0].push(label.to_string());
            rows[1].push(value);
        }
        printer.print_table(&["Field", "Value"], &rows);
    }
}
