// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! In-toto attestations in DSSE envelopes, attached to a subject manifest as
//! OCI referrers (cosign-compatible). Design record:
//! [`adr_sbom_attestations.md`](../../../../.claude/artifacts/adr_sbom_attestations.md).

pub mod dsse;
pub mod pipeline;
pub mod predicate;
pub mod statement;

/// Raw bytes of one attestation bundle fetched from a registry.
///
/// Not the 512 KiB signature-bundle cap: an SBOM envelope is a different artifact class.
pub(crate) const MAX_ATTESTATION_ENVELOPE_BYTES: usize = 32 * 1024 * 1024;

/// Decoded in-toto Statement payload, checked from the base64 length before the decode buffer exists.
pub(crate) const MAX_STATEMENT_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;

/// Attestation referrers considered for one subject.
pub(crate) const MAX_ATTESTATION_CANDIDATES: usize = 32;

/// Cumulative attestation bytes fetched in one verify run; bounds the candidates x envelope product.
pub(crate) const MAX_TOTAL_ATTESTATION_BYTES: usize = 64 * 1024 * 1024;

/// Local `--predicate` file.
///
/// Kept 1 MiB below [`MAX_STATEMENT_PAYLOAD_BYTES`] for the Statement wrapper, or verify refuses what attest accepted.
/// Enforce with a bounded read, never `metadata().len()` then an unbounded one.
pub const MAX_PREDICATE_FILE_BYTES: usize = 15 * 1024 * 1024;

/// The one DSSE `payloadType` this version writes and accepts.
pub(crate) const DSSE_PAYLOAD_TYPE: &str = "application/vnd.in-toto+json";

/// The in-toto Statement `_type` OCX writes: the first accepted one.
pub(crate) const STATEMENT_TYPE_WRITTEN: &str = ACCEPTED_STATEMENT_TYPES[0];

/// Statement `_type` values accepted on verify; the first is the one written.
///
/// Dropping v0.1 refuses every cosign v3 attestation, which still writes it.
pub(crate) const ACCEPTED_STATEMENT_TYPES: &[&str] =
    &["https://in-toto.io/Statement/v1", "https://in-toto.io/Statement/v0.1"];

/// `(kind, version)` pairs accepted from a bundle's `tlogEntries[].kindVersion`; the first is the one written.
///
/// Never add `intoto:0.0.1`: its PayloadHash is relaxed.
pub(crate) const ACCEPTED_TLOG_KINDS: &[(&str, &str)] = &[("dsse", "0.0.1")];

/// The `(kind, version)` pair the sign side uploads: the first accepted one.
pub(crate) const TLOG_KIND_WRITTEN: (&str, &str) = ACCEPTED_TLOG_KINDS[0];

/// The `predicateType` cosign v3 writes on an image-signature DSSE statement.
///
/// A constant, not a [`predicate::PredicateType`] variant, or `PredicateType::ALIASES` exposes it as an `attest --type` value.
pub const COSIGN_SIGN_PREDICATE_TYPE: &str = "https://sigstore.dev/cosign/sign/v1";

#[cfg(test)]
mod tests {
    use super::*;

    /// cosign's own signature referrer, captured in G0. `include_str!` rather
    /// than a runtime read: a moved fixture becomes a compile error.
    const KEYLESS_REFERRER_MANIFEST: &str =
        include_str!("../../../test/tests/fixtures/golden/keyless_referrer_manifest.json");

    /// T-12. The predicate type is what cosign v3.1.1 actually annotated its
    /// signature referrer with, not a value read off a spec page.
    #[test]
    fn cosign_sign_predicate_type_matches_the_golden_referrer() {
        let manifest: serde_json::Value =
            serde_json::from_str(KEYLESS_REFERRER_MANIFEST).expect("golden referrer manifest is JSON");
        let annotated = manifest
            .pointer("/annotations/dev.sigstore.bundle.predicateType")
            .and_then(serde_json::Value::as_str)
            .expect("golden referrer annotates a predicateType");

        assert_eq!(annotated, COSIGN_SIGN_PREDICATE_TYPE);
    }
}
