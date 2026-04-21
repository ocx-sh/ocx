// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fulcio CSR client — `/api/v2/signingCert`.
//!
//! Thin wrapper over `sigstore::fulcio::FulcioClient`, routed to the v2
//! endpoint (`request_cert_v2`) rather than the deprecated v1 path. Emits
//! typed errors on HTTP failures — 401/403 map to
//! [`SignErrorKind::OidcTokenRejected`] / [`SignErrorKind::FulcioBadRequest`].
//!
//! Phase 1 stub — bodies use `unimplemented!()`. Dead-code lint suppressed
//! because this module is private; Phase 5c wires the call sites.
#![allow(dead_code)]

use super::error::SignErrorKind;
use super::oidc::OidcToken;

/// Fulcio-issued signing certificate with chain.
///
/// Used by [`super::bundle::BundleBuilder`] in Phase 5c.
pub(super) struct FulcioCertificate {
    pub(super) leaf: Vec<u8>,
    pub(super) chain: Vec<Vec<u8>>,
}

/// Fulcio client — Phase 5c implements the CSR round-trip.
struct FulcioClient {
    /// C-S1-3 injection seam: `https://fulcio.sigstore.dev/api/v2/signingCert`
    /// in production; `http://127.0.0.1:<port>` in tests.
    url: String,
}

impl FulcioClient {
    fn new(url: String) -> Self {
        Self { url }
    }

    async fn request_certificate(
        &self,
        _token: &OidcToken,
        _public_key_der: &[u8],
    ) -> Result<FulcioCertificate, SignErrorKind> {
        unimplemented!("FulcioClient::request_certificate — Phase 5 delegates to sigstore-rs")
    }
}
