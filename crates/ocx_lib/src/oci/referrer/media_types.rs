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

/// CycloneDX JSON SBOM, used as the `artifactType` on an **unsigned** SBOM
/// referrer manifest and as its layer's `mediaType`.
///
/// The unsigned attach path writes the SBOM document itself as the referrer
/// payload and types it by what it is, which is what `cosign attach sbom`,
/// `oras attach` and `syft attest --output` all do. A signed attach keeps
/// [`SIGSTORE_BUNDLE_V03`] instead: there the payload is a bundle, and the
/// SBOM's own type is the DSSE `predicateType` inside it.
pub const SBOM_CYCLONEDX: &str = "application/vnd.cyclonedx+json";

/// SPDX in its JSON serialization (`spdxjson`).
pub const SBOM_SPDX_JSON: &str = "application/spdx+json";

/// SPDX in its tag-value text serialization (`spdx`).
///
/// The one SBOM type here that is not JSON, which is why the read path treats
/// a referrer payload as opaque bytes rather than as a `RawValue`.
pub const SBOM_SPDX_TEXT: &str = "text/spdx";

/// Every artifact type an unsigned SBOM referrer may declare.
///
/// The read path filters a listing against this set and refuses a payload
/// layer typed outside it; the attach path picks one entry per `--type`.
pub const SBOM_ARTIFACT_TYPES: &[&str] = &[SBOM_CYCLONEDX, SBOM_SPDX_JSON, SBOM_SPDX_TEXT];

// ── cosign's own SBOM layer spellings ─────────────────────────────────
//
// The three constants above are what OCX *writes*; these two are what cosign
// v3.1.1 writes and OCX must therefore *read*. The two sets are deliberately
// not unified: widening what OCX emits would change a wire format for no
// reason, while refusing to read cosign's spelling would leave the parity
// reader blind to cosign's own default output.
//
// Measured, `cosign attach sbom --sbom DOC --type T [--input-format F]`, from
// the `mediaType [...]` cosign prints for the layer it uploads:
//
//   type=spdx      format=json (auto for .json)  -> `text/spdx+json`
//   type=spdx      format=text                   -> `text/spdx`          (SBOM_SPDX_TEXT)
//   type=cyclonedx format=json (auto)            -> `application/vnd.cyclonedx+json` (SBOM_CYCLONEDX)
//   type=cyclonedx format=xml                    -> `application/vnd.cyclonedx+xml`
//   type=syft      format=json                   -> `application/vnd.syft+json`
//
// `application/spdx+json` — OCX's own SPDX-JSON spelling — is *not* in that
// table, and `--type spdx` is cosign's default, so `text/spdx+json` is the
// single most likely `.sbom` layer type in the wild.
//
// syft is absent on purpose rather than forgotten: there is no in-toto
// predicateType URI for syft's native format, so it has nothing to be labelled
// with and is refused by name (`sbom_media_type_unsupported`) rather than
// listed under a URI nobody claimed.

/// SPDX in its JSON serialization, as `cosign attach sbom --type spdx` types it.
///
/// Not [`SBOM_SPDX_JSON`]: cosign derives this from its own `text/spdx` by
/// appending `+json` rather than from the registered `application/spdx+json`.
/// Both name the same document, and [`sbom_predicate_type_uri`] maps them to
/// one predicateType.
///
/// [`sbom_predicate_type_uri`]: crate::oci::attest::predicate::sbom_predicate_type_uri
pub const COSIGN_SBOM_SPDX_JSON: &str = "text/spdx+json";

/// CycloneDX in its XML serialization, as `cosign attach sbom --type cyclonedx
/// --input-format xml` types it.
///
/// Listed and labelled like any other CycloneDX document. Not summarizable:
/// `crate::sbom` parses CycloneDX **JSON** only, the same asymmetry
/// `adr_sbom_attestations.md` D2 records for SPDX.
pub const COSIGN_SBOM_CYCLONEDX_XML: &str = "application/vnd.cyclonedx+xml";

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

// ── cosign sidecar wire types ─────────────────────────────────────────
//
// The legacy `sha256-<hex>.sig` / `.att` / `.sbom` tag schema, which cosign
// 3.x still reads and which registries without the Referrers API still hand
// out. Measured against cosign v3.1.1; see the commit body for the commands.

/// Layer media type of a cosign simplesigning payload — the claim in
/// [`crate::oci::simplesigning`].
pub const SIMPLESIGNING_MEDIA_TYPE: &str = "application/vnd.dev.cosign.simplesigning.v1+json";

/// `artifactType` of a cosign signature referrer under the OCI 1.1 scheme.
///
/// Not what cosign 3.1.1's own `sign` writes — with a signing config in play
/// that path always emits [`SIGSTORE_BUNDLE_V03`]. This is the value from
/// cosign's SIGNATURE_SPEC, produced by the same
/// `application/vnd.dev.cosign.artifact.%s.v1+json` template whose `sbom`
/// instantiation was captured from a live registry.
pub const COSIGN_SIG_ARTIFACT_TYPE: &str = "application/vnd.dev.cosign.artifact.sig.v1+json";

/// `artifactType` of a cosign SBOM referrer under the OCI 1.1 scheme.
///
/// Measured: `COSIGN_EXPERIMENTAL=1 cosign attach sbom
/// --registry-referrers-mode oci-1-1` writes exactly this on the referrer
/// descriptor, while the layer keeps the SBOM's own type
/// ([`SBOM_CYCLONEDX`] and friends).
pub const COSIGN_SBOM_ARTIFACT_TYPE: &str = "application/vnd.dev.cosign.artifact.sbom.v1+json";

/// Layer media type of a DSSE envelope carried by a `.att` sidecar.
///
/// Measured, and the reason there is no third `artifactType` constant beside
/// the two above: cosign v3.1.1's `attach attestation` writes a
/// `sha256-<hex>.att` manifest whose one layer is typed this and which declares
/// **neither** `artifactType` **nor** `subject`, while `cosign attest` writes a
/// [`SIGSTORE_BUNDLE_V03`] referrer — the same type a signature referrer
/// carries. cosign publishes no attestation artifact type, so `.att` is a
/// tag-only shape; see `crate::oci::verify::attestation_sidecar`.
pub const DSSE_ENVELOPE_MEDIA_TYPE: &str = "application/vnd.dsse.envelope.v1+json";

// ── cosign sidecar annotations ────────────────────────────────────────
//
// The namespaces differ between the signature key and the rest. That is
// cosign's actual wire shape — measured in G0's golden fixtures and in the
// v3.1.1 binary's own string table — not a typo. Unifying them silently
// breaks interop with every signature cosign ever wrote.

/// Base64 signature over the simplesigning payload. Namespace
/// `dev.cosignproject.cosign` — note it differs from the three below.
pub const ANNOTATION_COSIGN_SIGNATURE: &str = "dev.cosignproject.cosign/signature";

/// PEM leaf certificate. Keyless only; its absence under a key is a legal
/// shape, not malformed input.
pub const ANNOTATION_COSIGN_CERTIFICATE: &str = "dev.sigstore.cosign/certificate";

/// PEM intermediate chain (keyless only).
pub const ANNOTATION_COSIGN_CHAIN: &str = "dev.sigstore.cosign/chain";

/// Offline Rekor bundle. Absent under `--no-rekor-upload`, and absent from
/// everything cosign v3.1.1 `attach signature` writes (see `generate.py`'s
/// `REKOR_RESPONSE_GAP`).
pub const ANNOTATION_COSIGN_BUNDLE: &str = "dev.sigstore.cosign/bundle";
