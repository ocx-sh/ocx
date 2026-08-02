// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! TUF trust-root loader.
//!
//! [`TrustRoot`] holds a collection of DER-encoded X.509 CA certificates used to
//! verify the Fulcio certificate chain during signature verification, plus an
//! optional **pinned** Rekor public key (PEM). When present, the verify pipeline
//! uses the pinned key to check the Signed Entry Timestamp instead of fetching
//! it from the Rekor endpoint — closing the trust-on-first-use hole and making
//! offline verification possible (see `adr_offline_verify_trust_cache.md`).
//!
//! Construction paths:
//!
//! - [`TrustRoot::load_from_pem`] — load a PEM file of one or more `CERTIFICATE`
//!   blocks (Fulcio CA only; no Rekor key). Used by acceptance tests to inject
//!   the `fake_fulcio` self-signed root so the verify pipeline trusts test-minted
//!   certificates.
//! - [`TrustRoot::load_trusted_root_json`] — parse a Sigstore `TrustedRoot` JSON
//!   (the `--tuf-root` / `OCX_SIGSTORE_TUF_ROOT` seam): Fulcio CA certs **and**
//!   the pinned Rekor public key. No TUF network fetch — real TUF refresh is
//!   deferred.
//! - [`TrustRoot::from_der_and_rekor`] — rebuild a trust root from cached DER
//!   certs + a cached Rekor key PEM (the trust-root cache load path).
//! - [`TrustRoot::load_embedded`] — load the bundled production Sigstore trust
//!   root. Deferred to a future slice (requires shipping a TUF root bundle).

use super::error::{TrustRootLoadReason, VerifyErrorKind};

/// Validates that `der` is a well-formed DER SEQUENCE TLV whose declared
/// content length matches the byte slice exactly and is non-empty.
///
/// X.509 certificates are always DER-encoded `SEQUENCE` types (tag `0x30`)
/// whose body always carries at least a `tbsCertificate` inner SEQUENCE —
/// an empty top-level SEQUENCE is never a real certificate.
///
/// This minimal check rejects clearly malformed DER without requiring an
/// external ASN.1 parsing crate. It enforces:
///
/// 1. The outer tag is `0x30` (SEQUENCE).
/// 2. The DER length field is well-formed (short or `0x81` / `0x82` long form).
/// 3. The declared content length is non-zero (real certs are never empty).
/// 4. `header_len + content_len == der.len()` exactly — no trailing bytes
///    after the declared SEQUENCE end and no length overshoot.
///
/// A future slice will replace this with `x509-parser` once `sigstore-rs`
/// pulls it transitively; until then the strict equality + non-empty
/// invariants are the load-bearing guarantees.
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
    // Parse the DER length field and compute the header byte count in one
    // pass so the two stay in sync.
    let (content_len, length_field_len) = match rest.first() {
        None => return Err("invalid X.509 certificate in trust root: truncated length field".to_string()),
        Some(&b) if b < 0x80 => (b as usize, 1),
        Some(&0x81) => {
            let &len_byte = rest.get(1).ok_or_else(|| {
                "invalid X.509 certificate in trust root: truncated one-byte extended length".to_string()
            })?;
            (len_byte as usize, 2)
        }
        Some(&0x82) => {
            let hi = *rest.get(1).ok_or_else(|| {
                "invalid X.509 certificate in trust root: truncated two-byte extended length".to_string()
            })? as usize;
            let lo = *rest.get(2).ok_or_else(|| {
                "invalid X.509 certificate in trust root: truncated two-byte extended length".to_string()
            })? as usize;
            ((hi << 8) | lo, 3)
        }
        Some(&b) => {
            return Err(format!(
                "invalid X.509 certificate in trust root: unsupported DER length encoding byte {b:#04x}"
            ));
        }
    };
    // Real X.509 certs always carry at least a tbsCertificate SEQUENCE in
    // the outer SEQUENCE body. Empty SEQUENCE `[0x30, 0x00]` is structurally
    // valid DER but never a certificate.
    if content_len == 0 {
        return Err("invalid X.509 certificate in trust root: empty SEQUENCE body".to_string());
    }
    let header_len = 1 /* tag */ + length_field_len;
    let total_declared = header_len + content_len;
    // Strict equality: anything past the declared SEQUENCE end is garbage
    // (e.g. an extra `0xFF 0xFF` tail appended to a valid cert blob), and
    // anything short of `der.len()` would mean the declared length lies.
    if total_declared != der.len() {
        return Err(format!(
            "invalid X.509 certificate in trust root: declared length {total_declared} does not match actual {actual}",
            actual = der.len()
        ));
    }
    Ok(())
}

/// Sigstore trust root (Fulcio root certs + optional pinned Rekor public key).
///
/// Stores DER-encoded X.509 certificate bytes used as trust anchors for Fulcio
/// certificate chain validation, plus an optional Rekor public key (PEM). When
/// the Rekor key is present the verify pipeline pins it for SET verification
/// (no network); when absent the pipeline falls back to fetching it from the
/// Rekor endpoint (online-only).
#[derive(Debug)]
pub struct TrustRoot {
    /// DER-encoded X.509 certificates parsed from the PEM input.
    ///
    /// An empty `Vec` is valid at construction time and means "no trust anchors
    /// loaded" — the verify pipeline will reject all chains if the root is empty.
    der_certs: Vec<Vec<u8>>,

    /// Pinned Rekor public key (PEM), when the trust material carries one.
    ///
    /// `Some` for a `TrustedRoot` JSON or a cache load; `None` for a bare
    /// Fulcio-CA PEM, in which case the pipeline TOFU-fetches the key online.
    rekor_public_key_pem: Option<String>,
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
        let pem_str = std::str::from_utf8(pem_bytes).map_err(|_| {
            VerifyErrorKind::TrustRootLoad(TrustRootLoadReason::PemParseFailed {
                detail: "input is not valid UTF-8".to_string(),
            })
        })?;

        let parsed = pem::parse_many(pem_str).map_err(|e| {
            VerifyErrorKind::TrustRootLoad(TrustRootLoadReason::PemParseFailed {
                detail: format!("malformed PEM: {e}"),
            })
        })?;

        let mut der_certs: Vec<Vec<u8>> = Vec::new();
        for block in parsed.into_iter().filter(|b| b.tag() == "CERTIFICATE") {
            let bytes = block.contents().to_vec();
            validate_der_certificate(&bytes)
                .map_err(|detail| VerifyErrorKind::TrustRootLoad(TrustRootLoadReason::PemParseFailed { detail }))?;
            der_certs.push(bytes);
        }

        if der_certs.is_empty() {
            return Err(VerifyErrorKind::TrustRootLoad(TrustRootLoadReason::NoCertificateBlocks));
        }

        Ok(Self {
            der_certs,
            rekor_public_key_pem: None,
        })
    }

    /// Rebuild a trust root from cached DER certificates + a cached Rekor key.
    ///
    /// The trust-root cache load path. `der_certs` are the Fulcio CA certs; the
    /// Rekor key PEM is `Some` when the cache captured one (the offline path
    /// requires it). No validation beyond what the cache already round-tripped.
    pub fn from_der_and_rekor(der_certs: Vec<Vec<u8>>, rekor_public_key_pem: Option<String>) -> Self {
        Self {
            der_certs,
            rekor_public_key_pem,
        }
    }

    /// Parse a Sigstore [`TrustedRoot`][trusted-root] JSON document.
    ///
    /// Extracts the Fulcio CA certificate(s) from
    /// `certificateAuthorities[].certChain.certificates[].rawBytes` (base64 DER)
    /// and the pinned Rekor public key from `tlogs[].publicKey.rawBytes` (base64
    /// DER SubjectPublicKeyInfo, re-encoded to PEM). Parsed leniently via
    /// `serde_json` so both the public-good and fake-stack shapes load without a
    /// protobuf-JSON codec dependency.
    ///
    /// This is the `--tuf-root` / `OCX_SIGSTORE_TUF_ROOT` air-gapped seam. It does
    /// NOT perform any TUF network fetch or metadata-expiry verification — real
    /// TUF refresh is deferred.
    ///
    /// # Errors
    ///
    /// [`VerifyErrorKind::TrustRootLoad`] when the JSON is malformed, carries no
    /// certificate authorities, or a `rawBytes` field is not valid base64.
    ///
    /// [trusted-root]: https://github.com/sigstore/protobuf-specs/blob/main/protos/sigstore_trustroot.proto
    pub fn load_trusted_root_json(json_bytes: &[u8]) -> Result<Self, VerifyErrorKind> {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD;
        let load_err = |detail: String| VerifyErrorKind::TrustRootLoad(TrustRootLoadReason::PemParseFailed { detail });

        let root: serde_json::Value =
            serde_json::from_slice(json_bytes).map_err(|e| load_err(format!("invalid TrustedRoot JSON: {e}")))?;

        // Fulcio CA certificates: certificateAuthorities[].certChain.certificates[].rawBytes
        let mut der_certs: Vec<Vec<u8>> = Vec::new();
        if let Some(authorities) = root.get("certificateAuthorities").and_then(|v| v.as_array()) {
            for authority in authorities {
                let certificates = authority
                    .get("certChain")
                    .and_then(|c| c.get("certificates"))
                    .and_then(|c| c.as_array());
                for cert in certificates.into_iter().flatten() {
                    let Some(raw) = cert.get("rawBytes").and_then(|v| v.as_str()) else {
                        continue;
                    };
                    let der = b64
                        .decode(raw)
                        .map_err(|_| load_err("certificateAuthorities rawBytes is not valid base64".to_string()))?;
                    validate_der_certificate(&der).map_err(load_err)?;
                    der_certs.push(der);
                }
            }
        }
        if der_certs.is_empty() {
            return Err(VerifyErrorKind::TrustRootLoad(TrustRootLoadReason::NoCertificateBlocks));
        }

        // Rekor public key: first tlogs[].publicKey.rawBytes (DER SPKI) → PEM.
        // The verify pipeline loads it via `from_public_key_pem`, so re-encode
        // the DER as a `PUBLIC KEY` PEM block here.
        let rekor_public_key_pem = root
            .get("tlogs")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .find_map(|tlog| {
                tlog.get("publicKey")
                    .and_then(|k| k.get("rawBytes"))
                    .and_then(|v| v.as_str())
            })
            .map(|raw| {
                let der = b64
                    .decode(raw)
                    .map_err(|_| load_err("tlogs publicKey rawBytes is not valid base64".to_string()))?;
                Ok::<String, VerifyErrorKind>(pem::encode(&pem::Pem::new("PUBLIC KEY", der)))
            })
            .transpose()?;

        Ok(Self {
            der_certs,
            rekor_public_key_pem,
        })
    }

    /// Returns the DER-encoded certificates held by this trust root.
    pub fn der_certs(&self) -> &[Vec<u8>] {
        &self.der_certs
    }

    /// Returns the pinned Rekor public key (PEM), if this trust root carries one.
    ///
    /// `Some` means the verify pipeline pins this key for SET verification (no
    /// network); `None` means it TOFU-fetches the key from the Rekor endpoint.
    pub fn rekor_public_key_pem(&self) -> Option<&str> {
        self.rekor_public_key_pem.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A synthetic CERTIFICATE PEM block whose body is the smallest non-empty
    // DER SEQUENCE accepted by `validate_der_certificate`. It is NOT a real
    // X.509 certificate, but it passes the strict structural check: outer
    // tag = SEQUENCE, declared length matches actual, content non-empty.
    //
    // Decoded bytes: [0x30, 0x03, 0x02, 0x01, 0x01]
    //                SEQUENCE { INTEGER 1 }
    //                (tag=0x30, len=0x03, body = `02 01 01` = INTEGER value 1)
    // Base64 of [0x30, 0x03, 0x02, 0x01, 0x01] = "MAMCAQE="
    const TEST_CERT_PEM: &[u8] = b"\
-----BEGIN CERTIFICATE-----\n\
MAMCAQE=\n\
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

    // A synthetic CERTIFICATE PEM block whose body is an empty DER SEQUENCE
    // `[0x30, 0x00]` (structurally valid DER but never a real X.509 cert,
    // since real certs always carry at least a tbsCertificate body).
    // Base64 of [0x30, 0x00] = "MAA="
    const EMPTY_SEQUENCE_PEM: &[u8] = b"\
-----BEGIN CERTIFICATE-----\n\
MAA=\n\
-----END CERTIFICATE-----\n";

    // A synthetic CERTIFICATE PEM block whose body is a valid 5-byte SEQUENCE
    // followed by two `0xFF` trailing bytes — declared length is consistent
    // with the leading 5 bytes but the buffer is 7 bytes long. The strict
    // equality check must reject this as trailing garbage.
    //
    // Decoded bytes: [0x30, 0x03, 0x02, 0x01, 0x01, 0xFF, 0xFF]
    // Base64 of those 7 bytes = "MAMCAQH//w=="
    const TRAILING_GARBAGE_PEM: &[u8] = b"\
-----BEGIN CERTIFICATE-----\n\
MAMCAQH//w==\n\
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
            Err(VerifyErrorKind::TrustRootLoad(_)) => {} // expected
            other => panic!("expected TrustRootLoad, got: {other:?}"),
        }
    }

    #[test]
    fn load_from_pem_no_cert_blocks_returns_trust_root_load_error() {
        // Valid PEM but not a CERTIFICATE block.
        let non_cert = b"-----BEGIN PUBLIC KEY-----\nFAKE\n-----END PUBLIC KEY-----\n";
        let result = TrustRoot::load_from_pem(non_cert);
        match result {
            Err(VerifyErrorKind::TrustRootLoad(_)) => {} // expected
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
            Err(VerifyErrorKind::TrustRootLoad(TrustRootLoadReason::PemParseFailed { detail })) => {
                assert!(
                    detail.contains("invalid X.509 certificate in trust root"),
                    "detail should mention invalid X.509 certificate: {detail}"
                );
            }
            other => panic!("expected TrustRootLoad(PemParseFailed), got: {other:?}"),
        }
    }

    #[test]
    fn pem_parser_rejects_empty_sequence() {
        // `[0x30, 0x00]` is structurally valid DER (SEQUENCE with declared
        // length 0) but never a real X.509 certificate — real certs always
        // carry a tbsCertificate body. The strict parser must reject it.
        let result = TrustRoot::load_from_pem(EMPTY_SEQUENCE_PEM);
        match result {
            Err(VerifyErrorKind::TrustRootLoad(TrustRootLoadReason::PemParseFailed { detail })) => {
                assert!(
                    detail.contains("empty SEQUENCE body"),
                    "detail should call out empty SEQUENCE body: {detail}"
                );
            }
            other => panic!("expected TrustRootLoad(PemParseFailed), got: {other:?}"),
        }
    }

    #[test]
    fn pem_parser_rejects_trailing_garbage() {
        // A well-formed 5-byte SEQUENCE followed by two `0xFF` trailing bytes
        // (declared total = 5, actual buffer = 7). The previous parser
        // accepted this because it only checked `declared <= actual`. The
        // strict parser must reject it because trailing bytes after the
        // declared SEQUENCE end are not valid DER.
        let result = TrustRoot::load_from_pem(TRAILING_GARBAGE_PEM);
        match result {
            Err(VerifyErrorKind::TrustRootLoad(TrustRootLoadReason::PemParseFailed { detail })) => {
                assert!(
                    detail.contains("does not match actual"),
                    "detail should call out length mismatch: {detail}"
                );
            }
            other => panic!("expected TrustRootLoad(PemParseFailed), got: {other:?}"),
        }
    }

    // A minimal Sigstore TrustedRoot JSON: one Fulcio CA cert (the same
    // structurally-valid DER as TEST_CERT_PEM, base64 "MAMCAQE=") and one tlog
    // public key (arbitrary DER — `load_trusted_root_json` does not validate the
    // Rekor key body, only re-encodes it to PEM). Base64 of [0x30,0x00] = "MAA=".
    const TRUSTED_ROOT_JSON: &[u8] = br#"{
      "certificateAuthorities": [
        { "certChain": { "certificates": [ { "rawBytes": "MAMCAQE=" } ] } }
      ],
      "tlogs": [
        { "publicKey": { "rawBytes": "MAA=" } }
      ]
    }"#;

    #[test]
    fn load_trusted_root_json_extracts_fulcio_and_pinned_rekor_key() {
        let tr = TrustRoot::load_trusted_root_json(TRUSTED_ROOT_JSON).expect("valid trusted root JSON");
        assert_eq!(tr.der_certs().len(), 1, "one Fulcio CA cert");
        assert_eq!(tr.der_certs()[0], vec![0x30, 0x03, 0x02, 0x01, 0x01]);
        let pem = tr.rekor_public_key_pem().expect("Rekor key pinned");
        assert!(pem.contains("BEGIN PUBLIC KEY"), "Rekor key re-encoded to PEM: {pem}");
    }

    #[test]
    fn load_trusted_root_json_without_tlogs_pins_no_rekor_key() {
        let json =
            br#"{ "certificateAuthorities": [ { "certChain": { "certificates": [ { "rawBytes": "MAMCAQE=" } ] } } ] }"#;
        let tr = TrustRoot::load_trusted_root_json(json).expect("valid");
        assert!(tr.rekor_public_key_pem().is_none(), "no tlogs → no pinned Rekor key");
    }

    #[test]
    fn load_trusted_root_json_rejects_no_authorities() {
        // Structurally valid JSON but no certificate authorities → NoCertificateBlocks.
        let json = br#"{ "tlogs": [] }"#;
        match TrustRoot::load_trusted_root_json(json) {
            Err(VerifyErrorKind::TrustRootLoad(TrustRootLoadReason::NoCertificateBlocks)) => {}
            other => panic!("expected NoCertificateBlocks, got: {other:?}"),
        }
    }

    #[test]
    fn load_trusted_root_json_rejects_malformed_json() {
        match TrustRoot::load_trusted_root_json(b"not json at all") {
            Err(VerifyErrorKind::TrustRootLoad(TrustRootLoadReason::PemParseFailed { .. })) => {}
            other => panic!("expected PemParseFailed, got: {other:?}"),
        }
    }

    #[test]
    fn load_trusted_root_json_rejects_invalid_fulcio_der() {
        // A CA rawBytes whose decoded body is not a valid X.509 SEQUENCE.
        // Base64 "b2N4" decodes to "ocx" (0x6f...), not tag 0x30.
        let json =
            br#"{ "certificateAuthorities": [ { "certChain": { "certificates": [ { "rawBytes": "b2N4" } ] } } ] }"#;
        match TrustRoot::load_trusted_root_json(json) {
            Err(VerifyErrorKind::TrustRootLoad(TrustRootLoadReason::PemParseFailed { detail })) => {
                assert!(detail.contains("invalid X.509 certificate"), "detail: {detail}");
            }
            other => panic!("expected PemParseFailed for bad DER, got: {other:?}"),
        }
    }

    #[test]
    fn from_der_and_rekor_carries_both() {
        let tr = TrustRoot::from_der_and_rekor(vec![vec![0x30, 0x03, 0x02, 0x01, 0x01]], Some("PINNED".to_string()));
        assert_eq!(tr.der_certs().len(), 1);
        assert_eq!(tr.rekor_public_key_pem(), Some("PINNED"));
    }

    #[test]
    fn load_from_pem_pins_no_rekor_key() {
        let tr = TrustRoot::load_from_pem(TEST_CERT_PEM).expect("valid PEM");
        assert!(
            tr.rekor_public_key_pem().is_none(),
            "a bare Fulcio PEM pins no Rekor key"
        );
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
