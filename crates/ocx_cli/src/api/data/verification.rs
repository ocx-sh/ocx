// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Report type for `ocx verify` output.
//!
//! Renders the subject + referrer digests and the cert identity/issuer that
//! the Sigstore bundle asserts. Includes the Rekor-integrated signing time
//! and the `cert_expired_but_tlog_valid` flag so consumers can observe the
//! "cert expired yet SET witnesses pre-expiry signing" success path.

// Consumed by `command/verify.rs` in Phase 5.
#![allow(dead_code)]

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
    #[allow(clippy::too_many_arguments)]
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
}
