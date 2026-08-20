// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The DSSE envelope: PAE encoding, the wire shape, and the sign-side envelope
//! hashes.
//!
//! Nothing here verifies a signature. This module owns the *structural* half of
//! a DSSE envelope — the bytes a signature is computed over, and the checks that
//! must pass before a payload is worth parsing at all. The cryptographic half
//! lives in `oci/verify/dsse.rs`, which calls in here first.
//!
//! Items are `pub` rather than the ADR's `pub(crate)` for the reason
//! [`crate::oci::attest`] records against its constants: no in-crate caller
//! exists until the verify and sign pipelines land, and `pub(crate)` trips
//! `dead_code` under `warnings = "deny"` until one does. Each of those work
//! packages narrows what it consumes as it lands.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize, Serializer};

use crate::oci::attest::{DSSE_PAYLOAD_TYPE, MAX_STATEMENT_PAYLOAD_BYTES};
use crate::oci::verify::VerifyErrorKind;
use crate::oci::{Algorithm, Digest};

/// Pre-Authentication Encoding, per the DSSE spec.
///
/// `"DSSEv1" SP LEN(type) SP type SP LEN(body) SP body`, where `LEN` is the
/// ASCII decimal *byte* length. `payload` is the raw decoded payload — never
/// the base64 text (verifier checklist row 1).
pub(crate) fn pae(payload_type: &str, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload_type.len().saturating_add(payload.len()).saturating_add(32));
    out.extend_from_slice(b"DSSEv1 ");
    out.extend_from_slice(payload_type.len().to_string().as_bytes());
    out.push(b' ');
    out.extend_from_slice(payload_type.as_bytes());
    out.push(b' ');
    out.extend_from_slice(payload.len().to_string().as_bytes());
    out.push(b' ');
    out.extend_from_slice(payload);
    out
}

/// A DSSE envelope, with the payload held decoded.
///
/// Field order is fixed by declaration order and serde emits in that order, so
/// the same envelope always serializes to the same bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DsseEnvelope {
    /// The decoded payload. Base64 exists only at the serde boundary.
    pub payload: Vec<u8>,
    /// The declared payload type; what says how to read [`Self::payload`].
    pub payload_type: String,
    /// Exactly one signature on every envelope OCX writes or accepts.
    pub signatures: Vec<DsseSignature>,
}

/// One signature over the envelope's PAE.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DsseSignature {
    /// The raw signature bytes; base64 only at the serde boundary.
    pub sig: Vec<u8>,
    /// A lookup hint, never a security decision (checklist row 10).
    pub keyid: String,
}

impl DsseEnvelope {
    /// Parses and structurally validates an envelope.
    ///
    /// **Preconditions.** The caller owns the `MAX_ATTESTATION_ENVELOPE_BYTES`
    /// bound on `bytes` (checklist row 15) — this function is handed a slice
    /// and cannot see how it was read. The decoded-payload bound,
    /// `MAX_STATEMENT_PAYLOAD_BYTES` (row 16), is enforced here instead,
    /// because only this side knows the base64 length before the decode.
    ///
    /// # Errors
    ///
    /// Rejects malformed JSON, a `payloadType` other than the in-toto one, an
    /// over-cap payload, and any signature count other than one.
    pub(crate) fn parse(bytes: &[u8]) -> Result<Self, VerifyErrorKind> {
        let wire: WireEnvelope = serde_json::from_slice(bytes).map_err(|_| VerifyErrorKind::BundleParseFailed)?;

        // Row 3: the declared type is what says how to read the bytes, so it is
        // checked before anything reads them.
        if wire.payload_type != DSSE_PAYLOAD_TYPE {
            return Err(VerifyErrorKind::PayloadTypeUnsupported {
                payload_type: wire.payload_type,
            });
        }

        // Row 8: verifying one signature out of several would report
        // "verified" for an envelope whose others nobody checked.
        if wire.signatures.len() != 1 {
            return Err(VerifyErrorKind::MultipleSignatures {
                count: wire.signatures.len(),
            });
        }

        // Row 16: asserted from the base64 length, before the decode buffer
        // exists. Base64 expands at a fixed 4/3, so `len / 4 * 3` is the
        // decoded size's upper bound; refusing on that bound is conservative
        // by at most the two padding bytes, which is the safe direction. The
        // reported `actual` is therefore that bound — the number the check
        // acted on — since counting the real bytes would mean decoding them.
        // The arithmetic cannot overflow (the result is under the input length)
        // and the `as u64` widens rather than narrows on every Rust target.
        let decoded_ceiling = wire.payload.len() / 4 * 3;
        if decoded_ceiling > MAX_STATEMENT_PAYLOAD_BYTES {
            return Err(VerifyErrorKind::AttestationPayloadTooLarge {
                limit: MAX_STATEMENT_PAYLOAD_BYTES as u64,
                actual: decoded_ceiling as u64,
            });
        }

        let payload = BASE64
            .decode(&wire.payload)
            .map_err(|_| VerifyErrorKind::BundleParseFailed)?;
        let signatures = wire
            .signatures
            .into_iter()
            .map(|signature| {
                Ok(DsseSignature {
                    sig: BASE64
                        .decode(&signature.sig)
                        .map_err(|_| VerifyErrorKind::BundleParseFailed)?,
                    keyid: signature.keyid,
                })
            })
            .collect::<Result<Vec<_>, VerifyErrorKind>>()?;

        Ok(Self {
            payload,
            payload_type: wire.payload_type,
            signatures,
        })
    }
}

impl Serialize for DsseEnvelope {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        WireEnvelope {
            payload: BASE64.encode(&self.payload),
            payload_type: self.payload_type.clone(),
            signatures: self
                .signatures
                .iter()
                .map(|signature| WireSignature {
                    sig: BASE64.encode(&signature.sig),
                    keyid: signature.keyid.clone(),
                })
                .collect(),
        }
        .serialize(serializer)
    }
}

/// The JSON shape on the wire. Field order here IS the emitted order.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireEnvelope {
    payload: String,
    payload_type: String,
    signatures: Vec<WireSignature>,
}

#[derive(Serialize, Deserialize)]
struct WireSignature {
    sig: String,
    /// cosign omits this on a keyless signature, and row 10 makes it a hint
    /// rather than a security input — so its absence is not an error. On the
    /// write side an empty keyid is omitted too: Rekor's envelopeHash commits
    /// to these bytes, and every bundle-side verifier (sigstore-rs included)
    /// recomputes the envelope from proto3 JSON, which skips empty fields —
    /// emitting `"keyid":""` here would make our own log entry unverifiable.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    keyid: String,
}

/// The two hashes the `dsse:0.0.1` canonicalized body commits to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EnvelopeHashes {
    /// `sha256` over the exact serialized envelope bytes.
    pub envelope: Digest,
    /// `sha256` over the decoded payload bytes.
    pub payload: Digest,
}

/// Hashes the exact serialized envelope bytes handed to Rekor, never a
/// re-serialization of the struct. Sign-side self-check only.
pub(crate) fn envelope_hashes(envelope_json: &[u8], payload: &[u8]) -> EnvelopeHashes {
    EnvelopeHashes {
        envelope: Algorithm::Sha256.hash(envelope_json),
        payload: Algorithm::Sha256.hash(payload),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci::attest::{DSSE_PAYLOAD_TYPE, MAX_STATEMENT_PAYLOAD_BYTES};
    use crate::oci::verify::VerifyErrorKind;
    use base64::engine::general_purpose::STANDARD as BASE64;

    /// A Statement-shaped payload with characters base64 never produces, so a
    /// PAE that consumed the encoded text instead of the decoded bytes cannot
    /// coincidentally match.
    const PAYLOAD: &str = r#"{"_type":"https://in-toto.io/Statement/v1"}"#;

    fn envelope_json(payload: &str, payload_type: &str, signatures: &str) -> String {
        format!(
            r#"{{"payload":"{}","payloadType":"{payload_type}","signatures":{signatures}}}"#,
            BASE64.encode(payload)
        )
    }

    fn one_signature() -> &'static str {
        r#"[{"sig":"c2ln","keyid":"kid"}]"#
    }

    // ---- PAE ------------------------------------------------------------

    #[test]
    fn pae_matches_the_dsse_spec_example() {
        // The vector from the DSSE spec itself. Pinned as literal bytes: a
        // length computed from the input would agree with any implementation,
        // including a wrong one.
        assert_eq!(
            pae("http://example.com/HelloWorld", b"hello world"),
            b"DSSEv1 29 http://example.com/HelloWorld 11 hello world".to_vec()
        );
    }

    #[test]
    fn pae_over_an_in_toto_payload_is_pinned_byte_for_byte() {
        assert_eq!(
            pae(DSSE_PAYLOAD_TYPE, PAYLOAD.as_bytes()),
            format!("DSSEv1 28 {DSSE_PAYLOAD_TYPE} 43 {PAYLOAD}").into_bytes()
        );
    }

    #[test]
    fn pae_consumes_the_decoded_payload_never_the_base64_text() {
        // Checklist row 1. The encoded form is 60 bytes to the decoded 43, and
        // carries none of `{`, `"` or `/` — so a swapped implementation reds on
        // both the length field and the body.
        let over_decoded = pae(DSSE_PAYLOAD_TYPE, PAYLOAD.as_bytes());
        let over_encoded = pae(DSSE_PAYLOAD_TYPE, BASE64.encode(PAYLOAD).as_bytes());
        assert_ne!(over_decoded, over_encoded);
        assert!(
            String::from_utf8_lossy(&over_decoded).contains(" 43 {"),
            "decoded PAE must carry the raw JSON body"
        );
    }

    #[test]
    fn pae_length_prefixes_count_bytes_not_characters() {
        // `café` is 5 bytes and 4 characters; the spec says bytes.
        assert_eq!(pae("t", "café".as_bytes()), b"DSSEv1 1 t 5 caf\xc3\xa9".to_vec());
    }

    #[test]
    fn pae_handles_an_empty_payload() {
        assert_eq!(pae("t", b""), b"DSSEv1 1 t 0 ".to_vec());
    }

    // ---- envelope serialization -----------------------------------------

    #[test]
    fn serializing_an_envelope_base64s_the_payload_in_fixed_field_order() {
        let envelope = DsseEnvelope {
            payload: PAYLOAD.as_bytes().to_vec(),
            payload_type: DSSE_PAYLOAD_TYPE.to_owned(),
            signatures: vec![DsseSignature {
                sig: b"sig".to_vec(),
                keyid: "kid".to_owned(),
            }],
        };
        assert_eq!(
            serde_json::to_string(&envelope).expect("envelope serializes"),
            envelope_json(PAYLOAD, DSSE_PAYLOAD_TYPE, one_signature())
        );
    }

    #[test]
    fn an_envelope_round_trips_through_the_wire_form() {
        let wire = envelope_json(PAYLOAD, DSSE_PAYLOAD_TYPE, one_signature());
        let parsed = DsseEnvelope::parse(wire.as_bytes()).expect("well-formed envelope parses");
        assert_eq!(parsed.payload, PAYLOAD.as_bytes());
        assert_eq!(parsed.payload_type, DSSE_PAYLOAD_TYPE);
        assert_eq!(
            parsed.signatures,
            vec![DsseSignature {
                sig: b"sig".to_vec(),
                keyid: "kid".to_owned()
            }]
        );
        assert_eq!(serde_json::to_string(&parsed).expect("re-serializes"), wire);
    }

    #[test]
    fn parsing_tolerates_an_absent_keyid() {
        // cosign omits `keyid` on a keyless signature. Row 10 makes it a hint,
        // so its absence is not an error.
        let wire = envelope_json(PAYLOAD, DSSE_PAYLOAD_TYPE, r#"[{"sig":"c2ln"}]"#);
        let parsed = DsseEnvelope::parse(wire.as_bytes()).expect("a keyid-less envelope parses");
        assert_eq!(parsed.signatures[0].keyid, "");
    }

    // ---- envelope refusals ----------------------------------------------

    #[test]
    fn parsing_refuses_malformed_json() {
        assert!(matches!(
            DsseEnvelope::parse(b"{not json"),
            Err(VerifyErrorKind::BundleParseFailed)
        ));
    }

    #[test]
    fn parsing_refuses_a_non_base64_payload() {
        let wire = format!(
            r#"{{"payload":"not base64!!","payloadType":"{DSSE_PAYLOAD_TYPE}","signatures":{}}}"#,
            one_signature()
        );
        assert!(matches!(
            DsseEnvelope::parse(wire.as_bytes()),
            Err(VerifyErrorKind::BundleParseFailed)
        ));
    }

    #[test]
    fn parsing_refuses_a_foreign_payload_type() {
        // Checklist row 3: the declared type is what says how to read the
        // bytes, so a plausible-but-wrong one is refused before the parse.
        let wire = envelope_json(PAYLOAD, "application/json", one_signature());
        assert!(matches!(
            DsseEnvelope::parse(wire.as_bytes()),
            Err(VerifyErrorKind::PayloadTypeUnsupported { payload_type }) if payload_type == "application/json"
        ));
    }

    #[test]
    fn parsing_refuses_two_signatures() {
        // Checklist row 8: verifying one of several would report "verified"
        // for an envelope whose other signatures nobody checked.
        let wire = envelope_json(
            PAYLOAD,
            DSSE_PAYLOAD_TYPE,
            r#"[{"sig":"c2ln","keyid":"a"},{"sig":"c2ln","keyid":"b"}]"#,
        );
        assert!(matches!(
            DsseEnvelope::parse(wire.as_bytes()),
            Err(VerifyErrorKind::MultipleSignatures { count: 2 })
        ));
    }

    #[test]
    fn parsing_refuses_zero_signatures() {
        let wire = envelope_json(PAYLOAD, DSSE_PAYLOAD_TYPE, "[]");
        assert!(matches!(
            DsseEnvelope::parse(wire.as_bytes()),
            Err(VerifyErrorKind::MultipleSignatures { count: 0 })
        ));
    }

    #[test]
    fn parsing_refuses_an_over_cap_payload_before_decoding_it() {
        // Checklist row 16. The cap is asserted from the base64 length, so the
        // decode buffer is never allocated: base64 expands at a fixed 4/3, so
        // an over-cap encoded length proves an over-cap decoded one.
        let encoded_len = MAX_STATEMENT_PAYLOAD_BYTES / 3 * 4 + 8;
        let wire = format!(
            r#"{{"payload":"{}","payloadType":"{DSSE_PAYLOAD_TYPE}","signatures":{}}}"#,
            "A".repeat(encoded_len),
            one_signature()
        );
        assert!(matches!(
            DsseEnvelope::parse(wire.as_bytes()),
            Err(VerifyErrorKind::AttestationPayloadTooLarge { limit, .. })
                if limit as usize == MAX_STATEMENT_PAYLOAD_BYTES
        ));
    }

    // ---- envelope hashes -------------------------------------------------

    #[test]
    fn envelope_hashes_cover_the_bytes_given_not_a_reserialization() {
        // The input is deliberately NOT what this module would emit — extra
        // whitespace and a different field order. A hash of a re-serialization
        // would disagree with the bytes actually uploaded.
        let odd = format!(
            "{{ \"payloadType\": \"{DSSE_PAYLOAD_TYPE}\",\n  \"payload\": \"{}\" }}",
            BASE64.encode(PAYLOAD)
        );
        let hashes = envelope_hashes(odd.as_bytes(), PAYLOAD.as_bytes());
        assert_eq!(
            hashes.envelope,
            crate::oci::digest::Algorithm::Sha256.hash(odd.as_bytes())
        );
        assert_eq!(
            hashes.payload,
            crate::oci::digest::Algorithm::Sha256.hash(PAYLOAD.as_bytes())
        );
    }
}
