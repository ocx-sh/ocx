// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The in-toto Statement: build, parse, and subject binding.
//!
//! The predicate is a [`RawValue`] on both sides — spliced verbatim when
//! building, borrowed as the exact sub-slice when parsing. Never a
//! `serde_json::Value`: this crate enables `serde_json`'s `preserve_order`, so a
//! `Value` round-trip would preserve key order but still re-spell whitespace and
//! numbers, defeating the byte-fidelity contract (D-b, checklist row 2).
//!
//! Items are `pub` rather than the ADR's `pub(crate)` for the reason
//! [`crate::oci::attest`] records against its constants: no in-crate caller
//! exists until the verify and sign pipelines land, and `pub(crate)` trips
//! `dead_code` under `warnings = "deny"` until one does. Each of those work
//! packages narrows what it consumes as it lands.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::value::RawValue;

use crate::oci::Digest;
use crate::oci::attest::predicate::PredicateType;
use crate::oci::attest::{ACCEPTED_STATEMENT_TYPES, STATEMENT_TYPE_WRITTEN};
use crate::oci::sign::SignErrorKind;
use crate::oci::verify::VerifyErrorKind;

/// An in-toto Statement, the DSSE payload OCX signs and verifies.
#[derive(Debug, Clone)]
pub(crate) struct Statement {
    /// The `_type` URI. OCX writes v1 and accepts the closed v1/v0.1 allowlist.
    pub statement_type: String,
    /// What this Statement is about. Every entry is checked on verify.
    pub subject: Vec<Subject>,
    /// The resolved predicate-type URI.
    pub predicate_type: String,
    /// The predicate document, byte-for-byte as it arrived.
    pub predicate: Box<RawValue>,
}

// `RawValue` implements no comparison of its own, and byte equality of the
// predicate slice is exactly the contract this type carries — two Statements
// are the same Statement when their predicate bytes are the same bytes.
impl PartialEq for Statement {
    fn eq(&self, other: &Self) -> bool {
        self.statement_type == other.statement_type
            && self.subject == other.subject
            && self.predicate_type == other.predicate_type
            && self.predicate.get() == other.predicate.get()
    }
}

impl Eq for Statement {}

impl Serialize for Statement {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        WireStatement {
            statement_type: self.statement_type.clone(),
            subject: self.subject.clone(),
            predicate_type: self.predicate_type.clone(),
            predicate: self.predicate.clone(),
        }
        .serialize(serializer)
    }
}

/// The one DigestSet key that binds. Hardcoded per checklist row 6.
const SUBJECT_DIGEST_ALGORITHM: &str = "sha256";

/// Subjects and algorithm names a refusal is allowed to name.
///
/// A hostile Statement fits hundreds of thousands of subjects inside
/// `MAX_STATEMENT_PAYLOAD_BYTES`, and both refusals name what they saw. The cap
/// is applied where the diagnosis is built, not where it is rendered: the
/// `--json` envelope carries the structured fields straight out and would
/// bypass a renderer-side truncation (PKG-26).
const MAX_REPORTED_SUBJECTS: usize = 8;

/// The JSON shape on the wire. Field order here IS the emitted order, and
/// `predicate` stays a `RawValue` on both sides so the document survives the
/// round trip byte-for-byte.
#[derive(Serialize, Deserialize)]
struct WireStatement {
    #[serde(rename = "_type")]
    statement_type: String,
    subject: Vec<Subject>,
    #[serde(rename = "predicateType")]
    predicate_type: String,
    predicate: Box<RawValue>,
}

/// One in-toto subject.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Subject {
    /// Informational only — [`Self::digest`] is the sole binding (checklist row 4).
    pub name: String,
    /// Algorithm-keyed digest set, e.g. `{"sha256": "<hex, no prefix>"}`.
    pub digest: BTreeMap<String, String>,
}

/// Builds the Statement OCX writes.
///
/// # Errors
///
/// Only if the cosign predicate wrapper cannot be serialized.
pub(crate) fn build(
    subject_name: &str,
    subject_digest: &Digest,
    predicate_type: &PredicateType,
    predicate: &RawValue,
    now: DateTime<Utc>,
) -> Result<Statement, SignErrorKind> {
    let (algorithm, hex) = subject_digest.parts();
    Ok(Statement {
        statement_type: STATEMENT_TYPE_WRITTEN.to_owned(),
        subject: vec![Subject {
            name: subject_name.to_owned(),
            digest: BTreeMap::from([(algorithm.to_owned(), hex.to_owned())]),
        }],
        predicate_type: predicate_type.uri().to_owned(),
        predicate: predicate_type
            .wrap(predicate, now)
            .map_err(|error| SignErrorKind::Internal(Box::new(error)))?,
    })
}

/// Parses a verified payload.
///
/// The bytes a signature covers are always `DsseEnvelope::payload`, never a
/// re-serialization of the returned [`Statement`] — the parse is tolerant and
/// drops unknown fields, so re-serializing would hand a verifier different
/// bytes than the ones that were signed. This is the sigstore-rs defect the
/// ADR names, and the reason `predicate` stays a [`RawValue`].
///
/// **Preconditions.** `payload` is already bounded: it reaches here decoded
/// from `DsseEnvelope::parse`, which enforces `MAX_STATEMENT_PAYLOAD_BYTES`
/// (checklist row 16). A caller obtaining a payload by any other route owes
/// that bound itself.
///
/// # Errors
///
/// Rejects malformed JSON and any `_type` outside `ACCEPTED_STATEMENT_TYPES`.
pub(crate) fn parse(payload: &[u8]) -> Result<Statement, VerifyErrorKind> {
    let wire: WireStatement = serde_json::from_slice(payload).map_err(|_| VerifyErrorKind::BundleParseFailed)?;

    // Row 18, in the D-b form: a closed two-element allowlist, because cosign
    // v3 still writes v0.1 and refusing it would refuse every cosign-produced
    // attestation in existence.
    if !ACCEPTED_STATEMENT_TYPES.contains(&wire.statement_type.as_str()) {
        return Err(VerifyErrorKind::StatementTypeUnsupported {
            statement_type: wire.statement_type,
        });
    }

    Ok(Statement {
        statement_type: wire.statement_type,
        subject: wire.subject,
        predicate_type: wire.predicate_type,
        predicate: wire.predicate,
    })
}

/// Checks that some subject binds `target` by `sha256` (checklist rows 4/5/6).
///
/// # Errors
///
/// Three distinct refusals: no subjects at all, no `sha256` key in any digest
/// set, or a `sha256` present that names another artifact. Each names at most
/// [`MAX_REPORTED_SUBJECTS`] of what it saw.
pub(crate) fn binds_subject(statement: &Statement, target: &Digest) -> Result<(), VerifyErrorKind> {
    // Row 5: nothing to compare is a different diagnosis from comparing and
    // disagreeing, so it gets its own refusal.
    if statement.subject.is_empty() {
        return Err(VerifyErrorKind::StatementSubjectAbsent);
    }

    // Row 6: `sha256` is hardcoded. A co-present weaker algorithm never
    // satisfies the check, so a collision cannot stand in for the binding.
    let mut found = Vec::new();
    let mut found_total = 0usize;
    for subject in &statement.subject {
        match subject.digest.get(SUBJECT_DIGEST_ALGORITHM) {
            // Row 4: every subject is checked, not `subject[0]` alone.
            Some(hex) if target.algorithm().prefix() == SUBJECT_DIGEST_ALGORITHM && hex == target.hex() => {
                return Ok(());
            }
            Some(hex) => {
                found_total = found_total.saturating_add(1);
                if found.len() < MAX_REPORTED_SUBJECTS {
                    found.push(format!("{SUBJECT_DIGEST_ALGORITHM}:{hex}"));
                }
            }
            None => {}
        }
    }

    if found.is_empty() {
        let mut algorithms: Vec<String> = statement
            .subject
            .iter()
            .flat_map(|subject| subject.digest.keys().cloned())
            .collect();
        // `dedup` only drops adjacent repeats, and the same algorithm can
        // appear under two subjects without being adjacent.
        algorithms.sort();
        algorithms.dedup();
        algorithms.truncate(MAX_REPORTED_SUBJECTS);
        return Err(VerifyErrorKind::StatementSubjectWeakAlgorithm { algorithms });
    }

    let omitted = found_total.saturating_sub(found.len());
    if omitted > 0 {
        found.push(format!("and {omitted} more"));
    }
    Err(VerifyErrorKind::StatementSubjectMismatch {
        expected: target.to_string(),
        actual: found.join(", "),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci::attest::{ACCEPTED_STATEMENT_TYPES, STATEMENT_TYPE_WRITTEN};

    /// Byte fidelity has three axes here: a trailing-zero float, an escaped
    /// non-ASCII codepoint, and irregular whitespace. Key order is deliberately
    /// not one of them — `serde_json`'s `preserve_order` is on workspace-wide,
    /// so key order survives a round-trip and discriminates nothing.
    const AWKWARD_PREDICATE: &str = concat!(
        "{\n",
        "  \"specVersion\" : \"1.5\",\n",
        "    \"zebra\": 1.50,\n",
        "  \"unicode\": \"caf\\u00e9\",\n",
        "  \"nested\": [ 1,2 ,3 ]\n",
        "}"
    );

    const HEX_A: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const HEX_B: &str = "2222222222222222222222222222222222222222222222222222222222222222";

    fn raw(text: &str) -> Box<RawValue> {
        RawValue::from_string(text.to_owned()).expect("test input is valid JSON")
    }

    fn fixed_time() -> DateTime<Utc> {
        "2026-08-20T09:41:07Z".parse().expect("literal is RFC 3339")
    }

    fn subject(name: &str, pairs: &[(&str, &str)]) -> Subject {
        Subject {
            name: name.to_owned(),
            digest: pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect(),
        }
    }

    fn statement_with(subjects: Vec<Subject>) -> Statement {
        Statement {
            statement_type: STATEMENT_TYPE_WRITTEN.to_owned(),
            subject: subjects,
            predicate_type: "https://cyclonedx.org/bom".to_owned(),
            predicate: raw("{}"),
        }
    }

    fn payload(statement_type: &str, predicate: &str) -> String {
        format!(
            r#"{{"_type":"{statement_type}","subject":[{{"name":"pkg","digest":{{"sha256":"{HEX_A}"}}}}],"predicateType":"https://cyclonedx.org/bom","predicate":{predicate}}}"#
        )
    }

    // ---- build -----------------------------------------------------------

    #[test]
    fn build_writes_the_v1_statement_type() {
        // D-b: strict producer. v0.1 is accepted on read and never written.
        let built = build(
            "pkg",
            &Digest::Sha256(HEX_A.into()),
            &PredicateType::CycloneDx,
            &raw("{}"),
            fixed_time(),
        )
        .expect("build succeeds");
        assert_eq!(built.statement_type, "https://in-toto.io/Statement/v1");
    }

    #[test]
    fn build_names_the_subject_and_binds_it_by_bare_hex() {
        let built = build(
            "ghcr.io/acme/tool",
            &Digest::Sha256(HEX_A.into()),
            &PredicateType::CycloneDx,
            &raw("{}"),
            fixed_time(),
        )
        .expect("build succeeds");
        assert_eq!(built.subject, vec![subject("ghcr.io/acme/tool", &[("sha256", HEX_A)])]);
    }

    #[test]
    fn build_resolves_the_predicate_type_uri() {
        let built = build(
            "pkg",
            &Digest::Sha256(HEX_A.into()),
            &PredicateType::SlsaProvenance1,
            &raw("{}"),
            fixed_time(),
        )
        .expect("build succeeds");
        assert_eq!(built.predicate_type, "https://slsa.dev/provenance/v1");
    }

    #[test]
    fn build_splices_the_predicate_bytes_verbatim() {
        // D-b byte fidelity: whatever the --predicate file held is what gets
        // signed. A `Value` round-trip here would re-spell 1.50 and the escape.
        let built = build(
            "pkg",
            &Digest::Sha256(HEX_A.into()),
            &PredicateType::CycloneDx,
            &raw(AWKWARD_PREDICATE),
            fixed_time(),
        )
        .expect("build succeeds");
        assert_eq!(built.predicate.get(), AWKWARD_PREDICATE);
        assert!(
            serde_json::to_string(&built)
                .expect("serializes")
                .contains(AWKWARD_PREDICATE),
            "the serialized Statement must carry the predicate bytes unchanged"
        );
    }

    #[test]
    fn build_wraps_a_custom_predicate_and_still_embeds_it_verbatim() {
        let built = build(
            "pkg",
            &Digest::Sha256(HEX_A.into()),
            &PredicateType::Custom,
            &raw(AWKWARD_PREDICATE),
            fixed_time(),
        )
        .expect("build succeeds");
        assert_eq!(
            built.predicate.get(),
            format!("{{\"Data\":{AWKWARD_PREDICATE},\"Timestamp\":\"2026-08-20T09:41:07Z\"}}")
        );
    }

    // ---- parse -----------------------------------------------------------

    #[test]
    fn parse_accepts_both_allowlisted_statement_types() {
        // The allowlist is the closed pair, so both must round-trip and the
        // test must fail if either is dropped.
        for accepted in ACCEPTED_STATEMENT_TYPES {
            let parsed = parse(payload(accepted, "{}").as_bytes()).expect("an allowlisted _type parses");
            assert_eq!(&parsed.statement_type, accepted);
        }
        assert_eq!(ACCEPTED_STATEMENT_TYPES.len(), 2, "D-b pins a two-element allowlist");
    }

    #[test]
    fn parse_refuses_an_unlisted_statement_type() {
        // Checklist row 18, in its documented D-b form.
        let wire = payload("https://in-toto.io/Statement/v2", "{}");
        assert!(matches!(
            parse(wire.as_bytes()),
            Err(VerifyErrorKind::StatementTypeUnsupported { statement_type })
                if statement_type == "https://in-toto.io/Statement/v2"
        ));
    }

    #[test]
    fn parse_refuses_malformed_json() {
        assert!(matches!(parse(b"{not json"), Err(VerifyErrorKind::BundleParseFailed)));
    }

    #[test]
    fn parse_refuses_a_payload_missing_the_predicate() {
        let wire = r#"{"_type":"https://in-toto.io/Statement/v1","subject":[],"predicateType":"x"}"#;
        assert!(matches!(
            parse(wire.as_bytes()),
            Err(VerifyErrorKind::BundleParseFailed)
        ));
    }

    #[test]
    fn parse_borrows_the_predicate_as_the_exact_sub_slice() {
        // Checklist row 2 on the read side: `sbom --output` writes these bytes
        // back out, so a normalizing parse would silently rewrite the document.
        let parsed = parse(payload(STATEMENT_TYPE_WRITTEN, AWKWARD_PREDICATE).as_bytes()).expect("parses");
        assert_eq!(parsed.predicate.get(), AWKWARD_PREDICATE);
    }

    #[test]
    fn a_built_statement_survives_a_serialize_parse_round_trip() {
        let built = build(
            "pkg",
            &Digest::Sha256(HEX_A.into()),
            &PredicateType::CycloneDx,
            &raw(AWKWARD_PREDICATE),
            fixed_time(),
        )
        .expect("build succeeds");
        let bytes = serde_json::to_vec(&built).expect("serializes");
        assert_eq!(parse(&bytes).expect("re-parses"), built);
    }

    // ---- binds_subject ---------------------------------------------------

    #[test]
    fn binding_accepts_a_matching_sha256() {
        let statement = statement_with(vec![subject("pkg", &[("sha256", HEX_A)])]);
        assert!(binds_subject(&statement, &Digest::Sha256(HEX_A.into())).is_ok());
    }

    #[test]
    fn binding_checks_every_subject_not_only_the_first() {
        // Checklist row 4: sigstore-rs reads subject[0] only. A multi-subject
        // statement whose target sits at [1] must still bind.
        let statement = statement_with(vec![
            subject("other", &[("sha256", HEX_B)]),
            subject("pkg", &[("sha256", HEX_A)]),
        ]);
        assert!(binds_subject(&statement, &Digest::Sha256(HEX_A.into())).is_ok());
    }

    #[test]
    fn binding_refuses_a_statement_with_no_subject() {
        // Checklist row 5. Distinct from a mismatch: nothing to compare.
        assert!(matches!(
            binds_subject(&statement_with(vec![]), &Digest::Sha256(HEX_A.into())),
            Err(VerifyErrorKind::StatementSubjectAbsent)
        ));
    }

    #[test]
    fn binding_refuses_an_attestation_for_another_artifact() {
        // Checklist row 4, CVE-2026-31830 shape: a valid attestation for A
        // served as a referrer of B.
        let statement = statement_with(vec![subject("other", &[("sha256", HEX_B)])]);
        assert!(matches!(
            binds_subject(&statement, &Digest::Sha256(HEX_A.into())),
            Err(VerifyErrorKind::StatementSubjectMismatch { expected, actual })
                if expected.contains(HEX_A) && actual.contains(HEX_B)
        ));
    }

    #[test]
    fn binding_refuses_a_digest_set_with_no_sha256() {
        // Checklist row 6: an unusable DigestSet is refused, never matched on
        // what it does carry.
        let statement = statement_with(vec![subject("pkg", &[("md5", "abcd")])]);
        assert!(matches!(
            binds_subject(&statement, &Digest::Sha256(HEX_A.into())),
            Err(VerifyErrorKind::StatementSubjectWeakAlgorithm { algorithms })
                if algorithms == vec!["md5".to_owned()]
        ));
    }

    #[test]
    fn a_refusal_names_at_most_eight_subjects_and_counts_the_rest() {
        // A hostile Statement fits ~466k subjects inside the payload cap. Both
        // the message and the structured field are bounded where they are
        // built, so `--json` cannot carry the unbounded form either.
        let subjects = (0..20)
            .map(|n| subject("pkg", &[("sha256", &format!("{n:064}"))]))
            .collect();
        let Err(VerifyErrorKind::StatementSubjectMismatch { actual, .. }) =
            binds_subject(&statement_with(subjects), &Digest::Sha256(HEX_A.into()))
        else {
            panic!("20 non-matching subjects must refuse as a mismatch");
        };
        assert_eq!(actual.matches("sha256:").count(), MAX_REPORTED_SUBJECTS);
        assert!(actual.ends_with("and 12 more"), "got {actual}");
    }

    #[test]
    fn a_weak_algorithm_refusal_bounds_the_reported_algorithms() {
        let subjects = (0..20)
            .map(|n| subject("pkg", &[(&format!("alg{n:02}"), "aa")]))
            .collect();
        assert!(matches!(
            binds_subject(&statement_with(subjects), &Digest::Sha256(HEX_A.into())),
            Err(VerifyErrorKind::StatementSubjectWeakAlgorithm { algorithms })
                if algorithms.len() == MAX_REPORTED_SUBJECTS
        ));
    }

    #[test]
    fn the_reported_algorithms_are_sorted_and_deduplicated() {
        let statement = statement_with(vec![
            subject("a", &[("sha1", "aa"), ("md5", "bb")]),
            subject("b", &[("md5", "cc")]),
        ]);
        assert!(matches!(
            binds_subject(&statement, &Digest::Sha256(HEX_A.into())),
            Err(VerifyErrorKind::StatementSubjectWeakAlgorithm { algorithms })
                if algorithms == vec!["md5".to_owned(), "sha1".to_owned()]
        ));
    }

    #[test]
    fn a_matching_weak_algorithm_never_satisfies_the_binding() {
        // Checklist row 6, the co-present case: the md5 entry carries exactly
        // the target hex and the sha256 entry does not. Matching on the weaker
        // one would let a collision stand in for the binding, so this must
        // refuse even though a digest in the set does equal the target.
        let statement = statement_with(vec![subject("pkg", &[("md5", HEX_A), ("sha256", HEX_B)])]);
        assert!(matches!(
            binds_subject(&statement, &Digest::Sha256(HEX_A.into())),
            Err(VerifyErrorKind::StatementSubjectMismatch { .. })
        ));
    }

    #[test]
    fn binding_a_non_sha256_target_is_refused_rather_than_matched() {
        // The subject key is hardcoded `sha256`, so a sha512 target can never
        // bind — refused loudly rather than silently compared against nothing.
        let statement = statement_with(vec![subject("pkg", &[("sha512", HEX_A)])]);
        assert!(matches!(
            binds_subject(&statement, &Digest::Sha512(HEX_A.into())),
            Err(VerifyErrorKind::StatementSubjectWeakAlgorithm { .. })
        ));
    }
}
