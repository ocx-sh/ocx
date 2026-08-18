// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Rekor v1 transparency-log client — `POST /api/v1/log/entries`.
//!
//! Uploads a `hashedrekord:0.0.1` entry for the signature and returns the log
//! entry plus its Signed Entry Timestamp (SET). Rekor v2 (RFC 3161 TSA) is not
//! supported — deferred to #107 pending a sigstore-rs v2 client
//! (`research_sigstore_rs_spike.md`).
//!
//! The wire calls are hand-rolled (`reqwest`); nothing about the *verification*
//! format is ours — the SET and the Merkle proof are checked by `sigstore-rs`
//! (`oci::verify::tlog`).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sigstore::rekor::models::log_entry::RekorInclusionProof;
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
    /// The canonicalized log-entry body **as Rekor persisted it** — the `body`
    /// field of the log-entry response, not the proposal bytes we uploaded.
    /// Rekor re-canonicalizes the entry it stores, so the two differ in key
    /// order; the SET is signed over, and the Merkle leaf hashed from, Rekor's
    /// bytes. Embedded in the bundle's `canonicalizedBody`.
    pub(super) canonicalized_body: Vec<u8>,
    /// The Merkle inclusion proof and its signed checkpoint, when the log
    /// returned one inline. Rekor v1 does for an integrated entry; a log that
    /// omits it leaves the bundle with the SET as its only evidence.
    pub(super) inclusion_proof: Option<RekorInclusionProof>,
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
    /// Base64 of the canonical entry body Rekor persisted.
    body: String,
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
    /// Deserialized into `sigstore-rs`'s own API struct so the hex and Signed
    /// Note decoding stays that crate's code all the way to verification.
    #[serde(rename = "inclusionProof")]
    inclusion_proof: Option<RekorInclusionProof>,
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
        let body = serde_json::to_vec(&proposal).map_err(|e| SignErrorKind::Internal(Box::new(e)))?;

        let endpoint = self
            .url
            .join("api/v1/log/entries")
            .map_err(|e| SignErrorKind::Internal(Box::new(e)))?;
        let response = crate::oci::endpoint::sigstore_http_client()
            .post(endpoint)
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await
            .map_err(|_| SignErrorKind::RekorUnavailable)?;

        let status = response.status();
        if !status.is_success() {
            return Err(classify_upload_status(status));
        }

        // Capped: a log entry is kilobytes, and nothing in the Rekor API bounds
        // what the endpoint actually returns.
        let raw = crate::oci::endpoint::read_body_capped(response)
            .await
            .ok_or(SignErrorKind::RekorSetMalformed)?;
        parse_upload_response(&raw)
    }
}

/// Which failure a non-2xx upload response is.
///
/// The two outcomes carry different exit codes and opposite remediations —
/// `RekorUnavailable` is exit 83 and means retry, `RekorSetMalformed` is exit
/// 65 and means file a bug — so classifying a throttle as a malformed request
/// tells an operator to report a bug for a log that was merely busy. 429 is
/// grouped with 5xx deliberately: it is not a server error by status class,
/// but it is the same "come back later" for the caller.
///
/// Split out of [`RekorClient::upload_entry`] so the classification is
/// reachable without an HTTP round trip (ARCH-12).
fn classify_upload_status(status: reqwest::StatusCode) -> SignErrorKind {
    if status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        SignErrorKind::RekorUnavailable
    } else {
        SignErrorKind::RekorSetMalformed
    }
}

/// Decode a Rekor `POST /api/v1/log/entries` body into the entry it describes.
///
/// Every failure here is `RekorSetMalformed` rather than `RekorUnavailable`:
/// the log answered 2xx, so it is reachable, and what came back is unusable.
///
/// Split out of [`RekorClient::upload_entry`] so the decoding is reachable
/// from a test over already-fetched bytes (ARCH-12) — the base64 arms are
/// otherwise only exercised by a live log that never emits them.
fn parse_upload_response(raw: &[u8]) -> Result<RekorEntry, SignErrorKind> {
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD;

    let entries: BTreeMap<String, RekorLogEntry> =
        serde_json::from_slice(raw).map_err(|_| SignErrorKind::RekorSetMalformed)?;
    // The response is a map keyed by entry UUID and we uploaded exactly one, so
    // the first value is that entry. An empty map is a 2xx that recorded
    // nothing — unusable, not retryable.
    let entry = entries.into_values().next().ok_or(SignErrorKind::RekorSetMalformed)?;
    let set = b64
        .decode(entry.verification.signed_entry_timestamp.as_bytes())
        .map_err(|_| SignErrorKind::RekorSetMalformed)?;
    // Rekor's own bytes, never ours — see `RekorEntry::canonicalized_body`.
    let canonicalized_body = b64
        .decode(entry.body.as_bytes())
        .map_err(|_| SignErrorKind::RekorSetMalformed)?;

    Ok(RekorEntry {
        log_index: entry.log_index,
        integrated_time: entry.integrated_time,
        log_id: entry.log_id,
        signed_entry_timestamp: set,
        canonicalized_body,
        inclusion_proof: entry.verification.inclusion_proof,
    })
}

#[cfg(test)]
mod tests {
    //! The two branching halves of an upload response, tested over values
    //! rather than over a live log. Separate named tests per case rather than
    //! `#[rstest] #[case(...)]` rows because `rstest` is not a workspace
    //! dependency; a `for` loop over an array would abort at the first failure
    //! and report one opaque name, which is the property TEST-04 protects.

    use base64::Engine as _;
    use reqwest::StatusCode;

    use super::*;

    fn b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    /// A well-formed single-entry upload response, with `set` and `body` as
    /// given so a test can substitute one field at a time.
    fn response(set: &str, body: &str, inclusion_proof: &str) -> Vec<u8> {
        format!(
            r#"{{"24296f...":{{"body":"{body}","logIndex":42,"integratedTime":1700000000,\
"logID":"c0d23d","verification":{{"signedEntryTimestamp":"{set}","inclusionProof":{inclusion_proof}}}}}}}"#
        )
        .replace("\\\n", "")
        .into_bytes()
    }

    fn well_formed() -> Vec<u8> {
        response(&b64(b"set-bytes"), &b64(br#"{"kind":"hashedrekord"}"#), "null")
    }

    // ── status classification ────────────────────────────────────────────────

    #[test]
    fn a_500_is_the_log_being_unavailable() {
        assert!(matches!(
            classify_upload_status(StatusCode::INTERNAL_SERVER_ERROR),
            SignErrorKind::RekorUnavailable
        ));
    }

    #[test]
    fn a_503_is_the_log_being_unavailable() {
        assert!(matches!(
            classify_upload_status(StatusCode::SERVICE_UNAVAILABLE),
            SignErrorKind::RekorUnavailable
        ));
    }

    #[test]
    fn a_429_is_the_log_being_unavailable_despite_not_being_a_server_error() {
        // The case the status class alone gets wrong: a throttled signer is
        // told to retry (exit 83), not to file a bug (exit 65).
        assert!(matches!(
            classify_upload_status(StatusCode::TOO_MANY_REQUESTS),
            SignErrorKind::RekorUnavailable
        ));
    }

    #[test]
    fn a_400_is_a_malformed_entry() {
        assert!(matches!(
            classify_upload_status(StatusCode::BAD_REQUEST),
            SignErrorKind::RekorSetMalformed
        ));
    }

    #[test]
    fn a_409_conflict_is_a_malformed_entry() {
        // Rekor answers 409 for a duplicate entry. It is not retryable and the
        // operator cannot fix it by waiting, so it must not read as unavailable.
        assert!(matches!(
            classify_upload_status(StatusCode::CONFLICT),
            SignErrorKind::RekorSetMalformed
        ));
    }

    #[test]
    fn a_422_is_a_malformed_entry() {
        assert!(matches!(
            classify_upload_status(StatusCode::UNPROCESSABLE_ENTITY),
            SignErrorKind::RekorSetMalformed
        ));
    }

    // ── response decoding ────────────────────────────────────────────────────

    #[test]
    fn a_well_formed_response_decodes_to_the_entry() {
        let entry = parse_upload_response(&well_formed()).expect("well-formed response decodes");
        assert_eq!(entry.log_index, 42);
        assert_eq!(entry.integrated_time, 1_700_000_000);
        assert_eq!(entry.log_id, "c0d23d");
        assert_eq!(entry.signed_entry_timestamp, b"set-bytes");
        assert_eq!(entry.canonicalized_body, br#"{"kind":"hashedrekord"}"#);
        assert!(entry.inclusion_proof.is_none(), "this fixture carries no proof");
    }

    #[test]
    fn an_empty_entry_map_is_malformed() {
        // A 2xx that recorded nothing. Reachable, so not `RekorUnavailable`.
        assert!(matches!(
            parse_upload_response(b"{}"),
            Err(SignErrorKind::RekorSetMalformed)
        ));
    }

    #[test]
    fn a_non_json_body_is_malformed() {
        assert!(matches!(
            parse_upload_response(b"<html>gateway</html>"),
            Err(SignErrorKind::RekorSetMalformed)
        ));
    }

    #[test]
    fn a_set_that_is_not_base64_is_malformed() {
        let raw = response("not-base64!!", &b64(br#"{"kind":"hashedrekord"}"#), "null");
        assert!(matches!(
            parse_upload_response(&raw),
            Err(SignErrorKind::RekorSetMalformed)
        ));
    }

    #[test]
    fn a_body_that_is_not_base64_is_malformed() {
        // Distinct from the SET case: the two decodes are separate `?` arms and
        // one can be dropped without the other's test noticing.
        let raw = response(&b64(b"set-bytes"), "not-base64!!", "null");
        assert!(matches!(
            parse_upload_response(&raw),
            Err(SignErrorKind::RekorSetMalformed)
        ));
    }

    #[test]
    fn a_response_missing_the_verification_object_is_malformed() {
        let raw = br#"{"24296f...":{"body":"e30=","logIndex":42,"integratedTime":1,"logID":"c0"}}"#;
        assert!(matches!(
            parse_upload_response(raw),
            Err(SignErrorKind::RekorSetMalformed)
        ));
    }
}
