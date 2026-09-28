// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Rekor v1 transparency-log client — `POST /api/v1/log/entries`.
//!
//! Rekor v2 is not supported: ocx-sh/ocx#107.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sigstore::rekor::models::log_entry::RekorInclusionProof;
use url::Url;

use super::error::SignErrorKind;
use crate::attest::TLOG_KIND_WRITTEN;

/// A Rekor log entry with its Signed Entry Timestamp.
pub(super) struct RekorEntry {
    pub(super) log_index: u64,
    pub(super) integrated_time: u64,
    pub(super) log_id: String,
    pub(super) signed_entry_timestamp: Vec<u8>,
    /// The log-entry body as Rekor persisted it, never our proposal bytes: Rekor
    /// re-canonicalizes, and the SET and Merkle leaf cover its bytes.
    pub(super) canonicalized_body: Vec<u8>,
    /// The Merkle inclusion proof, when the log returned one inline.
    pub(super) inclusion_proof: Option<RekorInclusionProof>,
}

/// Rekor v1 client.
pub(super) struct RekorClient {
    url: Url,
}

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

#[derive(Serialize)]
struct DsseProposal<'a> {
    kind: &'a str,
    #[serde(rename = "apiVersion")]
    api_version: &'a str,
    spec: DsseSpec<'a>,
}

#[derive(Serialize)]
struct DsseSpec<'a> {
    #[serde(rename = "proposedContent")]
    proposed_content: DsseProposedContent<'a>,
}

#[derive(Serialize)]
struct DsseProposedContent<'a> {
    /// A JSON string, not a nested object: the log hashes exactly the bytes the signer produced.
    envelope: &'a str,
    verifiers: [String; 1],
}

#[derive(Deserialize)]
struct RekorLogEntry {
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
    #[serde(rename = "inclusionProof")]
    inclusion_proof: Option<RekorInclusionProof>,
}

impl RekorClient {
    pub(super) fn new(url: Url) -> Self {
        Self { url }
    }

    /// Upload a `hashedrekord` entry for `signature_der` over `payload_digest_hex` (lowercase hex SHA-256),
    /// returning the log entry + SET.
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
        self.post_proposal(body).await
    }

    /// Upload a `dsse:0.0.1` entry for `envelope_json`, returning the log entry.
    ///
    /// `envelope_json` must be the exact serialized envelope, never a re-serialization of a parsed one.
    pub(super) async fn upload_dsse_entry(
        &self,
        envelope_json: &[u8],
        leaf_pem: &str,
    ) -> Result<RekorEntry, SignErrorKind> {
        let envelope = std::str::from_utf8(envelope_json).map_err(|e| SignErrorKind::Internal(Box::new(e)))?;
        self.post_proposal(dsse_proposal_body(envelope, leaf_pem)?).await
    }

    /// POST a proposed entry and decode the log's answer.
    async fn post_proposal(&self, body: Vec<u8>) -> Result<RekorEntry, SignErrorKind> {
        let endpoint = self
            .url
            .join("api/v1/log/entries")
            .map_err(|e| SignErrorKind::Internal(Box::new(e)))?;
        let response = ocx_oci::endpoint::sigstore_http_client()
            .post(endpoint)
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await
            .map_err(|_| SignErrorKind::TransparencyLogUnavailable)?;

        let status = response.status();
        if !status.is_success() {
            return Err(classify_upload_status(status));
        }

        // Capped: nothing in the Rekor API bounds what the endpoint returns.
        let raw = ocx_oci::endpoint::read_body_capped(response)
            .await
            .ok_or(SignErrorKind::RekorSetMalformed)?;
        parse_upload_response(&raw)
    }
}

/// The `dsse:0.0.1` proposed-entry body for `envelope_json` and `leaf_pem`.
fn dsse_proposal_body(envelope_json: &str, leaf_pem: &str) -> Result<Vec<u8>, SignErrorKind> {
    use base64::Engine as _;
    let (kind, api_version) = TLOG_KIND_WRITTEN;

    let proposal = DsseProposal {
        kind,
        api_version,
        spec: DsseSpec {
            proposed_content: DsseProposedContent {
                envelope: envelope_json,
                verifiers: [base64::engine::general_purpose::STANDARD.encode(leaf_pem.as_bytes())],
            },
        },
    };
    serde_json::to_vec(&proposal).map_err(|e| SignErrorKind::Internal(Box::new(e)))
}

/// Which failure a non-2xx upload response is.
///
/// 429 groups with 5xx as retryable, or a throttled log sends the operator to file a bug.
fn classify_upload_status(status: reqwest::StatusCode) -> SignErrorKind {
    if status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        SignErrorKind::TransparencyLogUnavailable
    } else {
        SignErrorKind::RekorSetMalformed
    }
}

/// Decode a Rekor upload response; every failure is `RekorSetMalformed`, as the log answered 2xx.
fn parse_upload_response(raw: &[u8]) -> Result<RekorEntry, SignErrorKind> {
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD;

    let entries: BTreeMap<String, RekorLogEntry> =
        serde_json::from_slice(raw).map_err(|_| SignErrorKind::RekorSetMalformed)?;
    // Keyed by entry UUID; we uploaded one, and an empty map recorded nothing.
    let entry = entries.into_values().next().ok_or(SignErrorKind::RekorSetMalformed)?;
    let set = b64
        .decode(entry.verification.signed_entry_timestamp.as_bytes())
        .map_err(|_| SignErrorKind::RekorSetMalformed)?;
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
            SignErrorKind::TransparencyLogUnavailable
        ));
    }

    #[test]
    fn a_503_is_the_log_being_unavailable() {
        assert!(matches!(
            classify_upload_status(StatusCode::SERVICE_UNAVAILABLE),
            SignErrorKind::TransparencyLogUnavailable
        ));
    }

    #[test]
    fn a_429_is_the_log_being_unavailable_despite_not_being_a_server_error() {
        // The case the status class alone gets wrong: a throttled signer is
        // told to retry (exit 83), not to file a bug (exit 65).
        assert!(matches!(
            classify_upload_status(StatusCode::TOO_MANY_REQUESTS),
            SignErrorKind::TransparencyLogUnavailable
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
        // A 2xx that recorded nothing. Reachable, so not `TransparencyLogUnavailable`.
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

    // ── dsse:0.0.1 proposal ──────────────────────────────────────────────────

    #[test]
    fn a_dsse_proposal_is_the_pinned_wire_shape() {
        // Pinned as a literal, not rebuilt from the inputs: every field name
        // here is a spelling rekor matches exactly, and a body assembled by the
        // same code that the assertion re-derives would agree with itself no
        // matter which spelling it used. `envelope` is the stringified envelope
        // JSON — a JSON string, not a nested object — and `verifiers` is a
        // one-element array of the base64 leaf PEM.
        let body = dsse_proposal_body(r#"{"payload":"cA==","payloadType":"t","signatures":[]}"#, "PEM")
            .expect("a well-formed envelope encodes");
        assert_eq!(
            String::from_utf8(body).expect("the proposal is UTF-8 JSON"),
            concat!(
                r#"{"kind":"dsse","apiVersion":"0.0.1","spec":{"proposedContent":{"#,
                r#""envelope":"{\"payload\":\"cA==\",\"payloadType\":\"t\",\"signatures\":[]}","#,
                r#""verifiers":["UEVN"]}}}"#
            )
        );
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
