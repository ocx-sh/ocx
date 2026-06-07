// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Sigstore bundle v0.3 JSON builder.
//!
//! Produces the canonical `application/vnd.dev.sigstore.bundle.v0.3+json`
//! payload: cert chain + signature bytes + Rekor SET. The bundle is the
//! referrer's payload layer (see [`super::pipeline::SignPipeline`]).
//!
//! Phase 1 stub — the builder and deserialization stubs are reached in
//! Phase 5c; targeted `#[allow(dead_code)]` on the two stub items keeps
//! the rest of the module under dead-code lint coverage.

use serde::{Deserialize, Serialize};

use super::error::SignErrorKind;
use super::fulcio::FulcioCertificate;
use super::rekor::RekorEntry;

/// Serialized Sigstore bundle v0.3 payload.
///
/// Carries the raw JSON bytes plus the digest of those bytes. Bytes are
/// pushed as a blob; the digest is referenced by the referrer manifest's
/// `layers[0]` entry.
///
/// Re-exported by the `sign` module as `pub use bundle::SignedBundle`.
#[derive(Debug, Clone)]
pub struct SignedBundle {
    /// Canonical JSON bytes of the bundle v0.3 document.
    pub bytes: Vec<u8>,
    /// SHA-256 digest of `bytes` as a descriptor-ready string.
    pub digest: String,
}

/// Bundle v0.3 builder — Phase 5c will implement the `with_*` chain.
#[derive(Debug, Default)]
#[allow(dead_code)] // Phase 5c consumer (builder chain wired with sigstore-rs)
struct BundleBuilder {
    // Private state filled by `with_*` methods in Phase 5c.
}

#[allow(dead_code)] // Phase 5c consumer (builder chain wired with sigstore-rs)
impl BundleBuilder {
    fn new() -> Self {
        Self::default()
    }

    fn with_certificate(self, _cert: FulcioCertificate) -> Self {
        unimplemented!("BundleBuilder::with_certificate — Phase 5 implementation")
    }

    fn with_signature(self, _signature: Vec<u8>) -> Self {
        unimplemented!("BundleBuilder::with_signature — Phase 5 implementation")
    }

    fn with_rekor_entry(self, _entry: RekorEntry) -> Self {
        unimplemented!("BundleBuilder::with_rekor_entry — Phase 5 implementation")
    }

    fn with_target_digest(self, _digest: &str) -> Self {
        unimplemented!("BundleBuilder::with_target_digest — Phase 5 implementation")
    }

    fn build(self) -> Result<SignedBundle, SignErrorKind> {
        unimplemented!("BundleBuilder::build — Phase 5 serializes per Sigstore bundle v0.3 spec")
    }
}

/// Maximum accepted size of a Sigstore bundle v0.3 payload, in bytes.
///
/// Bundles are dominated by certificate chains (~10 KB) and a Rekor SET
/// (~5 KB); 512 KiB leaves two orders of magnitude of headroom while
/// preventing a hostile referrer from forcing us to allocate hundreds of MB
/// of JSON before we can reject it. This check runs BEFORE
/// `serde_json::from_slice` so the attacker's bytes never hit the parser.
#[allow(dead_code)] // Phase 5c consumer (consumed by from_bytes which Phase 5c wires)
pub(crate) const MAX_BUNDLE_SIZE_BYTES: usize = 512 * 1024;

/// Raw Sigstore bundle v0.3 document (for deserialization + round-trip tests).
///
/// The structural fields are intentionally left as JSON values so Phase 1
/// scaffolding compiles without nailing down the internal shape — Phase 5
/// replaces the inner type with the real sigstore-rs bundle struct.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)] // Phase 5c consumer (deserialization path wired with sigstore-rs)
struct BundleV03 {
    #[serde(rename = "mediaType")]
    media_type: String,
    #[serde(rename = "verificationMaterial")]
    verification_material: serde_json::Value,
    #[serde(rename = "messageSignature")]
    message_signature: serde_json::Value,
}

#[allow(dead_code)] // Phase 5c consumer (bundle-deserialization path)
impl BundleV03 {
    pub(crate) fn from_bytes(bytes: &[u8]) -> Result<Self, SignErrorKind> {
        if bytes.len() > MAX_BUNDLE_SIZE_BYTES {
            return Err(SignErrorKind::Internal(
                format!(
                    "bundle payload exceeds {} byte cap (got {})",
                    MAX_BUNDLE_SIZE_BYTES,
                    bytes.len()
                )
                .into(),
            ));
        }
        serde_json::from_slice::<Self>(bytes).map_err(|e| SignErrorKind::Internal(Box::new(e)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_bytes_rejects_oversized_payload() {
        // One byte over the cap — the guard condition is strictly `>`.
        let junk = vec![0xffu8; MAX_BUNDLE_SIZE_BYTES + 1];
        let result = BundleV03::from_bytes(&junk);
        assert!(
            matches!(result, Err(SignErrorKind::Internal(_))),
            "oversized payload must return Internal error, got: {result:?}"
        );
    }

    #[test]
    fn from_bytes_accepts_exactly_at_cap() {
        // The guard is `bytes.len() > MAX_BUNDLE_SIZE_BYTES`, so exactly-at-cap
        // must pass the size gate. Use invalid JSON so we don't depend on
        // constructing exactly-at-cap valid JSON — the assertion is that the
        // rejection path, if any, is a parse error, not a size error.
        let bytes = vec![b' '; MAX_BUNDLE_SIZE_BYTES];
        let result = BundleV03::from_bytes(&bytes);
        assert!(result.is_err(), "whitespace-only payload cannot parse as a bundle");
        let err_msg = format!("{:?}", result.as_ref().unwrap_err());
        assert!(
            !err_msg.contains("exceeds"),
            "exactly-at-cap payload must pass the size gate (got size-error: {err_msg})"
        );
    }

    #[test]
    fn from_bytes_round_trips_valid_bundle() {
        let bundle = BundleV03 {
            media_type: "application/vnd.dev.sigstore.bundle.v0.3+json".to_string(),
            verification_material: serde_json::Value::Null,
            message_signature: serde_json::Value::Null,
        };
        let bytes = serde_json::to_vec(&bundle).expect("serialization must succeed");
        let parsed = BundleV03::from_bytes(&bytes).expect("valid bundle must parse");
        assert_eq!(parsed.media_type, "application/vnd.dev.sigstore.bundle.v0.3+json");
    }
}
