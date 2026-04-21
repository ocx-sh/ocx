// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! TUF trust-root loader.
//!
//! [`TrustRoot`] holds a collection of DER-encoded X.509 CA certificates
//! used to verify the Fulcio certificate chain during signature verification.
//!
//! Two construction paths:
//!
//! - [`TrustRoot::load_from_pem`] — load a PEM file containing one or more
//!   `CERTIFICATE` blocks. Used by acceptance tests to inject the
//!   `fake_fulcio` self-signed root so the verify pipeline trusts test-minted
//!   certificates.
//! - [`TrustRoot::load_embedded`] — load the bundled production Sigstore trust
//!   root. Deferred to a future slice (requires shipping a TUF root bundle).

use super::error::VerifyErrorKind;

/// Validates that `der` begins with a well-formed DER SEQUENCE TLV header and
/// that the declared content length is consistent with the actual byte length.
///
/// X.509 certificates are always DER-encoded `SEQUENCE` types (tag `0x30`).
/// This minimal check rejects clearly malformed DER (wrong tag, truncated
/// length encoding, or declared length overshooting the actual bytes) without
/// requiring an external ASN.1 parsing crate.
///
/// Returns `Err(reason)` with a lowercase message when validation fails.
fn validate_der_certificate(der: &[u8]) -> Result<(), String> {
    // Minimum: tag byte + at least one length byte.
    let Some((&tag, rest)) = der.split_first() else {
        return Err("invalid X.509 certificate in trust root: empty DER bytes".to_string());
    };
    // X.509 Certificate outer structure is a SEQUENCE (tag 0x30).
    if tag != 0x30 {
        return Err(format!(
            "invalid X.509 certificate in trust root: expected SEQUENCE tag 0x30, got {tag:#04x}"
        ));
    }
    // Parse the DER length field.
    let content_len = match rest.first() {
        None => return Err("invalid X.509 certificate in trust root: truncated length field".to_string()),
        Some(&b) if b < 0x80 => b as usize,
        Some(&0x81) => {
            let &len_byte = rest.get(1).ok_or_else(|| {
                "invalid X.509 certificate in trust root: truncated one-byte extended length".to_string()
            })?;
            len_byte as usize
        }
        Some(&0x82) => {
            let hi = *rest.get(1).ok_or_else(|| {
                "invalid X.509 certificate in trust root: truncated two-byte extended length".to_string()
            })? as usize;
            let lo = *rest.get(2).ok_or_else(|| {
                "invalid X.509 certificate in trust root: truncated two-byte extended length".to_string()
            })? as usize;
            (hi << 8) | lo
        }
        Some(&b) => {
            return Err(format!(
                "invalid X.509 certificate in trust root: unsupported DER length encoding byte {b:#04x}"
            ));
        }
    };
    // Compute the number of bytes consumed by tag + length field.
    let header_len = 1 /* tag */ + match rest.first() {
        Some(&b) if b < 0x80 => 1,
        Some(&0x81) => 2,
        Some(&0x82) => 3,
        _ => 1,
    };
    let total_declared = header_len + content_len;
    if total_declared > der.len() {
        return Err(format!(
            "invalid X.509 certificate in trust root: declared length {total_declared} exceeds actual {actual}",
            actual = der.len()
        ));
    }
    Ok(())
}

/// Sigstore trust root (Fulcio root certs + Rekor public keys).
///
/// Internally stores DER-encoded X.509 certificate bytes. The verify pipeline
/// uses these as trust anchors for Fulcio certificate chain validation.
#[derive(Debug)]
pub struct TrustRoot {
    /// DER-encoded X.509 certificates parsed from the PEM input.
    ///
    /// An empty `Vec` is valid at construction time and means "no trust anchors
    /// loaded" — the verify pipeline will reject all chains if the root is empty.
    pub(super) der_certs: Vec<Vec<u8>>,
}

impl TrustRoot {
    /// Load the embedded production Sigstore trust root.
    ///
    /// Fails with [`VerifyErrorKind::TrustRootUnavailable`] — shipping a
    /// bundled TUF root is deferred to a future slice.
    pub fn load_embedded() -> Result<Self, VerifyErrorKind> {
        Err(VerifyErrorKind::TrustRootUnavailable)
    }

    /// Load a trust root from PEM-encoded bytes.
    ///
    /// Parses all `CERTIFICATE` PEM blocks in `pem_bytes`. At least one valid
    /// certificate block is required; an empty or all-non-certificate input
    /// returns [`VerifyErrorKind::TrustRootLoad`].
    ///
    /// # Errors
    ///
    /// Returns [`VerifyErrorKind::TrustRootLoad`] when:
    /// - `pem_bytes` contains no valid `CERTIFICATE` PEM blocks
    /// - `pem_bytes` is empty
    /// - The PEM data is malformed (invalid base64, truncated blocks)
    pub fn load_from_pem(pem_bytes: &[u8]) -> Result<Self, VerifyErrorKind> {
        let pem_str = std::str::from_utf8(pem_bytes).map_err(|_| VerifyErrorKind::TrustRootLoad {
            reason: "PEM input is not valid UTF-8".to_string(),
        })?;

        let parsed = pem::parse_many(pem_str).map_err(|e| VerifyErrorKind::TrustRootLoad {
            reason: format!("malformed PEM: {e}"),
        })?;

        let mut der_certs: Vec<Vec<u8>> = Vec::new();
        for block in parsed.into_iter().filter(|b| b.tag() == "CERTIFICATE") {
            let bytes = block.contents().to_vec();
            validate_der_certificate(&bytes).map_err(|reason| VerifyErrorKind::TrustRootLoad { reason })?;
            der_certs.push(bytes);
        }

        if der_certs.is_empty() {
            return Err(VerifyErrorKind::TrustRootLoad {
                reason: "no CERTIFICATE blocks found in PEM input".to_string(),
            });
        }

        Ok(Self { der_certs })
    }

    /// Returns the DER-encoded certificates held by this trust root.
    pub fn der_certs(&self) -> &[Vec<u8>] {
        &self.der_certs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A synthetic CERTIFICATE PEM block whose body is a minimal valid DER
    // SEQUENCE (tag=0x30, length=0x00). This is the smallest structurally
    // valid DER value accepted by `validate_der_certificate`. It is NOT a
    // real X.509 certificate, but it passes the DER structural check.
    //
    // Decoded bytes: [0x30, 0x00]  (SEQUENCE { } — empty SEQUENCE)
    // Base64 of [0x30, 0x00] = "MAA="
    const TEST_CERT_PEM: &[u8] = b"\
-----BEGIN CERTIFICATE-----\n\
MAA=\n\
-----END CERTIFICATE-----\n";

    // A synthetic PEM block whose DER body is NOT a SEQUENCE (starts with
    // 0x6F = ASCII 'o'). Used to verify that `validate_der_certificate`
    // rejects malformed DER inside a CERTIFICATE block.
    //
    // Decoded bytes: b"ocx test trust root placeholder der bytes"
    const MALFORMED_DER_PEM: &[u8] = b"\
-----BEGIN CERTIFICATE-----\n\
b2N4IHRlc3QgdHJ1c3Qgcm9vdCBwbGFjZWhvbGRlciBkZXIgYnl0ZXM=\n\
-----END CERTIFICATE-----\n";

    #[test]
    fn load_from_pem_succeeds_with_valid_certificate() {
        let tr = TrustRoot::load_from_pem(TEST_CERT_PEM).expect("valid PEM must parse");
        assert_eq!(tr.der_certs().len(), 1, "exactly one cert block");
        assert!(!tr.der_certs()[0].is_empty(), "DER bytes must be non-empty");
    }

    #[test]
    fn load_from_pem_loads_multiple_certificates() {
        // Two concatenated CERTIFICATE blocks.
        let two_certs = [TEST_CERT_PEM, TEST_CERT_PEM].concat();
        let tr = TrustRoot::load_from_pem(&two_certs).expect("two certs must parse");
        assert_eq!(tr.der_certs().len(), 2, "two cert blocks expected");
    }

    #[test]
    fn load_from_pem_empty_returns_trust_root_load_error() {
        let result = TrustRoot::load_from_pem(b"");
        match result {
            Err(VerifyErrorKind::TrustRootLoad { .. }) => {} // expected
            other => panic!("expected TrustRootLoad, got: {other:?}"),
        }
    }

    #[test]
    fn load_from_pem_no_cert_blocks_returns_trust_root_load_error() {
        // Valid PEM but not a CERTIFICATE block.
        let non_cert = b"-----BEGIN PUBLIC KEY-----\nFAKE\n-----END PUBLIC KEY-----\n";
        let result = TrustRoot::load_from_pem(non_cert);
        match result {
            Err(VerifyErrorKind::TrustRootLoad { .. }) => {} // expected
            other => panic!("expected TrustRootLoad, got: {other:?}"),
        }
    }

    #[test]
    fn load_from_pem_rejects_malformed_der_inside_certificate_block() {
        // A CERTIFICATE PEM block whose DER body is not a valid SEQUENCE.
        // `validate_der_certificate` must reject it with TrustRootLoad, not
        // silently accept it as a cert.
        let result = TrustRoot::load_from_pem(MALFORMED_DER_PEM);
        match result {
            Err(VerifyErrorKind::TrustRootLoad { reason }) => {
                assert!(
                    reason.contains("invalid X.509 certificate in trust root"),
                    "reason should mention invalid X.509 certificate: {reason}"
                );
            }
            other => panic!("expected TrustRootLoad for malformed DER, got: {other:?}"),
        }
    }

    #[test]
    fn load_embedded_returns_trust_root_unavailable() {
        // load_embedded is deferred; it must return TrustRootUnavailable, not panic.
        let result = TrustRoot::load_embedded();
        assert!(
            matches!(result, Err(VerifyErrorKind::TrustRootUnavailable)),
            "expected TrustRootUnavailable, got: {result:?}"
        );
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
