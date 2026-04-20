// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! TUF trust-root loader.
//!
//! Wraps `sigstore-trust-root = 0.6.4` to embed the stock Sigstore root at
//! build time. No BYO-trust in Slice 1 (see ADR Risks — v2 adds flag-based
//! trust-policy files).
//!
//! The [`TrustRoot`] type is injected into [`super::pipeline::VerifyContext`]
//! per C-S1-3: production callers use [`TrustRoot::load_embedded`]; tests
//! build a fake trust root from `fake_fulcio`'s self-signed root.
//!
//! Phase 1 stub — bodies use `unimplemented!()`.

use super::error::VerifyErrorKind;

/// Sigstore trust root (Fulcio root certs + Rekor public keys).
pub struct TrustRoot {
    // Inner state populated by `load_*` constructors in Phase 5.
}

impl TrustRoot {
    /// Load the embedded production Sigstore trust root.
    ///
    /// Fails with [`VerifyErrorKind::TrustRootUnavailable`] if the embedded
    /// asset is missing (build-time regression) or TUF verification fails.
    pub fn load_embedded() -> Result<Self, VerifyErrorKind> {
        unimplemented!("TrustRoot::load_embedded — Phase 5 wraps sigstore-trust-root")
    }

    /// Load a trust root from a PEM file (test-only injection path).
    ///
    /// Used by acceptance tests to inject the `fake_fulcio` self-signed root
    /// so the verify pipeline trusts test-minted certificates.
    pub fn load_from_pem(_pem_bytes: &[u8]) -> Result<Self, VerifyErrorKind> {
        unimplemented!("TrustRoot::load_from_pem — Phase 5 parses PEM and builds trust material")
    }
}
