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

#[cfg(test)]
mod tests {
    //! Skeleton tests for the TrustRoot loaders.
    //!
    //! Both constructors are Phase 1 stubs (`unimplemented!()`). The tests
    //! below lock in the *error type contract* — the functions must return
    //! `Result<Self, VerifyErrorKind>` — and act as executable TODO markers
    //! that flip to real assertions once Phase 5 ships.
    use super::*;

    #[test]
    #[should_panic(expected = "not implemented")]
    fn load_embedded_is_phase_1_stub() {
        // Phase 5 will return Ok(_) for the stock Sigstore root; until then,
        // the stub panics. When Phase 5 lands, convert this to
        //   let tr = TrustRoot::load_embedded().expect("embedded root loads");
        let _ = TrustRoot::load_embedded();
    }

    #[test]
    #[should_panic(expected = "not implemented")]
    fn load_from_pem_is_phase_1_stub() {
        // Phase 5 parses PEM and builds trust material. Stub panics today.
        let dummy_pem = b"-----BEGIN CERTIFICATE-----\nFAKE\n-----END CERTIFICATE-----\n";
        let _ = TrustRoot::load_from_pem(dummy_pem);
    }

    #[test]
    fn load_from_pem_signature_returns_verify_error_kind() {
        // Type-level check: the error channel must be VerifyErrorKind so
        // `ClassifyErrorKind::exit_code()` is reachable when the TrustRoot
        // loader fails. Captures the Result type at compile time via
        // assignment to a variable of the declared shape.
        let _fn: fn(&[u8]) -> Result<TrustRoot, VerifyErrorKind> = TrustRoot::load_from_pem;
        let _fn2: fn() -> Result<TrustRoot, VerifyErrorKind> = TrustRoot::load_embedded;
    }
}
