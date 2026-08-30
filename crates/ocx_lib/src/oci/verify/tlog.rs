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
//! Plus one assertion *about* the entry rather than over it:
//! [`verify_integrated_time_within_certificate`] re-checks that the entry's
//! `integratedTime` falls inside the signing certificate's validity window.
//! It lives here because this is the path both content modes share, so "runs
//! for signatures and for attestations" is a structural fact rather than a
//! discipline.
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
use chrono::{DateTime, SecondsFormat};
use serde::Serialize;
use sigstore::crypto::verification_key::CosignVerificationKey;
use sigstore::crypto::{Signature, SigningScheme};
use sigstore::rekor::models::InclusionProof;
use sigstore::rekor::models::log_entry::RekorInclusionProof;
use sigstore_protobuf_specs::dev::sigstore::rekor::v1::InclusionProof as ProtoInclusionProof;
use x509_cert::Certificate;
use x509_cert::time::Time;

use super::error::VerifyErrorKind;
use super::signing_instant::SigningInstant;

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

/// Re-assert that the entry's `integratedTime` falls inside the signing
/// certificate's validity window.
///
/// Part III row 13 (CVE-2024-55655). The delegated `sigstore` verifier checks
/// this too; the duplication is the point, because that CVE is precisely a
/// library dropping the step. The window is **inclusive at both ends** —
/// `adr_sbom_attestations.md` states it as `NotBefore <= integratedTime <=
/// NotAfter`.
///
/// Takes the already-parsed leaf: `parse_certificate` runs once in
/// `verify_one_referrer`, so the window checked here is the window the identity
/// check read, not a second parse that could disagree.
///
/// The instant arrives as a [`SigningInstant`] rather than a bare `i64` so no
/// caller can hand it the wall clock: a Fulcio certificate is short-lived by
/// design, and "is this valid *now*" refuses every keyless signature older than
/// its ten-minute window. See `super::signing_instant` for the whole rule,
/// including why the no-transparency-log case is legal and supplies its own
/// instant.
pub(super) fn verify_integrated_time_within_certificate(
    signed_at: SigningInstant,
    leaf: &Certificate,
) -> Result<(), VerifyErrorKind> {
    let signed_at = signed_at.epoch_seconds();
    let validity = &leaf.tbs_certificate.validity;
    let not_before = unix_seconds(validity.not_before);
    let not_after = unix_seconds(validity.not_after);
    if signed_at < not_before || signed_at > not_after {
        return Err(VerifyErrorKind::CertificateValidityWindow {
            integrated_time: rfc3339_utc(signed_at),
            not_before: rfc3339_utc(not_before),
            not_after: rfc3339_utc(not_after),
        });
    }
    Ok(())
}

/// Seconds since the Unix epoch for an X.509 `Time`.
///
/// `to_unix_duration` is non-negative by construction — `x509-cert` floors
/// `UTCTime` at 1970 — so the only unrepresentable value is a `notAfter` past
/// year 292277026596, and saturating there refuses nothing a real certificate
/// asserts.
fn unix_seconds(time: Time) -> i64 {
    i64::try_from(time.to_unix_duration().as_secs()).unwrap_or(i64::MAX)
}

/// Render epoch seconds as RFC 3339 with an explicit `Z` (PLAT-31).
///
/// The fallback is the bare integer: a timestamp chrono cannot represent is
/// already outside every certificate window, so it only ever appears inside a
/// refusal, where an unformatted number still names the offending value.
fn rfc3339_utc(epoch_seconds: i64) -> String {
    DateTime::from_timestamp(epoch_seconds, 0).map_or_else(
        || epoch_seconds.to_string(),
        |at| at.to_rfc3339_opts(SecondsFormat::Secs, true),
    )
}

#[cfg(test)]
/// A real P-256 self-signed CA certificate whose validity window is exactly
/// `2026-01-01T00:00:00Z` .. `2026-01-01T00:10:00Z` — ten minutes, the
/// shape Fulcio issues. Parsed by the same `x509-cert` code the pipeline
/// uses, so the boundary cases below are asserted against real DER rather
/// than a hand-built struct that could encode a window no parser produces.
///
/// Regenerate with:
/// `openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
///   -keyout /dev/null -outform DER -subj /CN=ocx-row13-fixture \
///   -not_before 260101000000Z -not_after 260101001000Z | base64 -w0`
const FIXTURE_CERT_DER_BASE64: &str = "MIIBjjCCATOgAwIBAgIUXDerMK9Jof8dxErPo1pTx55fDskwCgYIKoZIzj0EAwIwHDEaMBgGA1UEAwwRb2N4LXJvdzEzLWZpeHR1cmUwHhcNMjYwMTAxMDAwMDAwWhcNMjYwMTAxMDAxMDAwWjAcMRowGAYDVQQDDBFvY3gtcm93MTMtZml4dHVyZTBZMBMGByqGSM49AgEGCCqGSM49AwEHA0IABFi6Pl8zq1kkrEGV8nr66Trdd7QM0BKnLL0JHXFlZ3rSSW16yLZV7td8RAo0Mqo/VApbH7TeA/bXmByIGzn+8mijUzBRMB0GA1UdDgQWBBQf1NnnrXUcU+VMImU74mm+zuysXjAfBgNVHSMEGDAWgBQf1NnnrXUcU+VMImU74mm+zuysXjAPBgNVHRMBAf8EBTADAQH/MAoGCCqGSM49BAMCA0kAMEYCIQC+ZkSuPm9qPlJ4GftxDoyvXgo6yKt9zdSrfmsewd+B+gIhAMFZQ7iOfDmrNir7vXT5fXAn6XmS/PCOesmjsnFa9ywB";

/// G0's keyless golden bundle, verbatim. Its Fulcio certificate expired ten
/// minutes after capture, which is what makes it the regression fixture for
/// "validity anchors to signing time" — see `super::signing_instant`.
#[cfg(test)]
const GOLDEN_KEYLESS_BUNDLE: &str = include_str!("../../../../../test/tests/fixtures/golden/keyless_bundle.json");

/// DER bytes of [`FIXTURE_CERT_DER_BASE64`].
#[cfg(test)]
pub(super) fn fixture_certificate_der() -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(FIXTURE_CERT_DER_BASE64)
        .expect("fixture is valid base64")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `2026-01-01T00:00:00Z` — the fixture's `notBefore`.
    const NOT_BEFORE: i64 = 1_767_225_600;
    /// `2026-01-01T00:10:00Z` — the fixture's `notAfter`.
    const NOT_AFTER: i64 = 1_767_226_200;

    fn fixture_certificate() -> Certificate {
        use x509_cert::der::Decode as _;
        Certificate::from_der(&fixture_certificate_der()).expect("fixture is a valid X.509 certificate")
    }

    /// The window the fixture actually encodes, asserted before anything is
    /// built on it. Without this the boundary tests below could pass against a
    /// window nobody checked, and a regenerated fixture would move the goalposts
    /// silently.
    #[test]
    fn the_fixture_window_is_the_one_the_boundary_tests_assume() {
        let cert = fixture_certificate();
        let validity = &cert.tbs_certificate.validity;
        assert_eq!(validity.not_before.to_unix_duration().as_secs(), NOT_BEFORE as u64);
        assert_eq!(validity.not_after.to_unix_duration().as_secs(), NOT_AFTER as u64);
    }

    #[test]
    fn an_integrated_time_inside_the_window_verifies() {
        let cert = fixture_certificate();
        let verdict =
            verify_integrated_time_within_certificate(SigningInstant::TransparencyLog(NOT_BEFORE + 300), &cert);
        assert!(verdict.is_ok(), "{verdict:?}");
    }

    /// Inclusive lower bound. `adr_sbom_attestations.md` states row 13 as
    /// `NotBefore <= integratedTime <= NotAfter`, so a signature made in the
    /// certificate's first second is valid, not a boundary refusal.
    #[test]
    fn an_integrated_time_exactly_at_not_before_verifies() {
        let cert = fixture_certificate();
        let verdict = verify_integrated_time_within_certificate(SigningInstant::TransparencyLog(NOT_BEFORE), &cert);
        assert!(verdict.is_ok(), "notBefore is inclusive: {verdict:?}");
    }

    /// Inclusive upper bound, same ADR sentence.
    #[test]
    fn an_integrated_time_exactly_at_not_after_verifies() {
        let cert = fixture_certificate();
        let verdict = verify_integrated_time_within_certificate(SigningInstant::TransparencyLog(NOT_AFTER), &cert);
        assert!(verdict.is_ok(), "notAfter is inclusive: {verdict:?}");
    }

    /// One second below the window. Paired with the exactly-at test above, this
    /// is what distinguishes an inclusive bound from an exclusive one — either
    /// test alone passes under both readings.
    #[test]
    fn an_integrated_time_one_second_before_the_window_is_refused() {
        let cert = fixture_certificate();
        let verdict = verify_integrated_time_within_certificate(SigningInstant::TransparencyLog(NOT_BEFORE - 1), &cert);
        assert!(
            matches!(verdict, Err(VerifyErrorKind::CertificateValidityWindow { .. })),
            "{verdict:?}"
        );
    }

    /// One second above the window — the CVE-2024-55655 shape: an entry logged
    /// after the ephemeral certificate expired. Asserts the reported fields,
    /// not just the variant, because all three are RFC 3339 with an explicit
    /// `Z` (PLAT-31) and a wrong rendering would only surface to a user.
    #[test]
    fn an_integrated_time_one_second_after_the_window_is_refused_naming_the_window() {
        let cert = fixture_certificate();
        let verdict = verify_integrated_time_within_certificate(SigningInstant::TransparencyLog(NOT_AFTER + 1), &cert);
        let Err(VerifyErrorKind::CertificateValidityWindow {
            integrated_time,
            not_before,
            not_after,
        }) = verdict
        else {
            panic!("expected a validity-window refusal, got {verdict:?}");
        };
        assert_eq!(integrated_time, "2026-01-01T00:10:01Z");
        assert_eq!(not_before, "2026-01-01T00:00:00Z");
        assert_eq!(not_after, "2026-01-01T00:10:00Z");
    }

    /// The G0 keyless golden fixture, end to end: the real Fulcio certificate
    /// and the real `integratedTime` that signed against it.
    ///
    /// This is the trap the type exists for. The fixture's window is ten
    /// minutes wide (`2026-08-29T02:07:54Z .. 02:17:54Z`) and long gone, so a
    /// check reading the clock refuses a legitimately signed artifact. Both
    /// halves are asserted against the *same* certificate: the entry's own
    /// instant verifies, and an instant a day past `notAfter` is refused. One
    /// alone proves nothing — an always-Ok guard passes the first, an
    /// always-Err guard passes the second, and only the pair shows the verdict
    /// tracks the instant it was handed.
    #[test]
    fn the_golden_keyless_certificate_verifies_at_its_logged_instant_and_is_refused_later() {
        use x509_cert::der::Decode as _;

        let bundle: serde_json::Value =
            serde_json::from_str(GOLDEN_KEYLESS_BUNDLE).expect("the golden keyless bundle is JSON");
        let material = &bundle["verificationMaterial"];
        let leaf_der = base64::engine::general_purpose::STANDARD
            .decode(
                material["certificate"]["rawBytes"]
                    .as_str()
                    .expect("the bundle carries a leaf certificate"),
            )
            .expect("the leaf certificate is base64");
        let cert = Certificate::from_der(&leaf_der).expect("the leaf certificate is valid X.509");
        // protobuf JSON renders an int64 as a string.
        let integrated_time: i64 = material["tlogEntries"][0]["integratedTime"]
            .as_str()
            .expect("the bundle carries a tlog entry")
            .parse()
            .expect("integratedTime is an integer");

        // Pin the window the two assertions below depend on, so a regenerated
        // fixture moves the goalposts loudly instead of silently.
        let validity = &cert.tbs_certificate.validity;
        let not_before = i64::try_from(validity.not_before.to_unix_duration().as_secs()).expect("fits i64");
        let not_after = i64::try_from(validity.not_after.to_unix_duration().as_secs()).expect("fits i64");
        assert_eq!(rfc3339_utc(not_before), "2026-08-29T02:07:54Z");
        assert_eq!(rfc3339_utc(not_after), "2026-08-29T02:17:54Z");
        assert!(
            (not_before..=not_after).contains(&integrated_time),
            "the fixture was signed inside its own window"
        );

        let at_signing_time =
            verify_integrated_time_within_certificate(SigningInstant::TransparencyLog(integrated_time), &cert);
        assert!(
            at_signing_time.is_ok(),
            "an expired Fulcio certificate still verifies at its logged signing time: {at_signing_time:?}"
        );

        // A day past expiry — what a clock read would produce for this fixture,
        // and (since the `CallerSupplied` variant was deleted) reachable only as
        // a logged instant, which is the shape the guard now takes.
        let long_after =
            verify_integrated_time_within_certificate(SigningInstant::TransparencyLog(not_after + 86_400), &cert);
        assert!(
            matches!(long_after, Err(VerifyErrorKind::CertificateValidityWindow { .. })),
            "an instant past notAfter must be refused: {long_after:?}"
        );
    }

    /// A bundle carrying no `integratedTime` decodes it as protobuf's zero, and
    /// `BundleParts` widens that to `0`. Epoch zero is outside every real
    /// certificate window, so absence fails closed through the ordinary
    /// comparison — there is no separate "missing" branch to forget to write.
    #[test]
    fn a_missing_integrated_time_reads_as_epoch_zero_and_is_refused() {
        let cert = fixture_certificate();
        let verdict = verify_integrated_time_within_certificate(SigningInstant::TransparencyLog(0), &cert);
        assert!(
            matches!(verdict, Err(VerifyErrorKind::CertificateValidityWindow { .. })),
            "an absent integratedTime must not verify: {verdict:?}"
        );
    }

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
