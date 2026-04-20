// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Certificate identity + issuer matchers.
//!
//! Implements exact-match for `--certificate-identity` (SAN) and
//! `--certificate-oidc-issuer` (Fulcio OIDC issuer OID extension). Parses
//! the Fulcio-standard X.509 extension OIDs.
//!
//! Phase 1 stub — bodies use `unimplemented!()`.

use super::error::VerifyErrorKind;

/// Certificate SAN matcher.
pub struct IdentityMatcher {
    #[allow(dead_code)]
    expected: String,
}

impl IdentityMatcher {
    /// Construct a matcher for the given expected identity string.
    pub fn new(expected: String) -> Self {
        Self { expected }
    }

    /// Verify that the certificate's SAN equals the expected identity.
    ///
    /// Returns [`VerifyErrorKind::IdentityMismatch`] on mismatch.
    pub fn verify(&self, _cert_der: &[u8]) -> Result<(), VerifyErrorKind> {
        unimplemented!("IdentityMatcher::verify — Phase 5 parses SAN from cert extensions")
    }
}

/// Certificate issuer matcher (Fulcio OIDC-issuer OID extension).
pub struct IssuerMatcher {
    #[allow(dead_code)]
    expected: String,
}

impl IssuerMatcher {
    /// Construct a matcher for the given expected issuer URL.
    pub fn new(expected: String) -> Self {
        Self { expected }
    }

    /// Verify that the certificate's issuer extension equals the expected URL.
    ///
    /// Returns [`VerifyErrorKind::IssuerMismatch`] on mismatch.
    pub fn verify(&self, _cert_der: &[u8]) -> Result<(), VerifyErrorKind> {
        unimplemented!("IssuerMatcher::verify — Phase 5 parses OIDC issuer OID extension")
    }
}
