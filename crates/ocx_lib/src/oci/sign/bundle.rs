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
    HashAlgorithm, HashOutput, LogId, MessageSignature, X509Certificate,
};
use sigstore_protobuf_specs::dev::sigstore::rekor::v1::{
    Checkpoint, InclusionPromise, InclusionProof, KindVersion, TransparencyLogEntry,
};

use x509_cert::Certificate as X509Cert;
use x509_cert::der::Decode as _;

use super::error::SignErrorKind;
use super::fulcio::FulcioCertificate;
use super::rekor::RekorEntry;
use crate::oci::verify::identity::{oidc_issuer, subject_identity};
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
    /// SubjectAltName of the issued Fulcio leaf — the identity a verifier
    /// matches, read back off the certificate rather than off the token that
    /// bought it. Fulcio derives the SAN from the `email` claim for a human
    /// identity, so the token's `sub` (an opaque provider id for dex, Google
    /// and every other email provider) names something no `--certificate-identity`
    /// and no `[[trust.policy]]` will ever match. Empty when the leaf carries
    /// no SAN, which is a certificate the verify side rejects anyway.
    pub certificate_identity: String,
    /// OIDC issuer from the leaf's Fulcio issuer extension (`.1.8`), for the
    /// same reason: the certificate is what verification reads.
    pub certificate_oidc_issuer: String,
}

/// Convert Rekor's API-shaped inclusion proof into the bundle's protobuf form.
///
/// Returns `None` when any hex field is malformed. The caller turns that into a
/// hard sign failure: ocx's verifier requires the inclusion proof, so shipping a
/// bundle without one would publish an artifact this tool cannot verify.
fn proto_inclusion_proof(api: &sigstore::rekor::models::log_entry::RekorInclusionProof) -> Option<InclusionProof> {
    let hashes: Option<Vec<Vec<u8>>> = api.hashes.iter().map(|h| hex::decode(h).ok()).collect();
    Some(InclusionProof {
        log_index: api.log_index,
        root_hash: hex::decode(&api.root_hash).ok()?,
        tree_size: i64::try_from(api.tree_size).ok()?,
        hashes: hashes?,
        checkpoint: Some(Checkpoint {
            envelope: api.checkpoint.clone(),
        }),
    })
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
        // Mandatory, not best-effort: the verifier refuses a bundle carrying no
        // inclusion proof (`VerifyErrorKind::RekorInclusionProofAbsent`), so a
        // log that returns no usable proof must fail the sign rather than
        // publish an unverifiable artifact. Exit 83 — retrying may help.
        inclusion_proof: Some(
            rekor
                .inclusion_proof
                .as_ref()
                .and_then(proto_inclusion_proof)
                .ok_or(SignErrorKind::RekorUnavailable)?,
        ),
        canonicalized_body: rekor.canonicalized_body.clone(),
    };

    let verification_material = VerificationMaterial {
        timestamp_verification_data: None,
        tlog_entries: vec![tlog_entry],
        // `certificate`, not `x509CertificateChain`: bundle v0.3 replaced the
        // chain field with a single leaf, and a verifier that enforces the
        // profile refuses a document carrying the older shape under the newer
        // media type. Fulcio's intermediates come from the trust root, so the
        // leaf is all a chain could have carried anyway.
        content: Some(verification_material::Content::Certificate(X509Certificate {
            raw_bytes: cert.leaf_der.clone(),
        })),
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

    // Report what the verifier will read. The extractors are the verify side's
    // own, so the two commands cannot drift into disagreeing about one cert.
    let leaf = X509Cert::from_der(&cert.leaf_der).ok();
    let certificate_identity = leaf.as_ref().and_then(subject_identity).unwrap_or_default();
    let certificate_oidc_issuer = leaf.as_ref().and_then(oidc_issuer).unwrap_or_default();

    Ok(SignedBundle {
        bytes,
        digest,
        certificate_identity,
        certificate_oidc_issuer,
    })
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

    fn test_certificate() -> FulcioCertificate {
        FulcioCertificate {
            leaf_der: vec![1, 2, 3, 4],
            leaf_pem: "-----BEGIN CERTIFICATE-----\nAQIDBA==\n-----END CERTIFICATE-----\n".to_string(),
        }
    }

    fn test_entry(inclusion_proof: Option<sigstore::rekor::models::log_entry::RekorInclusionProof>) -> RekorEntry {
        RekorEntry {
            log_index: 7,
            integrated_time: 1_700_000_000,
            log_id: "ab".repeat(32),
            signed_entry_timestamp: vec![9, 9, 9],
            canonicalized_body: b"{\"kind\":\"hashedrekord\"}".to_vec(),
            inclusion_proof,
        }
    }

    fn test_proof() -> sigstore::rekor::models::log_entry::RekorInclusionProof {
        serde_json::from_str(
            r#"{"logIndex":7,"rootHash":"aa","treeSize":8,
                "hashes":["bb","cc"],"checkpoint":"envelope"}"#,
        )
        .expect("proof fixture parses")
    }

    #[test]
    fn build_refuses_an_entry_with_no_inclusion_proof() {
        // The verifier requires the Merkle proof; publishing a bundle without
        // one would ship an artifact ocx itself cannot verify. Exit 83.
        let subject = Algorithm::Sha256.hash(b"manifest bytes");
        let err = build_bundle(&test_certificate(), &[0xaa], &test_entry(None), &subject)
            .expect_err("a proofless Rekor entry must not produce a bundle");
        assert!(
            matches!(err, SignErrorKind::RekorUnavailable),
            "expected RekorUnavailable, got: {err:?}"
        );
    }

    #[test]
    fn build_refuses_a_proof_whose_hex_fields_are_malformed() {
        // `proto_inclusion_proof` drops an unparseable proof; that drop must
        // reach the same refusal, not silently publish a proofless bundle.
        let mut proof = test_proof();
        proof.root_hash = "not hex".into();
        let subject = Algorithm::Sha256.hash(b"manifest bytes");
        let err = build_bundle(&test_certificate(), &[0xaa], &test_entry(Some(proof)), &subject)
            .expect_err("an unencodable proof must not produce a bundle");
        assert!(
            matches!(err, SignErrorKind::RekorUnavailable),
            "expected RekorUnavailable, got: {err:?}"
        );
    }

    #[test]
    fn build_and_parse_round_trips() {
        let cert = test_certificate();
        let rekor = test_entry(Some(test_proof()));
        let subject = Algorithm::Sha256.hash(b"manifest bytes");
        let signed = build_bundle(&cert, &[0xaa, 0xbb], &rekor, &subject).expect("build");
        assert!(signed.digest.to_string().starts_with("sha256:"));
        let parsed = parse_bundle(&signed.bytes).expect("bundle round-trips");
        assert_eq!(parsed.media_type, BUNDLE_V03_MEDIA_TYPE);
        assert_eq!(parsed.verification_material.unwrap().tlog_entries.len(), 1);
    }
}
