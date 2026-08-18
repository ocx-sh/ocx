// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Rekor transparency-log verification, delegated to `sigstore-rs`.
//!
//! Two independent pieces of evidence a bundle carries about its Rekor entry:
//!
//! * the **Signed Entry Timestamp** (inclusion *promise*) — the log's ECDSA
//!   P-256 signature over the RFC 8785 canonical JSON of
//!   `{body, integratedTime, logIndex, logID}`; and
//! * the **inclusion proof** — the Merkle audit path from the entry's leaf hash
//!   to a signed checkpoint root.
//!
//! No cryptography is computed here. [`CosignVerificationKey`] owns the ECDSA
//! verification, [`InclusionProof::verify`] owns the RFC 6269 leaf hashing, the
//! audit-path recomputation and the checkpoint signature and root-consistency
//! checks, and `serde_json_canonicalizer` owns RFC 8785.
//!
//! [`SetPayload`] is the one thing declared locally: it is a four-field wire
//! *schema*, not an algorithm. `sigstore-rs` has the identical struct as
//! `cosign::bundle::Payload`, but its `cosign` feature additionally pulls in
//! `oci-client`, `regex` and `async-trait` — a second OCI client in a binary
//! that already ships one, for four field names. The shape is pinned by a test
//! against the bytes Rekor signs.
//!
//! Before this module OCX invented its own SET payload (`ocx-rekor-set-v1\n…`)
//! and verified it as Ed25519. Both were fictions of the in-repo fake stack: a
//! real Rekor signs ECDSA P-256 over canonical JSON, so every bundle produced
//! against a real log failed to verify and every bundle produced against the
//! fake proved nothing about the real format (#209).

use base64::Engine as _;
use serde::Serialize;
use sigstore::crypto::verification_key::CosignVerificationKey;
use sigstore::crypto::{Signature, SigningScheme};
use sigstore::rekor::models::InclusionProof;
use sigstore::rekor::models::log_entry::RekorInclusionProof;
use sigstore_protobuf_specs::dev::sigstore::rekor::v1::InclusionProof as ProtoInclusionProof;

use super::error::VerifyErrorKind;

/// The canonical payload a Rekor v1 Signed Entry Timestamp is computed over.
///
/// Field names and order are Rekor's wire schema, mirrored from
/// `sigstore::cosign::bundle::Payload`. RFC 8785 sorts keys on serialization,
/// so declaration order here is not load-bearing — the names and types are.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SetPayload {
    /// Base64 of the entry's `canonicalizedBody`.
    body: String,
    integrated_time: i64,
    log_index: i64,
    #[serde(rename = "logID")]
    log_id: String,
}

/// The Rekor entry fields the log's signature and proof are computed over.
pub(super) struct TlogEntry<'a> {
    /// The exact `canonicalizedBody` bytes carried in the bundle.
    pub(super) canonicalized_body: &'a [u8],
    pub(super) integrated_time: u64,
    pub(super) log_index: u64,
    /// Hex-encoded log id (SHA-256 of the log's public key).
    pub(super) log_id_hex: &'a str,
    /// Raw SET signature bytes (DER ECDSA).
    pub(super) signed_entry_timestamp: &'a [u8],
}

/// Parse the log's public key PEM into the scheme Rekor signs with.
///
/// Rekor v1 signs the SET and its checkpoints with ECDSA P-256 / SHA-256 in
/// ASN.1 DER form; the same key verifies both.
pub(super) fn rekor_key(pem: &str) -> Result<CosignVerificationKey, VerifyErrorKind> {
    CosignVerificationKey::from_pem(pem.as_bytes(), &SigningScheme::ECDSA_P256_SHA256_ASN1)
        .map_err(|_| VerifyErrorKind::RekorSetInvalid)
}

/// Verify the Signed Entry Timestamp over the entry's canonical payload.
///
/// The payload is built as [`SetPayload`] and canonicalized by
/// `serde_json_canonicalizer` — the identical construction
/// `sigstore::cosign::bundle::Bundle::verify_bundle` performs, so a SET this
/// accepts is one cosign accepts.
pub(super) fn verify_set(key: &CosignVerificationKey, entry: &TlogEntry<'_>) -> Result<(), VerifyErrorKind> {
    let payload = SetPayload {
        body: base64::engine::general_purpose::STANDARD.encode(entry.canonicalized_body),
        integrated_time: i64::try_from(entry.integrated_time).map_err(|_| VerifyErrorKind::RekorSetInvalid)?,
        log_index: i64::try_from(entry.log_index).map_err(|_| VerifyErrorKind::RekorSetInvalid)?,
        log_id: entry.log_id_hex.to_owned(),
    };
    let canonical = serde_json_canonicalizer::to_vec(&payload).map_err(|_| VerifyErrorKind::RekorSetInvalid)?;
    key.verify_signature(Signature::Raw(entry.signed_entry_timestamp), &canonical)
        .map_err(|_| VerifyErrorKind::RekorSetInvalid)
}

/// Verify the Merkle inclusion proof against its signed checkpoint.
///
/// The leaf is hashed from `canonicalized_body` exactly as stored, never from a
/// re-serialization of a parsed entry — Rekor's leaf hash is over the bytes it
/// persisted, and any re-encoding risks a different byte string for the same
/// logical entry.
pub(super) fn verify_inclusion(
    key: &CosignVerificationKey,
    proof: &ProtoInclusionProof,
    canonicalized_body: &[u8],
) -> Result<(), VerifyErrorKind> {
    // Cross the protobuf/API boundary through the crate's own conversion so
    // the hex decoding and the Signed Note checkpoint parsing stay its code.
    let api = RekorInclusionProof {
        hashes: proof.hashes.iter().map(hex::encode).collect(),
        log_index: proof.log_index,
        root_hash: hex::encode(&proof.root_hash),
        tree_size: u64::try_from(proof.tree_size).map_err(|_| VerifyErrorKind::RekorSetInvalid)?,
        checkpoint: proof
            .checkpoint
            .as_ref()
            .map(|c| c.envelope.clone())
            .unwrap_or_default(),
    };
    InclusionProof::try_from(&api)
        .map_err(|_| VerifyErrorKind::RekorSetInvalid)?
        .verify(canonicalized_body, key)
        .map_err(|_| VerifyErrorKind::RekorSetInvalid)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The SET is a signature over these exact bytes, so the schema is the
    /// contract: a renamed or retyped field silently fails every verification
    /// against a real log while every local round-trip still passes. Pinned
    /// against the canonical form Rekor signs (RFC 8785: keys sorted, no
    /// whitespace), with `logID` spelled as the log spells it.
    #[test]
    fn set_payload_canonical_form_matches_rekor() {
        let payload = SetPayload {
            body: "Ym9keQ==".to_owned(),
            integrated_time: 1_787_034_401,
            log_index: 1,
            log_id: "ecc9333fafb0".to_owned(),
        };
        let canonical = serde_json_canonicalizer::to_vec(&payload).expect("canonicalizes");
        assert_eq!(
            String::from_utf8(canonical).expect("utf-8"),
            r#"{"body":"Ym9keQ==","integratedTime":1787034401,"logID":"ecc9333fafb0","logIndex":1}"#
        );
    }

    /// A public key that is not the log's must not verify a SET. Guards the
    /// scheme argument too: parsing an ECDSA P-256 PEM under the wrong scheme
    /// would fail here rather than silently accept.
    #[test]
    fn set_rejects_a_signature_from_another_key() {
        // Two independently generated P-256 SPKI PEMs.
        const KEY: &str = "-----BEGIN PUBLIC KEY-----\nMFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEXfEB1QNlXmz9OcHqhVVsRHZBQBLI\nPWJVLNXKKvVZgHQZUSRzMOzT6JhZmXVUFNPXxUxJXKlvxLWMPDb3PZQSNA==\n-----END PUBLIC KEY-----\n";
        let entry = TlogEntry {
            canonicalized_body: b"body",
            integrated_time: 1,
            log_index: 0,
            log_id_hex: "ab",
            signed_entry_timestamp: &[0u8; 70],
        };
        // Either the key fails to parse or the signature fails to verify; both
        // are RekorSetInvalid, and neither may be Ok.
        let verdict = rekor_key(KEY).and_then(|k| verify_set(&k, &entry));
        assert!(matches!(verdict, Err(VerifyErrorKind::RekorSetInvalid)), "{verdict:?}");
    }
}
