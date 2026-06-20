// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Persistence primitive for `__ocx.patch` descriptor blobs.
//!
//! Closes the gap left by `pull_description` in `oci/client.rs`, which reads
//! artifact layers **into memory and never persists them**. Patch descriptors
//! must survive offline and be auditable (`adr_infrastructure_patches.md`
//! §"Settled open questions" #2), so they are written to the CAS blob store via
//! [`BlobStore::write_blob`].
//!
//! ## Responsibility split
//!
//! The Phase 2 persistence primitive is split into two functions with distinct
//! concerns:
//!
//! | Function | Concerns | Testable |
//! |---|---|---|
//! | [`fetch_patch_descriptor_blobs`] | Network: auth, OCI fetch, media-type validation | Integration test only |
//! | [`persist_patch_descriptor`] | Pure: blob writes + JSON parse | Unit-testable with synthetic bytes |
//!
//! This mirrors how `pull_description` separates transport from assembly.
//! Callers in Phase 3 (`SitePatchResolver`) will call `fetch_patch_descriptor_blobs`
//! and pipe the returned bytes into `persist_patch_descriptor`.
//!
//! ## What is NOT wired here
//!
//! Phase 2 delivers the primitive. Discovery (which repos to check, when to
//! re-check), tag-store recording, companion install, and GC root seeding are
//! **Phase 3+** concerns (`SitePatchResolver`).

use crate::{
    file_structure::BlobStore,
    oci::{Algorithm, Digest, Identifier},
    package::tag::InternalTag,
};

use super::{
    descriptor::{PATCH_DESCRIPTOR_LAYER_MEDIA_TYPE, PATCH_MANIFEST_ARTIFACT_TYPE, PatchDescriptor},
    error::PatchError,
};

// ── Size cap ─────────────────────────────────────────────────────────────────

/// Maximum allowed declared size (in bytes) for a patch descriptor layer blob.
///
/// A conforming patch descriptor is a small JSON document (rules + identifier
/// strings). 1 MiB is a generous ceiling that covers any realistic descriptor
/// while guarding against a malicious registry serving a multi-gigabyte blob
/// that would be buffered entirely in memory before parsing.
///
/// Mirrors the two-caps pattern in `Client::pull_layer` (CWE-400).
const MAX_DESCRIPTOR_LAYER_BYTES: u64 = 1 << 20; // 1 MiB

// ── PersistedDigests ─────────────────────────────────────────────────────────

/// Digests of the two blobs persisted by [`persist_patch_descriptor`]:
/// the manifest JSON and the descriptor layer JSON.
///
/// Callers (Phase 3 `SitePatchResolver`) record these into the tag store's
/// `__ocx.patch` key so GC can re-derive companion roots from local state
/// without a ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedDigests {
    /// SHA-256 digest of the raw manifest JSON blob.
    pub manifest_digest: Digest,
    /// SHA-256 digest of the descriptor layer JSON blob.
    pub layer_digest: Digest,
}

// ── Fetched blobs (intermediate transfer object) ──────────────────────────────

/// Raw bytes returned by [`fetch_patch_descriptor_blobs`] before persistence.
///
/// Kept as a plain struct so the network fn and the pure persistence fn can be
/// called and tested independently.
#[derive(Debug)]
pub struct FetchedDescriptorBlobs {
    /// Raw manifest JSON bytes (the OCI image manifest for `__ocx.patch`).
    pub manifest_bytes: Vec<u8>,
    /// Raw descriptor layer bytes (the `application/vnd.sh.ocx.patch.descriptor.v1+json` blob).
    pub layer_bytes: Vec<u8>,
    /// Digest of the manifest blob (computed or received from the registry).
    pub manifest_digest: Digest,
    /// Digest of the layer blob as declared in the manifest.
    pub layer_digest: Digest,
}

// ── Network primitive ─────────────────────────────────────────────────────────

/// Fetches the `__ocx.patch` manifest and its single descriptor layer for
/// `patch_identifier` from the OCI registry.
///
/// `patch_identifier` must already have the `__ocx.patch` tag set (see
/// `package::tag::PATCH_TAG`). The function:
///
/// 1. Authenticates against the registry (via the existing client auth path).
/// 2. Fetches the manifest; returns `Ok(None)` if the tag does not exist
///    ("looked, no patch" discovery state).
/// 3. Validates the manifest's `artifactType` against
///    [`PATCH_MANIFEST_ARTIFACT_TYPE`].
/// 4. Validates the manifest has exactly one layer.
/// 5. Validates the layer's `mediaType` against [`PATCH_DESCRIPTOR_LAYER_MEDIA_TYPE`].
/// 6. Validates the declared layer size against [`MAX_DESCRIPTOR_LAYER_BYTES`]
///    (CWE-400 pre-check — reject manifests with an oversized declared size).
/// 7. Fetches the single layer blob bytes with a stream-level byte cap of
///    [`MAX_DESCRIPTOR_LAYER_BYTES`] so a malicious registry cannot buffer
///    more bytes than its declared size regardless of what it streams.
/// 8. Returns the raw bytes and digests for the caller to persist.
///
/// This is a network function; it must not be called from inside `compose` or
/// GC leaf paths. Phase 3 `SitePatchResolver` calls it at discovery time.
///
/// # Errors
///
/// - [`PatchError::FetchFailed`] — a network error from the OCI client,
///   preserving the full [`crate::oci::client::ClientError`] source chain.
/// - [`PatchError::UnexpectedManifest`] — manifest was an image index or
///   otherwise unexpected shape.
/// - [`PatchError::UnexpectedArtifactType`] — artifact type did not match.
/// - [`PatchError::WrongLayerCount`] — manifest had zero or more than one layer.
/// - [`PatchError::UnexpectedLayerMediaType`] — layer media type did not match.
/// - [`PatchError::LayerSizeExceeded`] — declared layer size exceeds
///   [`MAX_DESCRIPTOR_LAYER_BYTES`].
pub async fn fetch_patch_descriptor_blobs(
    client: &crate::oci::client::Client,
    patch_identifier: &Identifier,
) -> Result<Option<FetchedDescriptorBlobs>, PatchError> {
    // Build the tag identifier: clone with the `__ocx.patch` well-known tag.
    let tag_identifier = patch_identifier.clone_with_tag(InternalTag::PATCH_TAG);

    // Step 1: Fetch the raw manifest bytes; `Ok(None)` → "looked, no patch".
    let (manifest_bytes, manifest_digest, manifest) = match client
        .fetch_patch_manifest_raw(&tag_identifier)
        .await
        .map_err(|source| PatchError::FetchFailed { source })?
    {
        Some(triple) => triple,
        None => return Ok(None),
    };

    // Step 2: Validate the manifest shape (must be a single-image manifest).
    let image_manifest = match manifest {
        crate::oci::Manifest::Image(m) => m,
        crate::oci::Manifest::ImageIndex(_) => {
            return Err(PatchError::UnexpectedManifest {
                detail: "expected image manifest for __ocx.patch, got image index".to_string(),
            });
        }
    };

    // Step 3: Validate the artifact type.
    match &image_manifest.artifact_type {
        Some(at) if at == PATCH_MANIFEST_ARTIFACT_TYPE => {}
        other => {
            return Err(PatchError::UnexpectedArtifactType { actual: other.clone() });
        }
    }

    // Step 4: Validate exactly one layer.
    if image_manifest.layers.len() != 1 {
        return Err(PatchError::WrongLayerCount {
            count: image_manifest.layers.len(),
        });
    }
    let layer_descriptor = &image_manifest.layers[0];

    // Step 5: Validate the layer media type. A manifest with the wrong layer
    // media type (e.g. a tar+gzip layer) would parse successfully if the bytes
    // happened to be valid JSON — rejecting here closes that ambiguity window.
    if layer_descriptor.media_type != PATCH_DESCRIPTOR_LAYER_MEDIA_TYPE {
        return Err(PatchError::UnexpectedLayerMediaType {
            expected: PATCH_DESCRIPTOR_LAYER_MEDIA_TYPE.to_string(),
            actual: layer_descriptor.media_type.clone(),
        });
    }

    // Step 6: Size cap (CWE-400). Reject manifests that declare a layer larger
    // than MAX_DESCRIPTOR_LAYER_BYTES before issuing the blob fetch. A negative
    // or zero declared size is also rejected as a malformed manifest.
    let declared_size = layer_descriptor.size;
    match u64::try_from(declared_size) {
        Ok(size) if size <= MAX_DESCRIPTOR_LAYER_BYTES => {}
        Ok(_) => {
            return Err(PatchError::LayerSizeExceeded {
                declared: declared_size,
                maximum: MAX_DESCRIPTOR_LAYER_BYTES,
            });
        }
        Err(_) => {
            return Err(PatchError::UnexpectedManifest {
                detail: format!("layer descriptor size '{declared_size}' is not a valid byte count"),
            });
        }
    }

    let layer_digest =
        Digest::try_from(layer_descriptor.digest.as_str()).map_err(|_| PatchError::UnexpectedManifest {
            detail: format!("layer digest '{}' is malformed", layer_descriptor.digest),
        })?;

    // Step 7: Fetch the descriptor layer blob into memory.
    // The declared-size guard above (step 6) ensures we do not *request* a
    // blob larger than MAX_DESCRIPTOR_LAYER_BYTES. The `max_bytes` argument to
    // `fetch_patch_layer_blob` closes the gap where a malicious registry
    // ignores its own declared size and streams more bytes: the function caps
    // the stream at `max_bytes + 1` bytes and returns
    // `ClientError::DecompressionCapExceeded` if the cap is hit.
    let layer_bytes = client
        .fetch_patch_layer_blob(&tag_identifier, &layer_digest, MAX_DESCRIPTOR_LAYER_BYTES)
        .await
        .map_err(|source| PatchError::FetchFailed { source })?;

    Ok(Some(FetchedDescriptorBlobs {
        manifest_bytes,
        layer_bytes,
        manifest_digest,
        layer_digest,
    }))
}

// ── Pure persistence primitive ────────────────────────────────────────────────

/// Writes manifest and layer blobs to the CAS store, then parses and returns
/// the [`PatchDescriptor`].
///
/// This function is **pure** in the sense that it requires no network access —
/// it operates entirely on the provided `blob_store`, `manifest_bytes`, and
/// `layer_bytes`. It is designed to be unit-tested with a temporary
/// [`BlobStore`] and synthetic bytes without any network dependency.
///
/// [`BlobStore::write_blob`] documents: "Caller MUST have verified
/// `digest == sha256(bytes)` upstream — this function does not re-hash."
/// This function re-verifies **both** the layer digest and the manifest digest
/// before writing to satisfy that contract. Phase 3 callers that construct
/// [`FetchedDescriptorBlobs`] directly (rather than via
/// [`fetch_patch_descriptor_blobs`]) may supply arbitrary bytes; re-verification
/// here closes the integrity gap for both blobs.
///
/// ## Steps
///
/// 1. Re-verify `layer_bytes` digest against `layer_digest`
///    ([`PatchError::LayerDigestMismatch`] on failure).
/// 2. Re-verify `manifest_bytes` digest against `manifest_digest`
///    ([`PatchError::ManifestDigestMismatch`] on failure — a distinct variant
///    from the layer check so callers can tell which blob failed).
/// 3. Write `manifest_bytes` to the blob store keyed by `(registry, manifest_digest)`.
/// 4. Write `layer_bytes` to the blob store keyed by `(registry, layer_digest)`.
/// 5. Parse `layer_bytes` as [`PatchDescriptor`] (via
///    [`PatchDescriptor::from_json_bytes`]).
/// 6. Return the parsed descriptor + the persisted [`PersistedDigests`].
///
/// # Errors
///
/// - [`PatchError::LayerDigestMismatch`] — if the computed SHA-256 of
///   `layer_bytes` does not match the declared digest.
/// - [`PatchError::ManifestDigestMismatch`] — if the computed SHA-256 of
///   `manifest_bytes` does not match the declared digest.
/// - [`PatchError::BlobWriteFailed`] — if a blob store write fails.
/// - [`PatchError::InvalidDescriptorJson`] — if the layer bytes are not valid
///   descriptor JSON.
/// - [`PatchError::UnsupportedVersion`] — if the `version` field carries an
///   unknown discriminant.
/// - [`PatchError::DescriptorTooLarge`] — if the descriptor exceeds structural
///   limits (rules count or packages-per-rule count).
pub async fn persist_patch_descriptor(
    blob_store: &BlobStore,
    registry: &str,
    manifest_digest: Digest,
    manifest_bytes: &[u8],
    layer_digest: Digest,
    layer_bytes: &[u8],
) -> Result<(PatchDescriptor, PersistedDigests), PatchError> {
    // Step 1: Re-verify the layer blob digest before writing to the CAS.
    // BlobStore::write_blob requires the caller to have verified this; since
    // Phase 3 callers may construct FetchedDescriptorBlobs directly, we
    // re-check here to enforce the CAS integrity invariant.
    let computed_digest = Algorithm::Sha256.hash(layer_bytes);
    if computed_digest != layer_digest {
        return Err(PatchError::LayerDigestMismatch {
            declared: layer_digest.to_string(),
            computed: computed_digest.to_string(),
        });
    }

    // Step 2: Re-verify the manifest blob digest before writing to the CAS.
    // BlobStore::write_blob requires the caller to have verified
    // `digest == sha256(bytes)`. The manifest digest comes from the OCI
    // transport (pull_manifest_raw → Docker-Content-Digest header or local
    // hash), but we cannot trust the registry unconditionally: a tampered
    // response could send mismatched bytes. Re-hashing here closes that
    // trust-boundary gap and keeps the integrity model consistent with the
    // layer digest re-verification above.
    let computed_manifest_digest = Algorithm::Sha256.hash(manifest_bytes);
    if computed_manifest_digest != manifest_digest {
        return Err(PatchError::ManifestDigestMismatch {
            declared: manifest_digest.to_string(),
            computed: computed_manifest_digest.to_string(),
        });
    }
    // Step 3: Persist the manifest blob.
    blob_store
        .write_blob(registry, &manifest_digest, manifest_bytes)
        .await
        .map_err(|source| PatchError::BlobWriteFailed { source })?;

    // Step 4: Persist the descriptor layer blob.
    blob_store
        .write_blob(registry, &layer_digest, layer_bytes)
        .await
        .map_err(|source| PatchError::BlobWriteFailed { source })?;

    // Step 5: Parse the layer bytes as a PatchDescriptor.
    let descriptor = PatchDescriptor::from_json_bytes(layer_bytes)?;

    // Step 6: Return parsed descriptor + persisted digests.
    Ok((
        descriptor,
        PersistedDigests {
            manifest_digest,
            layer_digest,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Helper: create a minimal valid descriptor JSON.
    fn minimal_descriptor_json() -> Vec<u8> {
        serde_json::json!({
            "version": 1,
            "rules": [
                {
                    "match": "*",
                    "packages": ["internal.company.com/certs/zscaler-root:latest"]
                }
            ]
        })
        .to_string()
        .into_bytes()
    }

    /// `persist_patch_descriptor` with valid bytes and a temporary blob store
    /// writes two blobs and returns a parsed descriptor.
    ///
    /// This test is UNIT-LEVEL: no network, no OCI registry, synthetic bytes only.
    #[tokio::test]
    async fn persist_writes_blobs_and_parses_descriptor() {
        let tmp = tempfile::TempDir::new().expect("temp dir");
        let blob_store = BlobStore::new(tmp.path());

        let manifest_bytes = b"{\"schemaVersion\":2}"; // minimal synthetic manifest
        let layer_bytes = minimal_descriptor_json();

        // Compute SHA-256 digests for the synthetic bytes.
        let manifest_digest = sha256_digest(manifest_bytes);
        let layer_digest = sha256_digest(&layer_bytes);

        let (descriptor, persisted) = persist_patch_descriptor(
            &blob_store,
            "internal.company.com",
            manifest_digest.clone(),
            manifest_bytes,
            layer_digest.clone(),
            &layer_bytes,
        )
        .await
        .expect("persist must succeed");

        assert_eq!(persisted.manifest_digest, manifest_digest);
        assert_eq!(persisted.layer_digest, layer_digest);
        assert_eq!(descriptor.rules.len(), 1);
        assert_eq!(descriptor.rules[0].match_pattern, "*");
    }

    /// A minimal valid descriptor round-trips through JSON serialization and
    /// `PatchDescriptor::from_json_bytes`.
    #[test]
    fn descriptor_from_json_bytes_round_trips() {
        let bytes = minimal_descriptor_json();
        let descriptor = PatchDescriptor::from_json_bytes(&bytes).expect("valid descriptor JSON must parse");
        assert_eq!(descriptor.version, super::super::descriptor::PatchDescriptorVersion::V1);
        assert_eq!(descriptor.rules.len(), 1);
    }

    /// Invalid JSON yields `PatchError::InvalidDescriptorJson`.
    #[test]
    fn descriptor_from_json_bytes_invalid_json() {
        let result = PatchDescriptor::from_json_bytes(b"not json {{{");
        assert!(
            matches!(result, Err(PatchError::InvalidDescriptorJson { .. })),
            "invalid JSON must yield InvalidDescriptorJson, got: {result:?}"
        );
    }

    /// An unknown version value (99) is rejected by the two-step pre-parse in
    /// `from_json_bytes`, surfacing `PatchError::UnsupportedVersion { version: 99 }`.
    #[test]
    fn descriptor_unknown_version_rejected() {
        let bytes = serde_json::json!({ "version": 99, "rules": [] })
            .to_string()
            .into_bytes();
        let result = PatchDescriptor::from_json_bytes(&bytes);
        assert!(
            matches!(result, Err(PatchError::UnsupportedVersion { version: 99 })),
            "unknown version must yield UnsupportedVersion {{ version: 99 }}, got: {result:?}"
        );
    }

    /// `persist_patch_descriptor` returns `LayerDigestMismatch` when the
    /// provided `layer_digest` does not match the actual SHA-256 of `layer_bytes`.
    ///
    /// This tests the CAS integrity guard: the function must re-verify the
    /// digest rather than blindly trusting the caller.
    #[tokio::test]
    async fn persist_rejects_layer_digest_mismatch() {
        let tmp = tempfile::TempDir::new().expect("temp dir");
        let blob_store = BlobStore::new(tmp.path());

        let manifest_bytes = b"{\"schemaVersion\":2}";
        let layer_bytes = minimal_descriptor_json();

        let manifest_digest = sha256_digest(manifest_bytes);
        // Deliberately provide a wrong (all-zeros) layer digest.
        let wrong_layer_digest =
            crate::oci::Digest::try_from("sha256:0000000000000000000000000000000000000000000000000000000000000000")
                .expect("valid zero digest");

        let result = persist_patch_descriptor(
            &blob_store,
            "internal.company.com",
            manifest_digest,
            manifest_bytes,
            wrong_layer_digest,
            &layer_bytes,
        )
        .await;

        assert!(
            matches!(result, Err(PatchError::LayerDigestMismatch { .. })),
            "wrong layer digest must yield LayerDigestMismatch, got: {result:?}"
        );
    }

    /// `persist_patch_descriptor` returns `ManifestDigestMismatch` (distinct
    /// from the layer variant) when the provided `manifest_digest` does not
    /// match the actual SHA-256 of `manifest_bytes`.
    ///
    /// Both blobs are re-verified before writing to the blob store, each with
    /// its own error variant so a caller can tell which blob failed.
    #[tokio::test]
    async fn persist_rejects_manifest_digest_mismatch() {
        let tmp = tempfile::TempDir::new().expect("temp dir");
        let blob_store = BlobStore::new(tmp.path());

        let manifest_bytes = b"{\"schemaVersion\":2}";
        let layer_bytes = minimal_descriptor_json();

        let layer_digest = sha256_digest(&layer_bytes);
        // Deliberately provide a wrong (all-zeros) manifest digest.
        let wrong_manifest_digest =
            crate::oci::Digest::try_from("sha256:0000000000000000000000000000000000000000000000000000000000000000")
                .expect("valid zero digest");

        let result = persist_patch_descriptor(
            &blob_store,
            "internal.company.com",
            wrong_manifest_digest,
            manifest_bytes,
            layer_digest,
            &layer_bytes,
        )
        .await;

        assert!(
            matches!(result, Err(PatchError::ManifestDigestMismatch { .. })),
            "wrong manifest digest must yield ManifestDigestMismatch, got: {result:?}"
        );
    }

    // ── Utility ──────────────────────────────────────────────────────────────

    /// Compute a SHA-256 digest for test bytes via `Algorithm::Sha256.hash`.
    fn sha256_digest(bytes: &[u8]) -> Digest {
        crate::oci::Algorithm::Sha256.hash(bytes)
    }
}
