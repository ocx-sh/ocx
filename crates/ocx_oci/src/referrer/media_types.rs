// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! OCI media types and artifact types used by the referrers subsystem.
//!
//! Every value is a wire string: changing one breaks interop with artifacts already pushed.

/// Sigstore bundle v0.3, the `artifactType` of a signature referrer.
pub const SIGSTORE_BUNDLE_V03: &str = "application/vnd.dev.sigstore.bundle.v0.3+json";

/// CycloneDX JSON SBOM: `artifactType` and layer type of an **unsigned** SBOM
/// referrer (a signed one is a [`SIGSTORE_BUNDLE_V03`]).
pub const SBOM_CYCLONEDX: &str = "application/vnd.cyclonedx+json";

pub const SBOM_SPDX_JSON: &str = "application/spdx+json";

/// SPDX tag-value text: not JSON, so the read path must keep payloads as opaque bytes.
pub const SBOM_SPDX_TEXT: &str = "text/spdx";

/// Every artifact type an unsigned SBOM referrer may declare.
pub const SBOM_ARTIFACT_TYPES: &[&str] = &[SBOM_CYCLONEDX, SBOM_SPDX_JSON, SBOM_SPDX_TEXT];

// cosign's own SBOM spellings, deliberately not unified with the three above
// (adr_sbom_attestations.md § Rationale from code: ocx_oci).

/// SPDX JSON as `cosign attach sbom --type spdx` types it — not [`SBOM_SPDX_JSON`],
/// or cosign-attached SBOMs stop matching.
pub const COSIGN_SBOM_SPDX_JSON: &str = "text/spdx+json";

/// CycloneDX XML as `cosign attach sbom --input-format xml` types it; listed but
/// not summarizable (`adr_sbom_attestations.md` D2).
pub const COSIGN_SBOM_CYCLONEDX_XML: &str = "application/vnd.cyclonedx+xml";

pub const EMPTY_CONFIG: &str = "application/vnd.oci.empty.v1+json";

/// The empty-config blob; push it before the referrer manifest, or a spec-strict
/// registry (zot) refuses the manifest with `MANIFEST_INVALID`.
pub const EMPTY_CONFIG_PAYLOAD: &[u8] = b"{}";

pub const EMPTY_CONFIG_DIGEST: &str = "sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a";

pub const EMPTY_CONFIG_SIZE: u64 = 2;

// The keys cosign's `WriteAttestationNewBundleFormat` writes (plus `CREATED`);
// the manifest digest is the referrer's address, so pushed bytes never migrate
// (adr_sbom_attestations.md § Storage shape: cosign v3 bundle over OCI referrers).

/// Which `content` oneof the bundle carries: tells a signature from an attestation without fetching it.
pub const ANNOTATION_BUNDLE_CONTENT: &str = "dev.sigstore.bundle.content";

pub const ANNOTATION_BUNDLE_PREDICATE_TYPE: &str = "dev.sigstore.bundle.predicateType";

/// [`ANNOTATION_BUNDLE_CONTENT`] value for a DSSE-enveloped attestation.
pub const BUNDLE_CONTENT_DSSE: &str = "dsse-envelope";

// ── cosign sidecar wire types (measured against cosign v3.1.1) ──────────

pub const SIMPLESIGNING_MEDIA_TYPE: &str = "application/vnd.dev.cosign.simplesigning.v1+json";

/// `artifactType` of a cosign signature referrer under the OCI 1.1 scheme
/// (cosign's SIGNATURE_SPEC; its own `sign` writes [`SIGSTORE_BUNDLE_V03`]).
pub const COSIGN_SIG_ARTIFACT_TYPE: &str = "application/vnd.dev.cosign.artifact.sig.v1+json";

/// `artifactType` of a cosign SBOM referrer under the OCI 1.1 scheme; the layer
/// keeps the SBOM's own type.
pub const COSIGN_SBOM_ARTIFACT_TYPE: &str = "application/vnd.dev.cosign.artifact.sbom.v1+json";

/// Layer media type of a DSSE envelope carried by a `.att` sidecar, which
/// declares no `artifactType` or `subject` — so there is no attestation artifact type to match.
pub const DSSE_ENVELOPE_MEDIA_TYPE: &str = "application/vnd.dsse.envelope.v1+json";

// The signature key's namespace differs from the rest by cosign's design, not
// a typo: unifying them breaks interop with every signature cosign ever wrote.

/// Base64 signature over the simplesigning payload.
pub const ANNOTATION_COSIGN_SIGNATURE: &str = "dev.cosignproject.cosign/signature";

/// PEM leaf certificate, keyless only: absent under a key is legal, not malformed.
pub const ANNOTATION_COSIGN_CERTIFICATE: &str = "dev.sigstore.cosign/certificate";

pub const ANNOTATION_COSIGN_CHAIN: &str = "dev.sigstore.cosign/chain";

/// Offline Rekor bundle; absent under `--no-rekor-upload` and from every cosign `attach signature`.
pub const ANNOTATION_COSIGN_BUNDLE: &str = "dev.sigstore.cosign/bundle";
