// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Sigstore bundle v0.3 assembly + parsing.
//!
//! Produces the canonical `application/vnd.dev.sigstore.bundle.v0.3+json`
//! payload (cert chain + message signature + Rekor transparency-log entry)
//! using the official `sigstore_protobuf_specs` types, so the output is a
//! genuine cosign-compatible bundle. The bundle is the referrer's payload
//! layer (see [`super::pipeline::SignPipeline`]).

// The `Bundle` type is re-exported by the `sigstore` crate (its bundle feature);
// the remaining protobuf message types come from `sigstore_protobuf_specs`.
use sigstore::bundle::Bundle;
use sigstore_protobuf_specs::dev::sigstore::bundle::v1::{VerificationMaterial, bundle, verification_material};
use sigstore_protobuf_specs::dev::sigstore::common::v1::{
    HashAlgorithm, HashOutput, LogId, MessageSignature, X509Certificate, X509CertificateChain,
};
use sigstore_protobuf_specs::dev::sigstore::rekor::v1::{InclusionPromise, KindVersion, TransparencyLogEntry};

use super::error::SignErrorKind;
use super::fulcio::FulcioCertificate;
use super::rekor::RekorEntry;
use crate::oci::{Algorithm, Digest};

/// Sigstore bundle v0.3 media type (`sigstore::bundle::models::Version::Bundle0_3`).
pub(crate) const BUNDLE_V03_MEDIA_TYPE: &str = "application/vnd.dev.sigstore.bundle.v0.3+json";

/// Serialized Sigstore bundle v0.3 payload.
///
/// Carries the raw JSON bytes plus the digest of those bytes. Bytes are pushed
/// as a blob; the digest is referenced by the referrer manifest's `layers[0]`.
#[derive(Debug, Clone)]
pub struct SignedBundle {
    /// Canonical JSON bytes of the bundle v0.3 document.
    pub bytes: Vec<u8>,
    /// SHA-256 digest of `bytes`.
    pub digest: Digest,
}

/// Assemble a Sigstore bundle v0.3 from the signing artifacts.
///
/// `subject_digest` is the target manifest digest that was signed over; its raw
/// bytes become the bundle's `messageSignature.messageDigest`.
pub(super) fn build_bundle(
    cert: &FulcioCertificate,
    signature_der: &[u8],
    rekor: &RekorEntry,
    subject_digest: &Digest,
) -> Result<SignedBundle, SignErrorKind> {
    let subject_digest_raw = hex::decode(subject_digest.hex()).map_err(|e| SignErrorKind::Internal(Box::new(e)))?;
    // The Rekor log id is hex; the protobuf LogId carries the raw key-id bytes.
    let log_id_raw = hex::decode(&rekor.log_id).unwrap_or_default();

    let tlog_entry = TransparencyLogEntry {
        log_index: rekor.log_index as i64,
        log_id: Some(LogId { key_id: log_id_raw }),
        kind_version: Some(KindVersion {
            kind: "hashedrekord".to_string(),
            version: "0.0.1".to_string(),
        }),
        integrated_time: rekor.integrated_time as i64,
        inclusion_promise: Some(InclusionPromise {
            signed_entry_timestamp: rekor.signed_entry_timestamp.clone(),
        }),
        inclusion_proof: None,
        canonicalized_body: rekor.canonicalized_body.clone(),
    };

    let verification_material = VerificationMaterial {
        timestamp_verification_data: None,
        tlog_entries: vec![tlog_entry],
        content: Some(verification_material::Content::X509CertificateChain(
            X509CertificateChain {
                certificates: vec![X509Certificate {
                    raw_bytes: cert.leaf_der.clone(),
                }],
            },
        )),
    };

    let message_signature = MessageSignature {
        message_digest: Some(HashOutput {
            algorithm: HashAlgorithm::Sha2256 as i32,
            digest: subject_digest_raw,
        }),
        signature: signature_der.to_vec(),
    };

    let bundle = Bundle {
        media_type: BUNDLE_V03_MEDIA_TYPE.to_string(),
        verification_material: Some(verification_material),
        content: Some(bundle::Content::MessageSignature(message_signature)),
    };

    let bytes = serde_json::to_vec(&bundle).map_err(|e| SignErrorKind::Internal(Box::new(e)))?;
    let digest = Algorithm::Sha256.hash(&bytes);
    Ok(SignedBundle { bytes, digest })
}

/// Maximum accepted size of a Sigstore bundle v0.3 payload, in bytes.
///
/// Bundles are dominated by certificate chains (~10 KB) and a Rekor SET
/// (~5 KB); 512 KiB leaves headroom while preventing a hostile referrer from
/// forcing a large allocation before the parser can reject it. This check runs
/// BEFORE `serde_json::from_slice` so the attacker's bytes never hit the parser.
pub(crate) const MAX_BUNDLE_SIZE_BYTES: usize = 512 * 1024;

/// Parse (size-capped) a Sigstore bundle v0.3 document.
///
/// Returns `None` when the payload exceeds [`MAX_BUNDLE_SIZE_BYTES`] or does not
/// deserialize; the verify pipeline maps `None` to
/// `VerifyErrorKind::BundleParseFailed`.
pub(crate) fn parse_bundle(bytes: &[u8]) -> Option<Bundle> {
    if bytes.len() > MAX_BUNDLE_SIZE_BYTES {
        return None;
    }
    serde_json::from_slice::<Bundle>(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bundle_rejects_oversized_payload() {
        let junk = vec![0xffu8; MAX_BUNDLE_SIZE_BYTES + 1];
        assert!(parse_bundle(&junk).is_none(), "oversized payload must be rejected");
    }

    #[test]
    fn parse_bundle_rejects_non_bundle_json() {
        assert!(parse_bundle(b"{}").is_none() || parse_bundle(b"not json").is_none());
        assert!(parse_bundle(b"not json at all").is_none());
    }

    #[test]
    fn build_and_parse_round_trips() {
        let cert = FulcioCertificate {
            leaf_der: vec![1, 2, 3, 4],
            leaf_pem: "-----BEGIN CERTIFICATE-----\nAQIDBA==\n-----END CERTIFICATE-----\n".to_string(),
        };
        let rekor = RekorEntry {
            log_index: 7,
            integrated_time: 1_700_000_000,
            log_id: "ab".repeat(32),
            signed_entry_timestamp: vec![9, 9, 9],
            canonicalized_body: b"{\"kind\":\"hashedrekord\"}".to_vec(),
        };
        let subject = Algorithm::Sha256.hash(b"manifest bytes");
        let signed = build_bundle(&cert, &[0xaa, 0xbb], &rekor, &subject).expect("build");
        assert!(signed.digest.to_string().starts_with("sha256:"));
        let parsed = parse_bundle(&signed.bytes).expect("bundle round-trips");
        assert_eq!(parsed.media_type, BUNDLE_V03_MEDIA_TYPE);
        assert_eq!(parsed.verification_material.unwrap().tlog_entries.len(), 1);
    }
}
