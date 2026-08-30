// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Sigstore bundle v0.3 assembly + parsing.
//!
//! Produces the canonical `application/vnd.dev.sigstore.bundle.v0.3+json`
//! payload (cert chain + DSSE envelope + Rekor transparency-log entry)
//! using the official `sigstore_protobuf_specs` types, so the output is a
//! genuine cosign-compatible bundle. The bundle is the referrer's payload
//! layer (see [`super::pipeline::SignPipeline`]).

// The `Bundle` type is re-exported by the `sigstore` crate (its bundle feature);
// the remaining protobuf message types come from `sigstore_protobuf_specs`.
use sigstore::bundle::Bundle;
use sigstore_protobuf_specs::dev::sigstore::bundle::v1::{VerificationMaterial, bundle, verification_material};
use sigstore_protobuf_specs::dev::sigstore::common::v1::{LogId, PublicKeyIdentifier, X509Certificate};
use sigstore_protobuf_specs::dev::sigstore::rekor::v1::{
    Checkpoint, InclusionPromise, InclusionProof, KindVersion, TransparencyLogEntry,
};
use sigstore_protobuf_specs::io::intoto::{Envelope, Signature};

use serde::Deserialize;

use x509_cert::Certificate as X509Cert;
use x509_cert::der::Decode as _;

use super::error::SignErrorKind;
use super::fulcio::FulcioCertificate;
use super::rekor::RekorEntry;
use crate::oci::attest::TLOG_KIND_WRITTEN;
use crate::oci::attest::dsse::{DsseEnvelope, envelope_hashes};
use crate::oci::sign::key_ref::KeyBackendKind;
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
    /// Which key model produced this signature. Reported so a consumer can tell
    /// a `file`-backed signature from a keyless one — and from a future KMS one
    /// — without parsing the bundle.
    pub key_backend: KeyBackendKind,
    /// The signing key's cosign hint (`base64(sha256(SPKI DER))`), present in
    /// key mode only. Keyless carries a certificate instead, and the identity
    /// fields above are read off it.
    pub public_key_hint: Option<String>,
    /// The Rekor log index, when a transparency record was created.
    ///
    /// `None` is a legal outcome under a key (`--rekor-upload` is opt-in there,
    /// see the spec's Rekor-upload default), and never under keyless. Carried
    /// so the sign result can **state** whether a record exists rather than
    /// leaving the operator to infer it from an omission.
    pub transparency_log_index: Option<u64>,
    /// The DSSE envelope's own bytes — the ones the bundle wraps, and the ones
    /// a cosign `sha256-<hex>.att` sidecar layer holds verbatim.
    ///
    /// Carried rather than re-derived: the sidecar publishes the *same*
    /// signature the bundle does, and digging the envelope back out of
    /// [`Self::bytes`] would make the sidecar's layer depend on this file's
    /// serialization of a document it already had in hand.
    pub envelope_json: Vec<u8>,
    /// PEM leaf certificate, keyless only — the sidecar's
    /// `dev.sigstore.cosign/certificate` annotation.
    ///
    /// The bundle carries the same certificate in its `verificationMaterial`;
    /// a sidecar has no bundle to carry it in, so the annotation is the only
    /// place a reader can find it. Absent under a key, where there is no
    /// certificate at all — the same legal shape
    /// [`SignedBlob`](super::signer::SignedBlob) documents.
    pub certificate_pem: Option<String>,
    /// The offline Rekor bundle for the sidecar's `dev.sigstore.cosign/bundle`
    /// annotation, when an entry was created.
    ///
    /// Same value, same derivation and same absence rule as
    /// [`SignedBlob::rekor_bundle`](super::signer::SignedBlob::rekor_bundle):
    /// under a key with no `--rekor-upload` there is no entry and no
    /// annotation.
    pub rekor_bundle: Option<String>,
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

/// The transparency-log entry, the verification material, the serialization and
/// the identity read back off the leaf.
///
/// Kept separate from [`build_dsse_bundle`] because `kind_version` and
/// `content` are the only things a second bundle shape would vary, and this
/// half is where the inclusion-proof rule and the v0.3 certificate shape live.
fn assemble(
    material: SigningMaterial<'_>,
    kind_version: (&str, &str),
    rekor: Option<&RekorEntry>,
    content: bundle::Content,
    envelope_json: Vec<u8>,
) -> Result<SignedBundle, SignErrorKind> {
    // A transparency record is mandatory under keyless and opt-in under a key
    // (spec §Rekor-upload default), so the entry list is built from what the
    // run actually produced rather than assumed to hold one.
    let tlog_entries = match rekor {
        Some(rekor) => {
            // The Rekor log id is hex; the protobuf LogId carries the raw key-id bytes.
            let log_id_raw = hex::decode(&rekor.log_id).unwrap_or_default();
            vec![TransparencyLogEntry {
                log_index: rekor.log_index as i64,
                log_id: Some(LogId { key_id: log_id_raw }),
                kind_version: Some(KindVersion {
                    kind: kind_version.0.to_string(),
                    version: kind_version.1.to_string(),
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
                        .ok_or(SignErrorKind::TransparencyLogUnavailable)?,
                ),
                canonicalized_body: rekor.canonicalized_body.clone(),
            }]
        }
        None => Vec::new(),
    };

    let verification_material = VerificationMaterial {
        timestamp_verification_data: None,
        tlog_entries,
        content: Some(material.verification_content()),
    };

    let bundle = Bundle {
        media_type: BUNDLE_V03_MEDIA_TYPE.to_string(),
        verification_material: Some(verification_material),
        content: Some(content),
    };

    let bytes = serde_json::to_vec(&bundle).map_err(|e| SignErrorKind::Internal(Box::new(e)))?;
    let digest = Algorithm::Sha256.hash(&bytes);

    // `certificate_pem` rides this same match rather than a second one: it is
    // the *same* certificate the `verificationMaterial` above was built from,
    // and a sidecar annotation naming a different one than the bundle would be
    // a signature two readers disagree about.
    let (certificate_identity, certificate_oidc_issuer, key_backend, public_key_hint, certificate_pem) = match material
    {
        // Report what the verifier will read. The extractors are the verify
        // side's own, so the two commands cannot drift into disagreeing about
        // one cert.
        SigningMaterial::Certificate(cert) => {
            let leaf = X509Cert::from_der(&cert.leaf_der).ok();
            (
                leaf.as_ref().and_then(subject_identity).unwrap_or_default(),
                leaf.as_ref().and_then(oidc_issuer).unwrap_or_default(),
                KeyBackendKind::Keyless,
                None,
                Some(cert.leaf_pem.clone()),
            )
        }
        // A key-mode signature carries no certificate, so it carries no
        // identity and no issuer. Empty strings rather than invented ones: the
        // absence is the fact, and the hint is what identifies the signer.
        SigningMaterial::PublicKey { hint, kind, .. } => {
            (String::new(), String::new(), kind, Some(hint.to_owned()), None)
        }
    };

    Ok(SignedBundle {
        bytes,
        digest,
        certificate_identity,
        certificate_oidc_issuer,
        key_backend,
        public_key_hint,
        transparency_log_index: rekor.map(|entry| entry.log_index),
        envelope_json,
        certificate_pem,
        rekor_bundle: rekor.map(super::simplesigning_write::offline_bundle).transpose()?,
    })
}

/// What a bundle's `verificationMaterial` holds: a Fulcio leaf, or a bare
/// public key.
///
/// `sigstore::bundle::sign` hardcodes `Content::X509CertificateChain`, so the
/// key-mode arm is hand-assembled here (spec §WP9). Modelling it as one enum
/// keeps the two arms in a single `match` the compiler keeps total, rather than
/// as two near-copies of `assemble`.
pub(super) enum SigningMaterial<'a> {
    /// Keyless: the ephemeral leaf Fulcio issued.
    Certificate(&'a FulcioCertificate),
    /// Key mode: the cosign hint for the signing key's public half, plus the
    /// backend that holds the private half.
    ///
    /// The SPKI DER itself is deliberately absent — `PublicKeyIdentifier`
    /// carries only the hint, and a verifier resolves it against a key it was
    /// given out of band. Deriving the hint stays
    /// [`public_key_hint`](crate::oci::sign::public_key_hint)'s job so the
    /// derivation lives in exactly one place.
    PublicKey { hint: &'a str, kind: KeyBackendKind },
}

impl SigningMaterial<'_> {
    /// The protobuf `verificationMaterial.content` oneof for this material.
    fn verification_content(&self) -> verification_material::Content {
        match self {
            // `certificate`, not `x509CertificateChain`: bundle v0.3 replaced
            // the chain field with a single leaf, and a verifier that enforces
            // the profile refuses a document carrying the older shape under the
            // newer media type. Fulcio's intermediates come from the trust
            // root, so the leaf is all a chain could have carried anyway.
            Self::Certificate(cert) => verification_material::Content::Certificate(X509Certificate {
                raw_bytes: cert.leaf_der.clone(),
            }),
            // `PublicKeyIdentifier` carries the hint and nothing else — the key
            // itself is delivered out of band and a verifier resolves the hint
            // against a key it was already given (`--key`, or a `kind = "key"`
            // signers entry). Matches `key_bundle.json`, which cosign v3.1.1
            // wrote with `--key`.
            Self::PublicKey { hint, .. } => verification_material::Content::PublicKey(PublicKeyIdentifier {
                hint: (*hint).to_owned(),
            }),
        }
    }
}

/// A DSSE envelope and the exact bytes it serialized to.
///
/// One value rather than two parameters: the sign-side hash checks cover the
/// payload only, so a struct and a byte string supplied separately could
/// disagree about `signatures` or `payloadType` and still pass both. Private
/// fields and a serializing constructor make the mismatched pair
/// unconstructible instead of merely untested.
pub(super) struct SignedEnvelope {
    envelope: DsseEnvelope,
    json: Vec<u8>,
}

impl SignedEnvelope {
    /// Serialize `envelope`, keeping the bytes alongside it.
    pub(super) fn new(envelope: DsseEnvelope) -> Result<Self, SignErrorKind> {
        let json = serde_json::to_vec(&envelope).map_err(|e| SignErrorKind::Internal(Box::new(e)))?;
        Ok(Self { envelope, json })
    }

    /// The bytes to upload — never a re-serialization of [`Self::envelope`].
    pub(super) fn json(&self) -> &[u8] {
        &self.json
    }
}

/// Assemble a Sigstore bundle v0.3 carrying a DSSE envelope.
///
/// The one bundle builder. `KindVersion` is `dsse:0.0.1` and the content oneof
/// is `dsseEnvelope`; the `messageSignature` form OCX used to write for image
/// signatures is gone (spec D2) — cosign v3 wraps an image signature in a DSSE
/// Statement too, so a second shape bought nothing but a third arm in the
/// discovery merge.
///
/// `signed` carries the envelope and the exact byte string uploaded to Rekor
/// as one value — see [`SignedEnvelope`] for why they may not be passed
/// separately.
pub(super) fn build_dsse_bundle(
    material: SigningMaterial<'_>,
    signed: &SignedEnvelope,
    rekor: Option<&RekorEntry>,
) -> Result<SignedBundle, SignErrorKind> {
    let envelope = &signed.envelope;
    // Only checkable against a body the log returned. Under a key with no
    // upload there is no body, and therefore nothing this check could compare
    // against — its absence is the legal `--no-rekor-upload` shape, not a
    // skipped verification.
    if let Some(rekor) = rekor {
        assert_body_records_our_envelope(&rekor.canonicalized_body, envelope, &signed.json)?;
    }

    let content = bundle::Content::DsseEnvelope(Envelope {
        payload: envelope.payload.clone(),
        payload_type: envelope.payload_type.clone(),
        signatures: envelope
            .signatures
            .iter()
            .map(|signature| Signature {
                sig: signature.sig.clone(),
                keyid: signature.keyid.clone(),
            })
            .collect(),
    });

    assemble(material, TLOG_KIND_WRITTEN, rekor, content, signed.json().to_vec())
}

/// The sign-side half of the D-g tlog binding: the log recorded the envelope
/// this process uploaded, and not some other one.
///
/// This check exists **only** here. A verifier holds a structured
/// protobuf-JSON `dsseEnvelope`, not the byte string that was uploaded, so it
/// cannot recompute `envelopeHash` at all — it binds on `payloadHash` and the
/// signatures instead. The sign side does hold those exact bytes, which makes
/// this the one place the comparison is honest rather than a re-serialization
/// guess about key order and base64 padding.
///
/// `payloadHash` is checked alongside it because `dsse:0.0.1` hashes the
/// **decoded payload**, while Rekor v2's `hashedrekord:0.0.2` hashes the PAE.
/// The two are both 32 plausible bytes, so a body carrying the v2 regime would
/// otherwise be published and only fail at some future verifier.
///
/// A `2xx` from the log with hashes that disagree is `RekorSetMalformed`, not
/// `TransparencyLogUnavailable`: the log is reachable and what it returned is
/// unusable, so waiting does not help.
fn assert_body_records_our_envelope(
    canonicalized_body: &[u8],
    envelope: &DsseEnvelope,
    envelope_json: &[u8],
) -> Result<(), SignErrorKind> {
    let body: DsseCanonicalBody =
        serde_json::from_slice(canonicalized_body).map_err(|_| SignErrorKind::RekorSetMalformed)?;
    let ours = envelope_hashes(envelope_json, &envelope.payload);

    // The `algorithm` label is deliberately not compared: a matching sha256 hex
    // already proves the log hashed these bytes, and a body that agreed on the
    // value while disagreeing on the label would be refused for nothing.
    if body.spec.envelope_hash.value != ours.envelope.hex() || body.spec.payload_hash.value != ours.payload.hex() {
        return Err(SignErrorKind::RekorSetMalformed);
    }
    Ok(())
}

/// The `dsse:0.0.1` canonicalized body, narrowed to the two hashes.
///
/// A missing field is a deserialization failure rather than a skipped
/// comparison — an absent hash read as "nothing to check" is how a binding
/// assertion becomes a no-op.
#[derive(Deserialize)]
struct DsseCanonicalBody {
    spec: DsseCanonicalSpec,
}

#[derive(Deserialize)]
struct DsseCanonicalSpec {
    #[serde(rename = "envelopeHash")]
    envelope_hash: RekorHashValue,
    #[serde(rename = "payloadHash")]
    payload_hash: RekorHashValue,
}

#[derive(Deserialize)]
struct RekorHashValue {
    value: String,
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
/// `max_bytes` is the caller's cap rather than [`MAX_BUNDLE_SIZE_BYTES`]
/// directly: an attestation bundle carries a whole SBOM and is bounded by
/// `MAX_ATTESTATION_ENVELOPE_BYTES` instead, and a cap hardcoded here would
/// refuse at 512 KiB whatever the fetch path had already accepted. The check
/// runs BEFORE `serde_json::from_slice` so the attacker's bytes never hit the
/// parser.
///
/// Returns `None` when the payload exceeds `max_bytes` or does not deserialize;
/// the verify pipeline maps `None` to `VerifyErrorKind::BundleParseFailed`.
pub(crate) fn parse_bundle(bytes: &[u8], max_bytes: usize) -> Option<Bundle> {
    if bytes.len() > max_bytes {
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
        assert!(
            parse_bundle(&junk, MAX_BUNDLE_SIZE_BYTES).is_none(),
            "oversized payload must be rejected"
        );
    }

    /// The cap is the caller's, not this module's: the same bytes that a
    /// signature-mode caller refuses must reach the parser for an
    /// attestation-mode caller, or `ocx package sbom` would refuse every SBOM
    /// over 512 KiB no matter what the fetch path accepted.
    #[test]
    fn parse_bundle_honours_the_callers_cap_in_both_directions() {
        let junk = vec![0xffu8; MAX_BUNDLE_SIZE_BYTES + 1];
        assert!(
            parse_bundle(&junk, MAX_BUNDLE_SIZE_BYTES).is_none(),
            "over the caller's cap must be refused before the parser sees it"
        );
        // Under a larger cap the size gate no longer fires, so the refusal has
        // to come from the parser instead — a different reason for the same
        // `None`, which is what proves the gate was skipped.
        assert!(
            parse_bundle(&junk, MAX_BUNDLE_SIZE_BYTES * 2).is_none(),
            "junk is still not a bundle"
        );
        let empty_bundle = br#"{"mediaType":"application/vnd.dev.sigstore.bundle.v0.3+json"}"#;
        assert!(
            parse_bundle(empty_bundle, empty_bundle.len()).is_some(),
            "a payload at exactly the cap must reach the parser"
        );
        assert!(
            parse_bundle(empty_bundle, empty_bundle.len() - 1).is_none(),
            "one byte over the cap must not"
        );
    }

    #[test]
    fn parse_bundle_rejects_non_json() {
        assert!(parse_bundle(b"not json", MAX_BUNDLE_SIZE_BYTES).is_none());
    }

    /// Parse-level shape validation is deliberately absent: `Bundle`'s fields
    /// are all optional, so `{}` deserializes cleanly with no content and no
    /// verification material. The verify pipeline is what refuses a
    /// content-less bundle (`content_matches_mode` rejects it as
    /// `NoUsableBundle`, which `from_bundle` maps to a `ModeMismatch` skip,
    /// never a parse error). This test pins that assumption so a `sigstore`
    /// upgrade that changes it turns red here instead of silently shifting
    /// which layer does the rejecting.
    #[test]
    fn parse_bundle_accepts_empty_json_with_no_content() {
        let bundle = parse_bundle(b"{}", MAX_BUNDLE_SIZE_BYTES)
            .expect("empty JSON object deserializes: every Bundle field is optional");
        assert!(bundle.content.is_none());
        assert!(bundle.verification_material.is_none());
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
    fn build_refuses_a_proof_whose_hex_fields_are_malformed() {
        // `proto_inclusion_proof` drops an unparseable proof; that drop must
        // reach the same refusal, not silently publish a proofless bundle.
        let mut proof = test_proof();
        proof.root_hash = "not hex".into();
        let entry = RekorEntry {
            canonicalized_body: dsse_body(ENVELOPE_HASH, PAYLOAD_HASH),
            ..test_entry(Some(proof))
        };
        let err = build_dsse_bundle(
            SigningMaterial::Certificate(&test_certificate()),
            &test_signed(),
            Some(&entry),
        )
        .expect_err("an unencodable proof must not produce a bundle");
        assert!(
            matches!(err, SignErrorKind::TransparencyLogUnavailable),
            "expected TransparencyLogUnavailable, got: {err:?}"
        );
    }

    // ── DSSE bundle ──────────────────────────────────────────────────────────

    /// The exact bytes the sign side hands to Rekor, pinned as a literal.
    /// Every hash below is taken over *this string*, so if `DsseEnvelope`'s
    /// serialization ever moves a field or resurrects an empty `keyid`, the
    /// pin reds here rather than the goldens silently agreeing with a new
    /// shape. An empty keyid is omitted by contract: Rekor's envelopeHash
    /// commits to these bytes, and bundle-side verifiers recompute the
    /// envelope from proto3 JSON, which skips empty fields.
    const ENVELOPE_JSON: &str = concat!(
        r#"{"payload":"eyJfdHlwZSI6Imh0dHBzOi8vaW4tdG90by5pby9TdGF0ZW1lbnQvdjEifQ==","#,
        r#""payloadType":"application/vnd.in-toto+json","#,
        r#""signatures":[{"sig":"c2lnLWJ5dGVz"}]}"#
    );
    const STATEMENT: &[u8] = br#"{"_type":"https://in-toto.io/Statement/v1"}"#;
    /// `sha256(ENVELOPE_JSON)`.
    const ENVELOPE_HASH: &str = "4abcb5f45e2370c7fb41acb8d8beec6af7a102e80b31c2702ed1681ef8a8fc0f";
    /// `sha256(STATEMENT)` — the **decoded** payload, which is what
    /// `dsse:0.0.1` hashes.
    const PAYLOAD_HASH: &str = "efd35cf5b72b5b9eeee4ee292f5d6b079191324b19ee9231892fb391c82c1d51";
    /// `sha256(PAE("application/vnd.in-toto+json", STATEMENT))` — the Rekor v2
    /// `hashedrekord:0.0.2` regime, and the wrong answer here. Distinct from
    /// `PAYLOAD_HASH`, which is the only reason a swap is detectable.
    const PAE_HASH: &str = "57dd565335c70baa2e9fc48a46e80907434e3c6ea2342ef806dad983891349cd";

    fn test_signed() -> SignedEnvelope {
        SignedEnvelope::new(test_envelope()).expect("the fixture envelope serializes")
    }

    fn test_envelope() -> DsseEnvelope {
        DsseEnvelope {
            payload: STATEMENT.to_vec(),
            payload_type: "application/vnd.in-toto+json".to_string(),
            signatures: vec![crate::oci::attest::dsse::DsseSignature {
                sig: b"sig-bytes".to_vec(),
                keyid: String::new(),
            }],
        }
    }

    /// A `dsse:0.0.1` canonicalized body carrying the given hex hashes.
    fn dsse_body(envelope_hash: &str, payload_hash: &str) -> Vec<u8> {
        format!(
            concat!(
                r#"{{"apiVersion":"0.0.1","spec":{{"signatures":[{{"signature":"c2lnLWJ5dGVz","verifier":"UEVN"}}],"#,
                r#""envelopeHash":{{"algorithm":"sha256","value":"{}"}},"#,
                r#""payloadHash":{{"algorithm":"sha256","value":"{}"}}}}}}"#
            ),
            envelope_hash, payload_hash
        )
        .into_bytes()
    }

    fn dsse_entry(body: Vec<u8>) -> RekorEntry {
        RekorEntry {
            canonicalized_body: body,
            ..test_entry(Some(test_proof()))
        }
    }

    #[test]
    fn the_dsse_envelope_serializes_to_the_pinned_bytes() {
        // The premise every golden hash below rests on. Asserted separately so
        // a serialization change reports itself rather than surfacing as an
        // unexplained hash mismatch three tests away.
        assert_eq!(
            String::from_utf8(test_signed().json().to_vec()).expect("UTF-8"),
            ENVELOPE_JSON
        );
    }

    #[test]
    fn upload_form_and_bundle_form_hash_identically() {
        // The invariant the keyid incident proved is load-bearing: Rekor's
        // envelopeHash commits to the uploaded serde_json spelling, while
        // every bundle-side verifier recomputes the hash from the bundle's
        // proto3-JSON envelope. The static ENVELOPE_JSON golden pins only the
        // upload half, so this asserts the two serializers agree on the same
        // envelope — either side moving alone reds here.
        let signed = test_signed();
        let bundle = build_dsse_bundle(
            SigningMaterial::Certificate(&test_certificate()),
            &signed,
            Some(&dsse_entry(dsse_body(ENVELOPE_HASH, PAYLOAD_HASH))),
        )
        .expect("a body whose hashes match the uploaded bytes builds");
        let parsed = parse_bundle(&bundle.bytes, crate::oci::attest::MAX_ATTESTATION_ENVELOPE_BYTES)
            .expect("the bundle round-trips");
        let Some(bundle::Content::DsseEnvelope(envelope)) = parsed.content else {
            panic!("dsse bundle must carry the dsseEnvelope oneof");
        };
        let bundle_form = serde_json::to_vec(&envelope).expect("prost envelope serializes");
        assert_eq!(
            Algorithm::Sha256.hash(&bundle_form).hex(),
            Algorithm::Sha256.hash(signed.json()).hex(),
            "the uploaded envelope and the bundle envelope must hash identically,\n\
             or the logged envelopeHash can never match a verifier's recompute"
        );
    }

    #[test]
    fn build_dsse_bundle_carries_the_dsse_envelope_content_oneof() {
        let signed = build_dsse_bundle(
            SigningMaterial::Certificate(&test_certificate()),
            &test_signed(),
            Some(&dsse_entry(dsse_body(ENVELOPE_HASH, PAYLOAD_HASH))),
        )
        .expect("a body whose hashes match the uploaded bytes builds");

        let parsed = parse_bundle(&signed.bytes, crate::oci::attest::MAX_ATTESTATION_ENVELOPE_BYTES)
            .expect("the bundle round-trips");
        let material = parsed.verification_material.expect("verification material");
        let kind = material.tlog_entries[0].kind_version.as_ref().expect("kindVersion");
        assert_eq!((kind.kind.as_str(), kind.version.as_str()), ("dsse", "0.0.1"));
        match parsed.content.expect("content oneof") {
            bundle::Content::DsseEnvelope(envelope) => {
                assert_eq!(envelope.payload, STATEMENT, "the payload travels decoded");
                assert_eq!(envelope.payload_type, "application/vnd.in-toto+json");
                assert_eq!(envelope.signatures.len(), 1);
                assert_eq!(envelope.signatures[0].sig, b"sig-bytes");
            }
            other => panic!("an attestation bundle carries dsseEnvelope, got: {other:?}"),
        }
    }

    #[test]
    fn build_dsse_bundle_refuses_a_body_whose_envelope_hash_disagrees() {
        // D-g's sign-side half: this is the one side that holds the exact bytes
        // it uploaded, so it is the one side where the check is honest. A log
        // that recorded a different envelope than we sent is unusable — 2xx or
        // not — because the bundle would ship a tlog entry binding someone
        // else's envelope.
        let err = build_dsse_bundle(
            SigningMaterial::Certificate(&test_certificate()),
            &test_signed(),
            Some(&dsse_entry(dsse_body(&"ab".repeat(32), PAYLOAD_HASH))),
        )
        .expect_err("a mismatched envelopeHash must not produce a bundle");
        assert!(
            matches!(err, SignErrorKind::RekorSetMalformed),
            "expected RekorSetMalformed, got: {err:?}"
        );
    }

    #[test]
    fn build_dsse_bundle_refuses_a_payload_hash_taken_over_the_pae() {
        // The specific wire error an adversarial pass caught: `dsse:0.0.1`
        // hashes the DECODED payload, while `hashedrekord:0.0.2` (Rekor v2)
        // hashes the PAE. Both are 32 plausible bytes and both round-trip, so
        // nothing but this assertion keeps the wrong one dead.
        let err = build_dsse_bundle(
            SigningMaterial::Certificate(&test_certificate()),
            &test_signed(),
            Some(&dsse_entry(dsse_body(ENVELOPE_HASH, PAE_HASH))),
        )
        .expect_err("a payloadHash over the PAE must not produce a bundle");
        assert!(
            matches!(err, SignErrorKind::RekorSetMalformed),
            "expected RekorSetMalformed, got: {err:?}"
        );
    }

    #[test]
    fn build_dsse_bundle_refuses_a_body_it_cannot_read_the_hashes_out_of() {
        // A body with no hashes at all must refuse, not skip the check. An
        // absent field read as "nothing to compare" is how a binding assertion
        // becomes a no-op.
        let err = build_dsse_bundle(
            SigningMaterial::Certificate(&test_certificate()),
            &test_signed(),
            Some(&dsse_entry(br#"{"apiVersion":"0.0.1","spec":{}}"#.to_vec())),
        )
        .expect_err("a body carrying no hashes must not produce a bundle");
        assert!(
            matches!(err, SignErrorKind::RekorSetMalformed),
            "expected RekorSetMalformed, got: {err:?}"
        );
    }

    #[test]
    fn build_dsse_bundle_refuses_an_entry_with_no_inclusion_proof() {
        // Same rule as the signature path: ocx's verifier requires the Merkle
        // proof, so publishing without one ships an unverifiable artifact.
        let entry = RekorEntry {
            canonicalized_body: dsse_body(ENVELOPE_HASH, PAYLOAD_HASH),
            ..test_entry(None)
        };
        let err = build_dsse_bundle(
            SigningMaterial::Certificate(&test_certificate()),
            &test_signed(),
            Some(&entry),
        )
        .expect_err("a proofless Rekor entry must not produce a bundle");
        assert!(
            matches!(err, SignErrorKind::TransparencyLogUnavailable),
            "expected TransparencyLogUnavailable, got: {err:?}"
        );
    }

    /// WP3/D2: an image signature is a DSSE Statement, not a `messageSignature`.
    ///
    /// The `messageSignature` builder is deleted, so this asserts on the shape
    /// the one remaining builder produces — a bundle carrying `dsseEnvelope`
    /// and a `dsse:0.0.1` transparency-log entry, which is what cosign v3
    /// writes for `cosign sign` and what `keyless_bundle.json` holds.
    #[test]
    fn the_only_bundle_shape_is_a_dsse_envelope() {
        let signed = build_dsse_bundle(
            SigningMaterial::Certificate(&test_certificate()),
            &test_signed(),
            Some(&dsse_entry(dsse_body(ENVELOPE_HASH, PAYLOAD_HASH))),
        )
        .expect("build");
        assert!(signed.digest.to_string().starts_with("sha256:"));

        let parsed = parse_bundle(&signed.bytes, MAX_BUNDLE_SIZE_BYTES).expect("bundle round-trips");
        assert_eq!(parsed.media_type, BUNDLE_V03_MEDIA_TYPE);
        let material = parsed.verification_material.expect("verification material");
        assert_eq!(material.tlog_entries.len(), 1);
        let kind = material.tlog_entries[0].kind_version.as_ref().expect("kindVersion");
        assert_eq!((kind.kind.as_str(), kind.version.as_str()), ("dsse", "0.0.1"));
        assert!(
            matches!(parsed.content, Some(bundle::Content::DsseEnvelope(_))),
            "the sign path writes dsseEnvelope; messageSignature is gone (spec D2)"
        );
    }
}
