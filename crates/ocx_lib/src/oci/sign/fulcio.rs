// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fulcio CSR client — `/api/v2/signingCert`.
//!
//! Thin wrapper over `sigstore::fulcio::FulcioClient`, routed to the v2
//! endpoint (`request_cert_v2`) rather than the deprecated v1 path. Emits
//! typed errors on HTTP failures — 401/403 map to
//! [`SignErrorKind::OidcTokenRejected`] / [`SignErrorKind::FulcioBadRequest`].
//!
//! Phase 1 stub — bodies use `unimplemented!()`.

use super::error::SignErrorKind;
use super::oidc::OidcToken;

/// Fulcio-issued signing certificate with chain.
pub struct FulcioCertificate {
    /// Leaf certificate (PEM-encoded).
    pub leaf: Vec<u8>,
    /// Intermediate + root chain (PEM-encoded).
    pub chain: Vec<Vec<u8>>,
}

/// Fulcio client.
pub struct FulcioClient {
    #[allow(dead_code)]
    url: String,
}

impl FulcioClient {
    /// Construct a Fulcio client targeting the given URL.
    ///
    /// Production: `https://fulcio.sigstore.dev/api/v2/signingCert`.
    /// Tests: `http://127.0.0.1:<fake_fulcio port>` (C-S1-3 seam).
    pub fn new(url: String) -> Self {
        Self { url }
    }

    /// POST a CSR to Fulcio's `/api/v2/signingCert` and return the signed cert.
    pub async fn request_certificate(
        &self,
        _token: &OidcToken,
        _public_key_der: &[u8],
    ) -> Result<FulcioCertificate, SignErrorKind> {
        unimplemented!("FulcioClient::request_certificate — Phase 5 delegates to sigstore-rs")
    }
}
