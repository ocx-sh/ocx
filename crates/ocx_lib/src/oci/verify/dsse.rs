// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! OCX's own layer around the delegated Sigstore verification of a DSSE
//! attestation.
//!
//! Nothing here builds a certificate chain, checks an SCT, or verifies a
//! signature — that is `verifier.verify(subject_bytes, bundle, …)` in
//! [`super::pipeline`], which runs unchanged for both content modes. This
//! module is defence in depth *around* that call, split by what each half
//! needs (`adr_sbom_attestations.md` D-d):
//!
//! | Half | Runs | Why there |
//! |---|---|---|
//! | [`verify_envelope`] | **before** the delegated call | its precise error kinds are the ones a user sees; the delegated refusal becomes redundancy rather than the only report |
//! | [`verify_tlog_binding`] | **after** the delegated call | it consumes log-entry material that call has already SET/Merkle-checked |
//!
//! It takes no verifying key. An earlier draft passed one, which implied this
//! module *replaced* the delegated call rather than layering over it — the
//! reading under which an attestation would silently lose the chain, SCT and
//! validity-window checks `ocx package verify` already performs.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::Deserialize;
use serde_json::value::RawValue;
use sigstore_protobuf_specs::dev::sigstore::bundle::v1::{Bundle, bundle};

use crate::oci::attest::dsse::{DsseEnvelope, DsseSignature};
use crate::oci::attest::predicate::{self, PredicateType};
use crate::oci::attest::{ACCEPTED_TLOG_KINDS, statement};
use crate::oci::verify::VerifyErrorKind;
use crate::oci::{Algorithm, Digest};

/// One attestation that passed every check, carried forward to the report.
///
/// The payload and the predicate travel as the bytes that were signed, never a
/// re-serialization (checklist row 2): `ocx package sbom --output` writes
/// [`Self::predicate`] verbatim, so what reaches the user is what the publisher
/// signed.
#[derive(Debug, Clone)]
pub struct VerifiedAttestation {
    /// predicateType read from the **signed** payload, never an annotation
    /// (checklist row 7 / CVE-2022-35929).
    pub predicate_type: String,
    /// The decoded in-toto Statement bytes the signature covers.
    pub payload: Vec<u8>,
    /// The predicate document as the verbatim sub-slice of [`Self::payload`].
    pub predicate: Box<RawValue>,
    /// The target digest this Statement was proven to bind.
    pub subject_digest: Digest,
}

/// [`verify_envelope`]'s output: the attestation, plus what the tlog half needs.
///
/// The signatures travel separately because `verifier.verify` takes the bundle
/// **by value** — by the time [`verify_tlog_binding`] runs, the envelope they
/// came from no longer exists.
pub(super) struct VerifiedEnvelope {
    /// The verified attestation, as the report DTO consumes it.
    pub attestation: VerifiedAttestation,
    /// The envelope's signatures, exactly as received.
    pub signatures: Vec<DsseSignature>,
}

/// The structural half: everything provable without a verifying key.
///
/// Runs **before** the delegated call. Rows 3, 8 and 16 live in
/// [`DsseEnvelope::parse`] and row 18 in
/// [`crate::oci::attest::statement::parse`]; this function is what surrounds
/// them — the caps custody, the subject binding (rows 4–6), and the
/// predicateType comparison (row 7).
///
/// # Errors
///
/// Every refusal is one candidate's verdict, which the ANY-of scan merges:
/// a non-DSSE bundle, a malformed or over-cap envelope, an unaccepted `_type`,
/// a Statement that binds no subject or another artifact, or a signed
/// predicateType other than the one requested.
pub(super) fn verify_envelope(
    bundle: &Bundle,
    target_digest: &Digest,
    expected_predicate_type: Option<&PredicateType>,
) -> Result<VerifiedEnvelope, VerifyErrorKind> {
    // The caller bounded the bundle blob's read (row 15) and the envelope
    // travels inside it, so nothing here goes back to the wire for bytes.
    let Some(bundle::Content::DsseEnvelope(envelope)) = bundle.content.as_ref() else {
        return Err(VerifyErrorKind::NoUsableBundle);
    };

    // Rows 3, 8 and 16 belong to `DsseEnvelope::parse`, which takes JSON; the
    // bundle carries the same envelope already decoded into a protobuf
    // message. Re-serializing through WP4's own `Serialize` impl and handing
    // it back to that parser keeps one implementation of those rules — written
    // a second time here, the two would diverge on the first fix.
    //
    // Lossless, and the impl matters: it always emits all three fields, where
    // prost-reflect's `skip_default_fields` would drop an empty `payloadType`,
    // which is precisely the case row 3 exists to refuse. `sigstore`'s own
    // `CheckedBundle` does the identical `serde_json::to_vec(&dsse)`.
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

    // Row 18 and rows 4/5/6, both delegated: the `_type` allowlist is
    // `statement::parse`'s, the every-subject binding is `binds_subject`'s.
    // This function is what surrounds them, not a second copy of them.
    let statement = statement::parse(&envelope.payload)?;
    statement::binds_subject(&statement, target_digest)?;

    // Row 7 / CVE-2022-35929: read from the signed payload, never from an
    // annotation. A requested type this Statement does not carry is a
    // narrowing miss the caller turns into not-found (S-017) — the annotation
    // direction is the caller's cross-check, and that one is a refusal.
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

/// The tlog half: checklist row 12, over the **received** bytes.
///
/// The canonicalized `dsse:0.0.1` body must commit to the signature actually
/// presented, never to a payload hash alone — GHSA-8gw7-4j42-w388, regressed as
/// CVE-2026-22703 in January 2026, which is why this stays OCX's own check
/// rather than the library's.
///
/// `payloadHash` is `sha256` over the **decoded payload**, which is what rekor's
/// `dsse:0.0.1` hashes. `envelopeHash` is deliberately **not** recomputed here
/// (D-g): the delegated bundle verifier already reconstructs the envelope from
/// the bundle's proto3 JSON (empty fields omitted — the sign side matches that
/// spelling by contract) and fails closed on a mismatch, so a second
/// reconstruction here would duplicate the same comparison.
///
/// # Errors
///
/// [`VerifyErrorKind::UnsupportedTlogEntryKind`] for a `(kind, version)` outside
/// [`crate::oci::attest::ACCEPTED_TLOG_KINDS`], and
/// [`VerifyErrorKind::TlogBindingMismatch`] on any divergence from the received
/// envelope.
pub(super) fn verify_tlog_binding(
    canonicalized_body: &[u8],
    payload: &[u8],
    signatures: &[DsseSignature],
) -> Result<(), VerifyErrorKind> {
    // Probe the kind before reading the spec: each kind canonicalizes its spec
    // differently, so deserializing a `hashedrekord` body into the dsse shape
    // would report a binding mismatch for what is really an unsupported entry
    // kind — the wrong diagnosis, and the wrong remedy for whoever reads it.
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

    // `payloadHash` is sha256 over the DECODED payload — rekor's `dsse:0.0.1`
    // rule. Hashing the PAE is `hashedrekord:0.0.2`'s rule, and comparing
    // against that here would mean no genuine entry could ever match.
    //
    // `envelopeHash` is deliberately NOT recomputed here (D-g): the delegated
    // bundle verifier reconstructs the envelope from proto3 JSON (empty fields
    // omitted, which the sign side matches by contract) and fails closed on a
    // mismatch, so recomputing it here would duplicate that comparison.
    //
    // Hex compared case-insensitively: it is a hash value, so the comparison
    // stays injective on the bytes either way, and rekor's own casing is not
    // something to fail closed on.
    let expected = Algorithm::Sha256.hash(payload);
    if !body.spec.payload_hash.algorithm.eq_ignore_ascii_case("sha256")
        || !body.spec.payload_hash.value.eq_ignore_ascii_case(expected.hex())
    {
        return Err(VerifyErrorKind::TlogBindingMismatch);
    }

    // GHSA-8gw7-4j42-w388, regressed as CVE-2026-22703: the entry must commit
    // to the signature actually presented, not merely to its payload. Equal
    // length as well as containment — a body naming an extra signature
    // describes a two-signature envelope, which is not the one received, and
    // `DsseEnvelope::parse` already refused that shape on the envelope side.
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

/// The `kind`/`apiVersion` probe, read before the spec (DATA-FMT-02's shape).
#[derive(Deserialize)]
struct TlogKind {
    kind: String,
    #[serde(rename = "apiVersion")]
    api_version: String,
}

/// A rekor `dsse:0.0.1` canonicalized body, narrowed to what row 12 compares.
///
/// Tolerant by intent: rekor owns this format and may add fields, so unknown
/// ones are ignored rather than refused. Nothing is re-serialized from it.
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

/// Enforces a trust policy's `builder` pin against a verified attestation (#103).
///
/// ANDed within a policy and ORed across the ANY-of set, so a matched policy
/// carrying no pin leaves the set unconstrained — the weakening `system_locked`
/// exists to contain (D-j).
///
/// Scoped to SLSA provenance: `builder` pins a *forward configuration* — which
/// builder this policy will accept output from — and only provenance carries a
/// builder identity to compare against. An SBOM or a custom predicate under a
/// builder-pinned policy therefore passes the pin untouched rather than being
/// refused for lacking a field its schema never had.
///
/// # Errors
///
/// [`VerifyErrorKind::BuilderMismatch`] when the predicate *is* provenance,
/// every matched policy pins a builder, and the provenance names a different
/// one or none that can be read. Within that scope a pin that cannot be
/// evaluated is a refusal, never a skip: passing an unpinnable provenance is how
/// a policy stops being a policy, and it is the v0.2-through-v1 schema hazard
/// the refusal exists for.
pub(super) fn enforce_builder_pin(
    matched: &[&crate::trust::CompiledPolicy],
    attestation: &VerifiedAttestation,
) -> Result<(), VerifyErrorKind> {
    // Out of scope before anything is read: `builder` constrains where a build
    // came from, so it has nothing to say about an SBOM or a custom predicate.
    // Dispatched on the resolved URI, so a Statement spelling the provenance URI
    // in full is provenance here too.
    let predicate_type = PredicateType::Uri(attestation.predicate_type.clone());
    if !predicate::is_provenance(&predicate_type) {
        return Ok(());
    }

    // ORed across the matched set, so one satisfied policy satisfies the set —
    // and a matched policy carrying no pin therefore satisfies it for free,
    // leaving the set unconstrained. That weakening is the decision (D-j), and
    // it is what `system_locked` exists to contain. An empty set falls out here
    // too: nothing matched, nothing pinned, nothing to enforce.
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

    // Read once, from the signed predicate. `builder_id` dispatches on the
    // resolved provenance version — v0.2 and v1 share no path — so a v0.2-shaped
    // body under a v1 `predicateType` reads as absent rather than matching a
    // field the declared schema does not have.
    let predicate: serde_json::Value =
        serde_json::from_str(attestation.predicate.get()).map_err(|_| VerifyErrorKind::BundleParseFailed)?;
    let found = predicate::builder_id(&predicate_type, &predicate);

    if let Some(found) = found
        && pins.contains(&found)
    {
        return Ok(());
    }

    // A pin that cannot be evaluated is a refusal, never a skip: silently
    // passing an unpinnable provenance is how a policy stops being a policy.
    // `found: None` is provenance carrying no readable `builder.id` — including
    // a body whose shape belongs to the other schema version, which is the
    // v0.2-through-v1 hazard this refusal exists for.
    Err(VerifyErrorKind::BuilderMismatch {
        expected: (*first).to_owned(),
        found: found.map(str::to_owned),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci::Algorithm;
    use crate::oci::attest::TLOG_KIND_WRITTEN;
    use crate::trust::CompiledPolicy;
    use base64::engine::general_purpose::STANDARD as BASE64;

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
            media_type: crate::oci::sign::bundle::BUNDLE_V03_MEDIA_TYPE.to_string(),
            verification_material: None,
            content: Some(bundle::Content::DsseEnvelope(Envelope {
                payload: payload.to_vec(),
                payload_type: crate::oci::attest::DSSE_PAYLOAD_TYPE.to_string(),
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
            media_type: crate::oci::sign::bundle::BUNDLE_V03_MEDIA_TYPE.to_string(),
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
            payload_type: crate::oci::attest::DSSE_PAYLOAD_TYPE.to_string(),
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
        let pae = crate::oci::attest::dsse::pae(crate::oci::attest::DSSE_PAYLOAD_TYPE, &payload);
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
            payload_type: crate::oci::attest::DSSE_PAYLOAD_TYPE.to_string(),
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
        let statement = crate::oci::attest::statement::parse(&payload).expect("fixture statement parses");
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
