// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Rekor v1 transparency-log client — `POST /api/v1/log/entries`.
//!
//! Uploads a `hashedrekord:0.0.1` entry for the signature and returns the log
//! entry plus its Signed Entry Timestamp (SET). Rekor v2 (RFC 3161 TSA) is not
//! supported — deferred to #107 pending a sigstore-rs v2 client
//! (`research_sigstore_rs_spike.md`).
//!
//! The wire calls are hand-rolled (`reqwest`) rather than routed through
//! `sigstore::rekor` so the SET verification format stays under OCX's control
//! on both the sign and verify sides — see [`set_signing_payload`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use url::Url;

use super::error::SignErrorKind;

/// A Rekor log entry with its Signed Entry Timestamp.
pub(super) struct RekorEntry {
    pub(super) log_index: u64,
    pub(super) integrated_time: u64,
    /// Hex-encoded Rekor log identifier (SHA-256 of the log's public key).
    pub(super) log_id: String,
    /// The Signed Entry Timestamp (raw signature bytes).
    pub(super) signed_entry_timestamp: Vec<u8>,
    /// The canonicalized log-entry body (the exact `hashedrekord` proposal
    /// bytes that were uploaded). Embedded in the bundle's `canonicalizedBody`.
    pub(super) canonicalized_body: Vec<u8>,
}

/// Rekor v1 client.
pub(super) struct RekorClient {
    url: Url,
}

// ── hashedrekord proposal (uploaded body) ────────────────────────────────────

#[derive(Serialize)]
struct HashedRekordProposal<'a> {
    kind: &'a str,
    #[serde(rename = "apiVersion")]
    api_version: &'a str,
    spec: HashedRekordSpec<'a>,
}

#[derive(Serialize)]
struct HashedRekordSpec<'a> {
    signature: RekorSignature<'a>,
    data: RekorData<'a>,
}

#[derive(Serialize)]
struct RekorSignature<'a> {
    content: &'a str,
    #[serde(rename = "publicKey")]
    public_key: RekorPublicKey<'a>,
}

#[derive(Serialize)]
struct RekorPublicKey<'a> {
    content: &'a str,
}

#[derive(Serialize)]
struct RekorData<'a> {
    hash: RekorHash<'a>,
}

#[derive(Serialize)]
struct RekorHash<'a> {
    algorithm: &'a str,
    value: &'a str,
}

// ── Rekor v1 response (subset) ───────────────────────────────────────────────

#[derive(Deserialize)]
struct RekorLogEntry {
    #[serde(rename = "logIndex")]
    log_index: u64,
    #[serde(rename = "integratedTime")]
    integrated_time: u64,
    #[serde(rename = "logID")]
    log_id: String,
    verification: RekorVerification,
}

#[derive(Deserialize)]
struct RekorVerification {
    #[serde(rename = "signedEntryTimestamp")]
    signed_entry_timestamp: String,
}

impl RekorClient {
    pub(super) fn new(url: Url) -> Self {
        Self { url }
    }

    /// Upload a `hashedrekord` entry for `signature` over `payload_digest_hex`,
    /// returning the log entry + SET.
    ///
    /// `signature_der` is the DER-encoded ECDSA signature; `cert_pem` is the
    /// leaf certificate PEM; `payload_digest_hex` is the lowercase hex of the
    /// SHA-256 subject digest.
    pub(super) async fn upload_entry(
        &self,
        signature_der: &[u8],
        cert_pem: &str,
        payload_digest_hex: &str,
    ) -> Result<RekorEntry, SignErrorKind> {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD;
        let sig_b64 = b64.encode(signature_der);
        let cert_b64 = b64.encode(cert_pem.as_bytes());

        let proposal = HashedRekordProposal {
            kind: "hashedrekord",
            api_version: "0.0.1",
            spec: HashedRekordSpec {
                signature: RekorSignature {
                    content: &sig_b64,
                    public_key: RekorPublicKey { content: &cert_b64 },
                },
                data: RekorData {
                    hash: RekorHash {
                        algorithm: "sha256",
                        value: payload_digest_hex,
                    },
                },
            },
        };
        // Serialize once so the uploaded bytes and the embedded canonicalized
        // body are byte-identical.
        let body = serde_json::to_vec(&proposal).map_err(|e| SignErrorKind::Internal(Box::new(e)))?;

        let endpoint = self
            .url
            .join("api/v1/log/entries")
            .map_err(|e| SignErrorKind::Internal(Box::new(e)))?;
        let response = reqwest::Client::new()
            .post(endpoint)
            .header("Content-Type", "application/json")
            .body(body.clone())
            .send()
            .await
            .map_err(|_| SignErrorKind::RekorUnavailable)?;

        let status = response.status();
        if !status.is_success() {
            // 5xx / 429 → the log is unavailable; other failures are malformed
            // requests we cannot recover from.
            return Err(if status.is_server_error() || status.as_u16() == 429 {
                SignErrorKind::RekorUnavailable
            } else {
                SignErrorKind::RekorSetMalformed
            });
        }

        let entries: BTreeMap<String, RekorLogEntry> =
            response.json().await.map_err(|_| SignErrorKind::RekorSetMalformed)?;
        let entry = entries.into_values().next().ok_or(SignErrorKind::RekorSetMalformed)?;
        let set = b64
            .decode(entry.verification.signed_entry_timestamp.as_bytes())
            .map_err(|_| SignErrorKind::RekorSetMalformed)?;

        Ok(RekorEntry {
            log_index: entry.log_index,
            integrated_time: entry.integrated_time,
            log_id: entry.log_id,
            signed_entry_timestamp: set,
            canonicalized_body: body,
        })
    }
}

/// Deterministic message the Rekor SET is computed over.
///
/// OCX controls this format on both the sign and verify sides (the offline fake
/// stack signs the identical bytes), so it is reproducible without JSON
/// canonicalization ambiguity. It binds the entry body to its inclusion
/// metadata (index, time, log id) so tampering with any of them breaks the SET.
///
/// NOTE: this is NOT the public-good Rekor v1 SET wire format; real-network
/// Rekor verification is out of #194 scope (a network-gated `#[ignore]` path).
pub(crate) fn set_signing_payload(
    canonicalized_body: &[u8],
    integrated_time: u64,
    log_index: u64,
    log_id_hex: &str,
) -> Vec<u8> {
    use base64::Engine as _;
    let body_b64 = base64::engine::general_purpose::STANDARD.encode(canonicalized_body);
    format!("ocx-rekor-set-v1\n{log_index}\n{integrated_time}\n{log_id_hex}\n{body_b64}").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::set_signing_payload;

    // The SET is Ed25519-signed over exactly these bytes on both the sign and
    // verify sides, so tamper detection reduces to this payload being
    // deterministic AND changing whenever any bound field changes. If it did
    // not, an altered Rekor entry would still verify.
    #[test]
    fn payload_is_deterministic() {
        let a = set_signing_payload(b"body", 100, 5, "ab");
        let b = set_signing_payload(b"body", 100, 5, "ab");
        assert_eq!(a, b);
    }

    #[test]
    fn payload_changes_when_any_bound_field_changes() {
        let base = set_signing_payload(b"body", 100, 5, "ab");
        assert_ne!(base, set_signing_payload(b"BODY", 100, 5, "ab"), "body");
        assert_ne!(base, set_signing_payload(b"body", 101, 5, "ab"), "integrated_time");
        assert_ne!(base, set_signing_payload(b"body", 100, 6, "ab"), "log_index");
        assert_ne!(base, set_signing_payload(b"body", 100, 5, "cd"), "log_id");
    }
}
