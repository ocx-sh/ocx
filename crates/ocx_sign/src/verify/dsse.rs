// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! OCX's own checks around the delegated Sigstore verification of a DSSE attestation (`adr_sbom_attestations.md §
//! D-d`).
//!
//! No verifying key is taken here: chain, SCT and signature checks stay in `verifier.verify` ([`super::pipeline`]), or
//! an attestation silently loses them.
//! [`verify_envelope`] runs before that call so its precise refusals are what the user sees;
//! [`verify_tlog_binding`] runs after it, because it consumes log material that call already SET/Merkle-checked.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::Deserialize;
use serde_json::value::RawValue;
use sigstore_protobuf_specs::dev::sigstore::bundle::v1::{Bundle, bundle};

use crate::attest::dsse::{DsseEnvelope, DsseSignature};
use crate::attest::predicate::{self, PredicateType};
use crate::attest::{ACCEPTED_TLOG_KINDS, statement};
use crate::verify::VerifyErrorKind;
use ocx_oci::{Algorithm, Digest};

/// One attestation that passed every check.
///
/// Payload and predicate are the signed bytes, never re-serialized: `ocx package sbom --output` writes
/// [`Self::predicate`] verbatim.
#[derive(Debug, Clone)]
pub struct VerifiedAttestation {
    /// predicateType read from the **signed** payload, never an annotation (CVE-2022-35929).
    pub predicate_type: String,
    /// The decoded in-toto Statement bytes the signature covers.
    pub payload: Vec<u8>,
    /// The predicate document as the verbatim sub-slice of [`Self::payload`].
    pub predicate: Box<RawValue>,
    /// The target digest this Statement was proven to bind.
    pub subject_digest: Digest,
}

/// [`verify_envelope`]'s output; the signatures travel separately because `verifier.verify` consumes the bundle before
/// [`verify_tlog_binding`] runs.
pub(super) struct VerifiedEnvelope {
    pub attestation: VerifiedAttestation,
    /// The envelope's signatures, exactly as received.
    pub signatures: Vec<DsseSignature>,
}

/// The structural half: everything provable without a verifying key, run before the delegated call.
///
/// # Errors
///
/// One candidate's verdict, merged by the ANY-of scan: a non-DSSE bundle, a malformed or over-cap envelope,
/// an unaccepted `_type`, a Statement binding no subject or another artifact, or a signed predicateType other than the
/// requested one.
pub(super) fn verify_envelope(
    bundle: &Bundle,
    target_digest: &Digest,
    expected_predicate_type: Option<&PredicateType>,
) -> Result<VerifiedEnvelope, VerifyErrorKind> {
    let Some(bundle::Content::DsseEnvelope(envelope)) = bundle.content.as_ref() else {
        return Err(VerifyErrorKind::NoUsableBundle);
    };

    // Serialize through `DsseEnvelope`, not prost-reflect: `skip_default_fields` would drop an empty `payloadType` that
    // `parse` must refuse.
    let envelope_json = serde_json::to_vec(&DsseEnvelope {
        payload: envelope.payload.clone(),
        payload_type: envelope.payload_type.clone(),
        signatures: envelope
            .signatures
            .iter()
            .map(|signature| DsseSignature {
                sig: signature.sig.clone(),
                keyid: signature.keyid.clone(),
            })
            .collect(),
    })
    .map_err(|_| VerifyErrorKind::BundleParseFailed)?;
    let envelope = DsseEnvelope::parse(&envelope_json)?;

    let statement = statement::parse(&envelope.payload)?;
    statement::binds_subject(&statement, target_digest)?;

    // Compare against the signed payload, never an annotation (CVE-2022-35929).
    if let Some(expected) = expected_predicate_type
        && statement.predicate_type != expected.uri()
    {
        return Err(VerifyErrorKind::PredicateTypeMismatch {
            expected: expected.uri().to_owned(),
            actual: statement.predicate_type,
        });
    }

    Ok(VerifiedEnvelope {
        attestation: VerifiedAttestation {
            predicate_type: statement.predicate_type,
            payload: envelope.payload,
            predicate: statement.predicate,
            subject_digest: target_digest.clone(),
        },
        signatures: envelope.signatures,
    })
}

/// The tlog half: the log entry must commit to the **received** signature, not a payload hash alone
/// (GHSA-8gw7-4j42-w388, regressed as CVE-2026-22703), so this stays OCX's own check.
///
/// # Errors
///
/// [`VerifyErrorKind::UnsupportedTlogEntryKind`] for a `(kind, version)` outside
/// [`crate::attest::ACCEPTED_TLOG_KINDS`], and
/// [`VerifyErrorKind::TlogBindingMismatch`] on any divergence from the received
/// envelope.
pub(super) fn verify_tlog_binding(
    canonicalized_body: &[u8],
    payload: &[u8],
    signatures: &[DsseSignature],
) -> Result<(), VerifyErrorKind> {
    // Probe the kind first, or a `hashedrekord` body reports a binding mismatch instead of an unsupported kind.
    let probe: TlogKind =
        serde_json::from_slice(canonicalized_body).map_err(|_| VerifyErrorKind::TlogBindingMismatch)?;
    if !ACCEPTED_TLOG_KINDS
        .iter()
        .any(|(kind, version)| *kind == probe.kind && *version == probe.api_version)
    {
        return Err(VerifyErrorKind::UnsupportedTlogEntryKind {
            kind: probe.kind,
            version: probe.api_version,
        });
    }

    let body: TlogBody =
        serde_json::from_slice(canonicalized_body).map_err(|_| VerifyErrorKind::TlogBindingMismatch)?;

    // sha256 over the DECODED payload, rekor's `dsse:0.0.1` rule; hashing the PAE instead would match no genuine entry.
    // Case-insensitive, so rekor's hex casing never fails a genuine entry.
    let expected = Algorithm::Sha256.hash(payload);
    if !body.spec.payload_hash.algorithm.eq_ignore_ascii_case("sha256")
        || !body.spec.payload_hash.value.eq_ignore_ascii_case(expected.hex())
    {
        return Err(VerifyErrorKind::TlogBindingMismatch);
    }

    // Equal length as well as containment: a body naming an extra signature describes an envelope other than the
    // received one.
    if body.spec.signatures.len() != signatures.len() {
        return Err(VerifyErrorKind::TlogBindingMismatch);
    }
    let all_logged = signatures.iter().all(|signature| {
        body.spec.signatures.iter().any(|entry| {
            BASE64
                .decode(&entry.signature)
                .is_ok_and(|bytes| bytes == signature.sig)
        })
    });
    if !all_logged {
        return Err(VerifyErrorKind::TlogBindingMismatch);
    }
    Ok(())
}

/// Whether this envelope carries cosign v3's **image-signature** Statement
/// ([`COSIGN_SIGN_PREDICATE_TYPE`][crate::attest::COSIGN_SIGN_PREDICATE_TYPE]).
///
/// Tolerant: an unreadable payload answers `false` and reaches [`verify_envelope`]'s precise refusal;
/// answering "malformed" here would turn every broken attestation into a silent skip.
pub(super) fn is_cosign_image_signature(envelope: &sigstore_protobuf_specs::io::intoto::Envelope) -> bool {
    #[derive(Deserialize)]
    struct PredicateTypeProbe {
        #[serde(rename = "predicateType")]
        predicate_type: String,
    }

    serde_json::from_slice::<PredicateTypeProbe>(&envelope.payload)
        .is_ok_and(|probe| probe.predicate_type == crate::attest::COSIGN_SIGN_PREDICATE_TYPE)
}

/// The `kind`/`apiVersion` probe, read before the spec.
#[derive(Deserialize)]
struct TlogKind {
    kind: String,
    #[serde(rename = "apiVersion")]
    api_version: String,
}

/// A rekor `dsse:0.0.1` canonicalized body, narrowed to what the binding check compares.
// No `deny_unknown_fields`: rekor owns this format, and a field it adds would fail every verify.
#[derive(Deserialize)]
struct TlogBody {
    spec: TlogSpec,
}

#[derive(Deserialize)]
struct TlogSpec {
    #[serde(rename = "payloadHash")]
    payload_hash: TlogHash,
    signatures: Vec<TlogSignature>,
}

#[derive(Deserialize)]
struct TlogHash {
    algorithm: String,
    value: String,
}

#[derive(Deserialize)]
struct TlogSignature {
    /// base64 of the raw signature bytes, as rekor writes it.
    signature: String,
}

/// Enforces a trust policy's `builder` pin against a verified attestation; only SLSA provenance is checked.
///
/// ORed across the matched set, so one matched policy without a pin leaves the set unconstrained.
///
/// # Errors
///
/// [`VerifyErrorKind::BuilderMismatch`] when the predicate is provenance, every matched policy pins a builder,
/// and the provenance names another builder or none that can be read.
pub(super) fn enforce_builder_pin(
    matched: &[&ocx_trust::CompiledPolicy],
    attestation: &VerifiedAttestation,
) -> Result<(), VerifyErrorKind> {
    let predicate_type = PredicateType::Uri(attestation.predicate_type.clone());
    if !predicate::is_provenance(&predicate_type) {
        return Ok(());
    }

    let mut pins = Vec::with_capacity(matched.len());
    for policy in matched {
        match policy.builder.as_deref() {
            Some(pin) => pins.push(pin),
            None => return Ok(()),
        }
    }
    let Some(first) = pins.first() else {
        return Ok(());
    };

    let predicate: serde_json::Value =
        serde_json::from_str(attestation.predicate.get()).map_err(|_| VerifyErrorKind::BundleParseFailed)?;
    let found = predicate::builder_id(&predicate_type, &predicate);

    if let Some(found) = found
        && pins.contains(&found)
    {
        return Ok(());
    }

    // An unreadable `builder.id` (incl. a v0.2 body under a v1 type) refuses, never skips, or the pin stops binding.
    Err(VerifyErrorKind::BuilderMismatch {
        expected: (*first).to_owned(),
        found: found.map(str::to_owned),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::attest::TLOG_KIND_WRITTEN;
    use base64::engine::general_purpose::STANDARD as BASE64;
    use ocx_oci::Algorithm;
    use ocx_trust::CompiledPolicy;

    const SUBJECT: &[u8] = b"the artifact these tests attest to";
    const OTHER: &[u8] = b"a different artifact entirely";
    const SIG: &[u8] = &[0xDE, 0xAD, 0xBE, 0xEF];

    fn target() -> Digest {
        Algorithm::Sha256.hash(SUBJECT)
    }

    /// An in-toto Statement payload, written as bytes rather than through
    /// `statement::build` so a test can produce shapes the builder never emits.
    fn statement(subject: &Digest, predicate_type: &str, predicate: &str) -> Vec<u8> {
        format!(
            r#"{{"_type":"https://in-toto.io/Statement/v1","subject":[{{"name":"pkg","digest":{{"sha256":"{}"}}}}],"predicateType":"{predicate_type}","predicate":{predicate}}}"#,
            subject.hex()
        )
        .into_bytes()
    }

    /// A Rekor `dsse:0.0.1` canonicalized body. Every field a caller might want
    /// to corrupt is a parameter, because each one corrupts differently.
    fn tlog_body(kind: &str, version: &str, payload_hash_hex: &str, signatures: &[&[u8]]) -> Vec<u8> {
        let entries: Vec<String> = signatures
            .iter()
            .map(|sig| {
                format!(
                    r#"{{"signature":"{}","verifier":"{}"}}"#,
                    BASE64.encode(sig),
                    BASE64.encode(b"pem")
                )
            })
            .collect();
        format!(
            r#"{{"apiVersion":"{version}","kind":"{kind}","spec":{{"envelopeHash":{{"algorithm":"sha256","value":"{}"}},"payloadHash":{{"algorithm":"sha256","value":"{payload_hash_hex}"}},"signatures":[{}]}}}}"#,
            Algorithm::Sha256.hash(b"whatever envelope bytes").hex(),
            entries.join(","),
        )
        .into_bytes()
    }

    fn one_signature() -> Vec<DsseSignature> {
        vec![DsseSignature {
            sig: SIG.to_vec(),
            keyid: String::new(),
        }]
    }

    /// A Statement binding several subjects, so "checks every subject" is
    /// testable — a decoy first entry is exactly the shape row 4 exists for.
    fn statement_binding(subjects: &[&Digest], predicate_type: &str) -> Vec<u8> {
        let entries: Vec<String> = subjects
            .iter()
            .map(|digest| format!(r#"{{"name":"pkg","digest":{{"sha256":"{}"}}}}"#, digest.hex()))
            .collect();
        format!(
            r#"{{"_type":"https://in-toto.io/Statement/v1","subject":[{}],"predicateType":"{predicate_type}","predicate":{{}}}}"#,
            entries.join(","),
        )
        .into_bytes()
    }

    fn dsse_bundle(payload: &[u8]) -> Bundle {
        use sigstore_protobuf_specs::dev::sigstore::bundle::v1::bundle;
        use sigstore_protobuf_specs::io::intoto::{Envelope, Signature};
        Bundle {
            media_type: crate::sign::bundle::BUNDLE_V03_MEDIA_TYPE.to_string(),
            verification_material: None,
            content: Some(bundle::Content::DsseEnvelope(Envelope {
                payload: payload.to_vec(),
                payload_type: crate::attest::DSSE_PAYLOAD_TYPE.to_string(),
                signatures: vec![Signature {
                    sig: SIG.to_vec(),
                    keyid: String::new(),
                }],
            })),
        }
    }

    // ── the structural half: everything provable without a verifying key ──

    /// The green case the refusals below are only meaningful against.
    #[test]
    fn verify_envelope_accepts_an_envelope_binding_the_target() {
        let payload = statement(&target(), "https://cyclonedx.org/bom", r#"{"bomFormat":"CycloneDX"}"#);
        let verified = verify_envelope(&dsse_bundle(&payload), &target(), None).expect("a bound envelope verifies");
        assert_eq!(verified.attestation.subject_digest, target());
        assert_eq!(
            verified.attestation.payload, payload,
            "the payload travels as the bytes that were signed, never a re-serialization",
        );
        assert_eq!(verified.signatures.len(), 1, "the parser admits exactly one signature");
        assert_eq!(verified.signatures[0].sig, SIG);
    }

    /// CVE-2026-31830, the cross-subject splice: an attestation that verifies
    /// perfectly but attests to a *different* artifact.
    #[test]
    fn verify_envelope_refuses_a_statement_binding_another_artifact() {
        let payload = statement_binding(&[&Algorithm::Sha256.hash(OTHER)], "https://cyclonedx.org/bom");
        assert!(matches!(
            verify_envelope(&dsse_bundle(&payload), &target(), None),
            Err(VerifyErrorKind::StatementSubjectMismatch { .. })
        ));
    }

    /// Row 4: every subject is checked, not `subject[0]` alone. A decoy first
    /// entry must not be able to hide a genuine binding behind it.
    #[test]
    fn verify_envelope_checks_every_subject_not_only_the_first() {
        let decoy = Algorithm::Sha256.hash(OTHER);
        let payload = statement_binding(&[&decoy, &target()], "https://cyclonedx.org/bom");
        assert!(
            verify_envelope(&dsse_bundle(&payload), &target(), None).is_ok(),
            "a binding subject anywhere in the list binds",
        );
    }

    /// Row 7 / CVE-2022-35929: the predicateType that reaches the report is the
    /// one inside the signed payload. An annotation never gets a say — that
    /// direction is the pipeline's cross-check, and it refuses rather than relabels.
    #[test]
    fn verify_envelope_reads_the_predicate_type_from_the_signed_payload() {
        let payload = statement(&target(), "https://slsa.dev/provenance/v1", "{}");
        let verified = verify_envelope(&dsse_bundle(&payload), &target(), None).expect("verifies");
        assert_eq!(verified.attestation.predicate_type, "https://slsa.dev/provenance/v1");
    }

    /// S-017: a requested `--type` that the signed payload does not carry is a
    /// narrowing miss. The kind is what the pipeline converts into "not found";
    /// it must be distinguishable from every other refusal here.
    #[test]
    fn verify_envelope_narrows_to_the_requested_predicate_type() {
        let payload = statement(&target(), "https://cyclonedx.org/bom", "{}");
        let bundle = dsse_bundle(&payload);
        assert!(
            verify_envelope(&bundle, &target(), Some(&PredicateType::CycloneDx)).is_ok(),
            "the requested type is the one the payload carries",
        );
        assert!(matches!(
            verify_envelope(&bundle, &target(), Some(&PredicateType::SlsaProvenance1)),
            Err(VerifyErrorKind::PredicateTypeMismatch { .. })
        ));
    }

    /// The mode gate the pipeline reads as `ModeMismatch`. A bundle carrying a
    /// message signature answers a different question entirely.
    #[test]
    fn verify_envelope_refuses_a_bundle_carrying_no_dsse_envelope() {
        let bundle = Bundle {
            media_type: crate::sign::bundle::BUNDLE_V03_MEDIA_TYPE.to_string(),
            verification_material: None,
            content: None,
        };
        assert!(matches!(
            verify_envelope(&bundle, &target(), None),
            Err(VerifyErrorKind::NoUsableBundle)
        ));
    }

    /// Rows 3/8/16 belong to `DsseEnvelope::parse`; this asserts they are
    /// reached rather than re-implemented — a wrong `payloadType` must refuse
    /// here too, with the parser's own kind.
    #[test]
    fn verify_envelope_delegates_the_envelope_parse_rather_than_repeating_it() {
        use sigstore_protobuf_specs::dev::sigstore::bundle::v1::bundle;
        use sigstore_protobuf_specs::io::intoto::{Envelope, Signature};
        let payload = statement(&target(), "https://cyclonedx.org/bom", "{}");
        let mut bundle = dsse_bundle(&payload);
        bundle.content = Some(bundle::Content::DsseEnvelope(Envelope {
            payload: payload.clone(),
            payload_type: "application/vnd.attacker+json".into(),
            signatures: vec![Signature {
                sig: SIG.to_vec(),
                keyid: String::new(),
            }],
        }));
        assert!(matches!(
            verify_envelope(&bundle, &target(), None),
            Err(VerifyErrorKind::PayloadTypeUnsupported { .. })
        ));

        // Row 8: more than one signature means one of them went unchecked.
        bundle.content = Some(bundle::Content::DsseEnvelope(Envelope {
            payload,
            payload_type: crate::attest::DSSE_PAYLOAD_TYPE.to_string(),
            signatures: vec![
                Signature {
                    sig: SIG.to_vec(),
                    keyid: String::new(),
                },
                Signature {
                    sig: vec![0x01],
                    keyid: String::new(),
                },
            ],
        }));
        assert!(matches!(
            verify_envelope(&bundle, &target(), None),
            Err(VerifyErrorKind::MultipleSignatures { count: 2 })
        ));
    }

    // ── row 12: the log entry must commit to the envelope actually received ──

    /// The shape a real Rekor `dsse:0.0.1` entry has. Red until the binding is
    /// implemented; the refusals below are only meaningful once this is green,
    /// because a function that refuses everything satisfies every refusal test.
    #[test]
    fn tlog_binding_accepts_a_body_that_hashes_the_decoded_payload() {
        let payload = statement(&target(), "https://cyclonedx.org/bom", "{}");
        let body = tlog_body(
            TLOG_KIND_WRITTEN.0,
            TLOG_KIND_WRITTEN.1,
            Algorithm::Sha256.hash(&payload).hex(),
            &[SIG],
        );
        assert!(
            verify_tlog_binding(&body, &payload, &one_signature()).is_ok(),
            "a well-formed dsse:0.0.1 body over this envelope must bind",
        );
    }

    /// GHSA-8gw7-4j42-w388, regressed as CVE-2026-22703. `payloadHash` is
    /// `sha256` over the **decoded payload**; hashing the PAE instead is
    /// `hashedrekord:0.0.2`'s rule, and accepting it here would mean the log
    /// entry is checked against a preimage no `dsse:0.0.1` entry ever carries —
    /// so every real entry would have to be waved through for this to pass.
    ///
    /// This is the mutation target: swapping `payload` for `pae(..)` in the
    /// implementation must turn this test red.
    #[test]
    fn tlog_binding_rejects_a_body_that_hashes_the_pae() {
        let payload = statement(&target(), "https://cyclonedx.org/bom", "{}");
        let pae = crate::attest::dsse::pae(crate::attest::DSSE_PAYLOAD_TYPE, &payload);
        let body = tlog_body(
            TLOG_KIND_WRITTEN.0,
            TLOG_KIND_WRITTEN.1,
            Algorithm::Sha256.hash(&pae).hex(),
            &[SIG],
        );
        assert!(
            matches!(
                verify_tlog_binding(&body, &payload, &one_signature()),
                Err(VerifyErrorKind::TlogBindingMismatch)
            ),
            "a payloadHash over the PAE is not this entry kind's rule",
        );
    }

    /// The core of the CVE class: the entry must commit to the signature the
    /// envelope presented, not merely to its payload. A body naming a different
    /// signature describes a different envelope.
    #[test]
    fn tlog_binding_rejects_a_body_naming_another_signature() {
        let payload = statement(&target(), "https://cyclonedx.org/bom", "{}");
        let body = tlog_body(
            TLOG_KIND_WRITTEN.0,
            TLOG_KIND_WRITTEN.1,
            Algorithm::Sha256.hash(&payload).hex(),
            &[&[0x11, 0x22, 0x33][..]],
        );
        assert!(matches!(
            verify_tlog_binding(&body, &payload, &one_signature()),
            Err(VerifyErrorKind::TlogBindingMismatch)
        ));
    }

    /// A body carrying the presented signature *plus* another still fails: the
    /// log would then be committing to an envelope with two signatures, which
    /// is not the one that was received.
    #[test]
    fn tlog_binding_rejects_a_body_carrying_an_extra_signature() {
        let payload = statement(&target(), "https://cyclonedx.org/bom", "{}");
        let body = tlog_body(
            TLOG_KIND_WRITTEN.0,
            TLOG_KIND_WRITTEN.1,
            Algorithm::Sha256.hash(&payload).hex(),
            &[SIG, &[0x11, 0x22][..]],
        );
        assert!(matches!(
            verify_tlog_binding(&body, &payload, &one_signature()),
            Err(VerifyErrorKind::TlogBindingMismatch)
        ));
    }

    /// Each entry kind has its own canonicalization, so an unrecognized one
    /// cannot be re-derived and compared at all. `ACCEPTED_TLOG_KINDS` holds
    /// exactly one pair, and the version half is checked as strictly as the
    /// kind half — `dsse:0.0.2` would canonicalize differently.
    #[test]
    fn tlog_binding_rejects_an_entry_kind_outside_the_accepted_pair() {
        let payload = statement(&target(), "https://cyclonedx.org/bom", "{}");
        let hex = Algorithm::Sha256.hash(&payload).hex().to_owned();
        for (kind, version) in [("hashedrekord", "0.0.2"), ("dsse", "0.0.2"), ("intoto", "0.0.2")] {
            let body = tlog_body(kind, version, &hex, &[SIG]);
            assert!(
                matches!(
                    verify_tlog_binding(&body, &payload, &one_signature()),
                    Err(VerifyErrorKind::UnsupportedTlogEntryKind { .. })
                ),
                "{kind}:{version} must be refused as an unsupported entry kind",
            );
        }
    }

    /// A body that is not JSON at all, or that omits the fields the comparison
    /// needs, is a mismatch rather than a panic or a pass.
    #[test]
    fn tlog_binding_rejects_a_malformed_body() {
        let payload = statement(&target(), "https://cyclonedx.org/bom", "{}");
        for body in [&b"not json"[..], b"{}", br#"{"kind":"dsse","apiVersion":"0.0.1"}"#] {
            assert!(
                verify_tlog_binding(body, &payload, &one_signature()).is_err(),
                "a body missing the material to compare must never bind",
            );
        }
    }

    /// Row 10: `keyid` is a lookup hint, never a security decision. A hostile
    /// one must change no outcome — it is carried forward for diagnostics and
    /// compared by nobody, including the row-12 binding, which reads `sig`
    /// alone. Asserted as a *pass*, because the failure mode here is a check
    /// that quietly starts consulting it.
    #[test]
    fn a_hostile_keyid_decides_nothing() {
        use sigstore_protobuf_specs::dev::sigstore::bundle::v1::bundle;
        use sigstore_protobuf_specs::io::intoto::{Envelope, Signature};
        // Path traversal and a bidi override, so a keyid that reached any
        // decision — or any unsanitized render — would be visible.
        const HOSTILE: &str = "../../etc/passwd\u{202e}";

        let payload = statement(&target(), "https://cyclonedx.org/bom", "{}");
        let mut bundle = dsse_bundle(&payload);
        bundle.content = Some(bundle::Content::DsseEnvelope(Envelope {
            payload: payload.clone(),
            payload_type: crate::attest::DSSE_PAYLOAD_TYPE.to_string(),
            signatures: vec![Signature {
                sig: SIG.to_vec(),
                keyid: HOSTILE.to_owned(),
            }],
        }));

        let verified = verify_envelope(&bundle, &target(), None).expect("a hostile keyid is not a refusal");
        assert_eq!(
            verified.signatures[0].keyid, HOSTILE,
            "carried verbatim, not sanitized here"
        );

        let body = tlog_body(
            TLOG_KIND_WRITTEN.0,
            TLOG_KIND_WRITTEN.1,
            Algorithm::Sha256.hash(&payload).hex(),
            &[SIG],
        );
        assert!(
            verify_tlog_binding(&body, &payload, &verified.signatures).is_ok(),
            "the binding compares signature bytes, never the hint beside them",
        );
    }

    // ── #103: the builder pin, ANDed within a policy and ORed across the set ──

    fn policy_with_builder(builder: Option<&str>) -> CompiledPolicy {
        let mut policy = CompiledPolicy::exact("ci@example.test".into(), "https://issuer.example".into());
        policy.builder = builder.map(str::to_owned);
        policy
    }

    fn provenance(predicate_type: &PredicateType, predicate: &str) -> VerifiedAttestation {
        let payload = statement(&target(), predicate_type.uri(), predicate);
        let statement = crate::attest::statement::parse(&payload).expect("fixture statement parses");
        VerifiedAttestation {
            predicate_type: statement.predicate_type,
            payload,
            predicate: statement.predicate,
            subject_digest: target(),
        }
    }

    const V1_BUILDER: &str = r#"{"runDetails":{"builder":{"id":"https://ci.example/builder@v1"}}}"#;

    /// No policy pins a builder, so the set imposes no constraint.
    #[test]
    fn builder_pin_is_inert_when_no_matched_policy_pins_one() {
        let policy = policy_with_builder(None);
        let attestation = provenance(&PredicateType::SlsaProvenance1, V1_BUILDER);
        assert!(enforce_builder_pin(&[&policy], &attestation).is_ok());
    }

    /// The pinned identity is the one the provenance names.
    #[test]
    fn builder_pin_accepts_the_builder_it_pins() {
        let policy = policy_with_builder(Some("https://ci.example/builder@v1"));
        let attestation = provenance(&PredicateType::SlsaProvenance1, V1_BUILDER);
        assert!(enforce_builder_pin(&[&policy], &attestation).is_ok());
    }

    /// A different builder built it. The whole point of the pin.
    #[test]
    fn builder_pin_refuses_a_different_builder() {
        let policy = policy_with_builder(Some("https://ci.example/builder@v1"));
        let attestation = provenance(
            &PredicateType::SlsaProvenance1,
            r#"{"runDetails":{"builder":{"id":"https://evil.example/builder"}}}"#,
        );
        assert!(matches!(
            enforce_builder_pin(&[&policy], &attestation),
            Err(VerifyErrorKind::BuilderMismatch { found: Some(found), .. }) if found == "https://evil.example/builder"
        ));
    }

    /// Within provenance, a pin that cannot be evaluated is a refusal, never a
    /// skip: passing an unpinnable provenance is how a policy stops being a
    /// policy. Both unreadable shapes refuse with `found: None`.
    #[test]
    fn builder_pin_refuses_a_provenance_it_cannot_read_a_builder_from() {
        let policy = policy_with_builder(Some("https://ci.example/builder@v1"));
        for attestation in [
            provenance(&PredicateType::SlsaProvenance1, r#"{"runDetails":{}}"#),
            // v0.2 shape under a v1 predicateType: the accessor dispatches on
            // the declared version, so this reads as absent rather than as a
            // match on a field the declared schema does not have. This is the
            // hazard the refusal exists for — the schemas share no path, so a
            // skip here would pass a v1-declared attestation whose builder was
            // never compared to anything.
            provenance(
                &PredicateType::SlsaProvenance1,
                r#"{"builder":{"id":"https://ci.example/builder@v1"}}"#,
            ),
        ] {
            assert!(
                matches!(
                    enforce_builder_pin(&[&policy], &attestation),
                    Err(VerifyErrorKind::BuilderMismatch { found: None, .. })
                ),
                "an unreadable builder must refuse, not pass: {}",
                attestation.predicate_type,
            );
        }
    }

    /// `builder` pins a forward configuration — which builder this policy
    /// accepts output from — so it scopes to provenance and has nothing to say
    /// about a predicate carrying no builder identity by design. Refusing an
    /// SBOM for lacking a field its schema never had would make a builder-pinned
    /// policy unable to verify SBOMs at all.
    #[test]
    fn builder_pin_does_not_apply_to_a_predicate_that_is_not_provenance() {
        let policy = policy_with_builder(Some("https://ci.example/builder@v1"));
        for attestation in [
            provenance(&PredicateType::CycloneDx, r#"{"bomFormat":"CycloneDX"}"#),
            provenance(&PredicateType::Spdx, r#"{"spdxVersion":"SPDX-2.3"}"#),
            provenance(&PredicateType::Custom, r#"{"anything":true}"#),
            // A body that happens to carry a provenance-shaped builder is still
            // out of scope: the declared type decides, never the shape.
            provenance(
                &PredicateType::CycloneDx,
                r#"{"runDetails":{"builder":{"id":"https://evil.example/builder"}}}"#,
            ),
        ] {
            assert!(
                enforce_builder_pin(&[&policy], &attestation).is_ok(),
                "a builder pin must not reach a non-provenance predicate: {}",
                attestation.predicate_type,
            );
        }
    }

    /// The scoping in the shape a user meets it: one subject carrying both a
    /// provenance attestation and an SBOM, verified under one builder-pinned
    /// policy. Both must pass — the provenance because it names the pinned
    /// builder, the SBOM because the pin does not reach it. Ungated, the SBOM
    /// refuses and `ocx package sbom` reports nothing for a correctly signed
    /// subject.
    #[test]
    fn a_pinned_policy_verifies_provenance_and_an_sbom_on_one_subject() {
        let policy = policy_with_builder(Some("https://ci.example/builder@v1"));
        let attestations = [
            provenance(&PredicateType::SlsaProvenance1, V1_BUILDER),
            provenance(
                &PredicateType::CycloneDx,
                r#"{"bomFormat":"CycloneDX","specVersion":"1.5"}"#,
            ),
        ];
        for attestation in &attestations {
            assert!(
                enforce_builder_pin(&[&policy], attestation).is_ok(),
                "both attestations on the subject must verify: {}",
                attestation.predicate_type,
            );
        }
    }

    /// ORed across the matched set: an equal-scope policy carrying no pin
    /// weakens the set, which is exactly what `system_locked` exists to contain.
    /// Encoded as a test so the weakening is a decision, not an accident.
    #[test]
    fn builder_pin_is_satisfied_when_any_matched_policy_is_satisfied() {
        let pinned = policy_with_builder(Some("https://ci.example/builder@v1"));
        let unpinned = policy_with_builder(None);
        let attestation = provenance(
            &PredicateType::SlsaProvenance1,
            r#"{"runDetails":{"builder":{"id":"https://other.example/builder"}}}"#,
        );
        assert!(
            enforce_builder_pin(&[&pinned, &unpinned], &attestation).is_ok(),
            "a matched policy with no pin leaves the set unconstrained",
        );
        assert!(
            enforce_builder_pin(&[&pinned], &attestation).is_err(),
            "and with that policy gone the pin bites again",
        );
    }

    /// An empty matched set never reaches here in the pipeline (`matching_policies`
    /// raises first), but the primitive must still fail closed rather than read
    /// "no policy objected".
    #[test]
    fn builder_pin_on_an_empty_matched_set_imposes_nothing() {
        let attestation = provenance(&PredicateType::SlsaProvenance1, V1_BUILDER);
        assert!(enforce_builder_pin(&[], &attestation).is_ok());
    }
}
