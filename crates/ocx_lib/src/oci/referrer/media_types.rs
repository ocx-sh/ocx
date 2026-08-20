// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Const table of OCI media types and artifact types used by the referrers
//! subsystem.
//!
//! Values are data-only (see plan Step 1.1) — do not add logic. The empty
//! config digest and size are the SHA-256 and byte length of the literal
//! OCI empty-descriptor payload `{}`, embedded to avoid recomputation.

/// Sigstore bundle v0.3 artifact type, used as the `artifactType` on a
/// signature referrer manifest.
pub const SIGSTORE_BUNDLE_V03: &str = "application/vnd.dev.sigstore.bundle.v0.3+json";

/// Empty OCI config media type per the empty-descriptor convention
/// (OCI image spec §"Guidelines for Empty Descriptors").
pub const EMPTY_CONFIG: &str = "application/vnd.oci.empty.v1+json";

/// Canonical empty-config blob payload (the literal two bytes `{}`).
///
/// A referrer manifest's `config` descriptor points at this blob; a
/// spec-strict registry (e.g. zot) rejects the manifest with
/// `MANIFEST_INVALID` unless the blob has been pushed first. Single source of
/// truth for the bytes whose SHA-256 is [`EMPTY_CONFIG_DIGEST`] and whose
/// length is [`EMPTY_CONFIG_SIZE`].
pub const EMPTY_CONFIG_PAYLOAD: &[u8] = b"{}";

/// SHA-256 digest of the canonical empty config payload (`{}` + newline free).
///
/// Frozen per plan Step 1.3 and OCI image-spec §"Guidelines for Empty
/// Descriptors".
pub const EMPTY_CONFIG_DIGEST: &str = "sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a";

/// Byte length of the canonical empty config payload (literal `{}`).
pub const EMPTY_CONFIG_SIZE: u64 = 2;

// ── Sigstore bundle referrer annotations ──────────────────────────────
//
// cosign's `WriteAttestationNewBundleFormat` writes exactly these three keys
// onto a bundle referrer manifest, and OCX writes all three so the same
// artifact carries one wire shape across both tools (ADR D1). The set is a
// one-way door: the manifest's SHA-256 *is* the referrer's registry address,
// so bytes already pushed can never be migrated.
//
// The `created` key is [`crate::oci::annotations::CREATED`] — an existing
// OCI-spec annotation constant, not redeclared here.

/// Which `content` oneof the bundle carries — the listing-time hint that tells
/// a signature referrer from an attestation referrer without fetching the blob.
pub(crate) const ANNOTATION_BUNDLE_CONTENT: &str = "dev.sigstore.bundle.content";

/// The resolved predicateType URI of an attestation. Written on attestation
/// referrers only; a signature has no predicate.
pub(crate) const ANNOTATION_BUNDLE_PREDICATE_TYPE: &str = "dev.sigstore.bundle.predicateType";

/// [`ANNOTATION_BUNDLE_CONTENT`] value for a DSSE-enveloped attestation.
///
/// Written by [`AttestPipeline`](crate::oci::attest::pipeline::AttestPipeline);
/// this is what tells an attestation referrer from a signature referrer in a
/// listing, without fetching the blob.
pub(crate) const BUNDLE_CONTENT_DSSE: &str = "dsse-envelope";

/// [`ANNOTATION_BUNDLE_CONTENT`] value for a signature bundle.
pub(crate) const BUNDLE_CONTENT_MESSAGE_SIGNATURE: &str = "message-signature";
