// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! In-toto attestations carried in DSSE envelopes and attached to a subject
//! manifest as OCI referrers (cosign-compatible).
//!
//! See
//! [`adr_sbom_attestations.md`](../../../../.claude/artifacts/adr_sbom_attestations.md)
//! for the design record.
//!
//! # Module layout
//!
//! | Module | Purpose |
//! |---|---|
//! | [`dsse`] | PAE encoding, the [`dsse::DsseEnvelope`] wire shape, sign-side envelope hashes |
//! | [`pipeline`] | [`pipeline::AttestPipeline`] — the push-side attach state machine |
//! | [`predicate`] | [`predicate::PredicateType`] vocabulary, the cosign wrapper, the SLSA builder accessor |
//! | [`statement`] | in-toto [`statement::Statement`] build, parse, and subject binding |
//!
//! # Bounds
//!
//! The `MAX_*` constants bound every untrusted byte count on the attestation
//! path. None is configurable in v1, and each has a dedicated error variant
//! naming it, so a limit trip is a caller decision point — hostile input (stop)
//! versus transient I/O (retry) — rather than a generic parse failure.
//!
//! Every constant here is `pub(crate)` except `MAX_PREDICATE_FILE_BYTES`, which
//! stays `pub` because it is enforced by the CLI's `--predicate` read, in a
//! different crate.

pub mod dsse;
pub mod pipeline;
pub mod predicate;
pub mod statement;

/// Raw bytes of one attestation bundle fetched from a registry.
///
/// 32 MiB: two orders above the largest realistic CycloneDX SBOM for a binary
/// package, two orders below a memory hazard. Deliberately NOT the 512 KiB
/// signature-bundle cap — a different artifact class gets its own bound.
/// Not configurable in v1.
pub(crate) const MAX_ATTESTATION_ENVELOPE_BYTES: usize = 32 * 1024 * 1024;

/// Decoded in-toto Statement payload.
///
/// Checked from the base64 length BEFORE allocating the decode buffer (base64
/// expands at a fixed 4/3, so the decoded size is known in advance). Tighter
/// than the envelope cap on purpose: this is the number a document author can
/// reason about. Not configurable in v1.
pub(crate) const MAX_STATEMENT_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;

/// Attestation referrers considered for one subject.
///
/// Larger than `MAX_SIGNATURE_CANDIDATES` (8) because attestations legitimately
/// fan out — one per predicate type per producer. Not configurable in v1.
pub(crate) const MAX_ATTESTATION_CANDIDATES: usize = 32;

/// Cumulative attestation bytes fetched in one verify run.
///
/// Closes the candidates x per-envelope product, which neither cap closes
/// alone. Not configurable in v1.
pub(crate) const MAX_TOTAL_ATTESTATION_BYTES: usize = 64 * 1024 * 1024;

/// Local `--predicate` file.
///
/// Deliberately 1 MiB BELOW [`MAX_STATEMENT_PAYLOAD_BYTES`]: the Statement wraps
/// the predicate in `_type`, `predicateType` and a `subject` array, so an
/// at-the-limit predicate would produce an over-limit payload and verify would
/// refuse what attest accepted. The 1 MiB is the wrapper reserve. Enforced by a
/// bounded read, never a `metadata().len()` check followed by an unbounded one.
/// Not configurable in v1.
pub const MAX_PREDICATE_FILE_BYTES: usize = 15 * 1024 * 1024;

/// The one DSSE `payloadType` this version writes and accepts.
pub(crate) const DSSE_PAYLOAD_TYPE: &str = "application/vnd.in-toto+json";

/// The in-toto Statement `_type` OCX writes.
pub(crate) const STATEMENT_TYPE_WRITTEN: &str = "https://in-toto.io/Statement/v1";

/// Statement `_type` values accepted on verify.
///
/// Strict producer, tolerant consumer: cosign v3 still writes v0.1, and
/// rejecting it would refuse every cosign-produced attestation in existence.
/// The two shapes differ only in this string — v1's ResourceDescriptor adds
/// optional fields and keeps `name` + `digest` — so acceptance is a closed
/// two-element allowlist, not a second parser.
pub(crate) const ACCEPTED_STATEMENT_TYPES: &[&str] =
    &["https://in-toto.io/Statement/v1", "https://in-toto.io/Statement/v0.1"];

/// `(kind, version)` pairs accepted from a bundle's `tlogEntries[].kindVersion`.
///
/// One entry, deliberately. `intoto:0.0.1` has a relaxed PayloadHash;
/// `intoto:0.0.2`'s canonicalization is unsourced AND unreachable through
/// sigstore's `tlog_entry_for_dsse`; `hashedrekord:0.0.2` is Rekor v2.
pub(crate) const ACCEPTED_TLOG_KINDS: &[(&str, &str)] = &[("dsse", "0.0.1")];

/// The `(kind, version)` pair the sign side uploads.
pub(crate) const TLOG_KIND_WRITTEN: (&str, &str) = ("dsse", "0.0.1");
