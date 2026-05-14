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

#[cfg(test)]
mod tests {
    use std::panic;

    use super::*;

    // ── Stub-panic guards ────────────────────────────────────────────────────
    //
    // Both matchers below are synchronous, so `catch_unwind` is sufficient —
    // no Tokio runtime needed. The stubs never inspect `_cert_der`, so an empty
    // slice is a valid input for panic-detection purposes.

    /// Assert that [`IdentityMatcher::verify`] still panics with the Phase-5
    /// stub message.
    ///
    /// Mirrors `verify_pipeline_stub_is_unimplemented` in `pipeline.rs`.
    /// Protects against the stub being silently swapped for a no-op `Ok(())`
    /// before Phase 5c lands: if that happened, the three `#[ignore]`d xfail
    /// tests below would pass vacuously when `#[ignore]` is removed, pinning
    /// no contract whatsoever.  This guard forces the implementor to update
    /// both this test AND the xfail tests in a single pass.
    #[test]
    fn identity_matcher_verify_is_unimplemented() {
        let result = panic::catch_unwind(|| {
            let matcher = IdentityMatcher::new("test@example.com".to_string());
            let _ = matcher.verify(&[]);
        });
        let panic_payload = match result {
            Err(payload) => payload,
            Ok(_) => panic!("IdentityMatcher::verify must panic (Phase 5c stub) — returned Ok instead"),
        };
        let msg = panic_payload
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| panic_payload.downcast_ref::<String>().map(|s| s.as_str()))
            .unwrap_or("<non-string panic>");
        assert!(
            msg.contains("Phase 5"),
            "expected Phase-5 stub panic message, got: {msg}"
        );
    }

    /// Assert that [`IssuerMatcher::verify`] still panics with the Phase-5
    /// stub message.
    ///
    /// `IssuerMatcher` is a separate type from `IdentityMatcher` (SAN vs OIDC
    /// issuer OID extension) so it needs its own guard — the two stubs are
    /// independent and one could be implemented without the other.
    #[test]
    fn issuer_matcher_verify_is_unimplemented() {
        let result = panic::catch_unwind(|| {
            let matcher = IssuerMatcher::new("https://accounts.google.com".to_string());
            let _ = matcher.verify(&[]);
        });
        let panic_payload = match result {
            Err(payload) => payload,
            Ok(_) => panic!("IssuerMatcher::verify must panic (Phase 5c stub) — returned Ok instead"),
        };
        let msg = panic_payload
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| panic_payload.downcast_ref::<String>().map(|s| s.as_str()))
            .unwrap_or("<non-string panic>");
        assert!(
            msg.contains("Phase 5"),
            "expected Phase-5 stub panic message, got: {msg}"
        );
    }

    // ── xfail contract anchors (Phase 5c) ───────────────────────────────────
    //
    // The bodies below are commented implementation sketches. When Phase 5c
    // removes `#[ignore]`, the empty/sketch body will fail to compile (or
    // trivially pass), signalling the implementor to write the actual contract
    // assertion before flipping `#[ignore]` active.

    /// Exact byte-equal identity strings match (Phase 5c identity contract anchor).
    ///
    /// `IdentityMatcher::verify` MUST accept a cert whose SAN equals the expected
    /// string byte-for-byte. This is the positive-path contract: same bytes => Ok.
    #[test]
    #[ignore = "phase 5c — IdentityMatcher::verify is unimplemented; flip active when identity parsing lands"]
    fn identity_byte_equal_match() {
        // Phase 5c implementation sketch:
        //
        //   let identity = "test@example.com";
        //   let cert_der = build_test_cert_with_san(identity); // helper in test utils
        //   let matcher = IdentityMatcher::new(identity.to_string());
        //   assert!(matcher.verify(&cert_der).is_ok(), "byte-equal SAN must match");
    }

    /// Trailing-slash variant of a SAN MUST NOT match the untrailed form.
    ///
    /// `https://accounts.google.com/` vs `https://accounts.google.com` are
    /// distinct strings. Phase 5c must perform byte-equal comparison only —
    /// no URL normalization, no path trimming. Mismatch => `IssuerMismatch`.
    #[test]
    #[ignore = "phase 5c — IdentityMatcher::verify is unimplemented; flip active when identity parsing lands"]
    fn identity_trailing_slash_rejected() {
        // Phase 5c implementation sketch:
        //
        //   let san_in_cert = "https://accounts.google.com/";
        //   let expected = "https://accounts.google.com";
        //   let cert_der = build_test_cert_with_san(san_in_cert);
        //   let matcher = IdentityMatcher::new(expected.to_string());
        //   assert!(
        //       matches!(matcher.verify(&cert_der), Err(VerifyErrorKind::IdentityMismatch)),
        //       "trailing-slash SAN must not match untrailed expected string"
        //   );
    }

    /// Mixed-case domain in SAN MUST NOT match the canonical lowercase form.
    ///
    /// `Github.com` vs `github.com` differ; Phase 5c performs no Unicode or
    /// case canonicalization at v1. Byte-equal only. Mismatch => `IdentityMismatch`.
    #[test]
    #[ignore = "phase 5c — IdentityMatcher::verify is unimplemented; flip active when identity parsing lands"]
    fn identity_mixed_case_domain_rejected() {
        // Phase 5c implementation sketch:
        //
        //   let san_in_cert = "https://Github.com/actions/runner";
        //   let expected = "https://github.com/actions/runner";
        //   let cert_der = build_test_cert_with_san(san_in_cert);
        //   let matcher = IdentityMatcher::new(expected.to_string());
        //   assert!(
        //       matches!(matcher.verify(&cert_der), Err(VerifyErrorKind::IdentityMismatch)),
        //       "mixed-case domain in SAN must not match lowercase expected string"
        //   );
    }
}
