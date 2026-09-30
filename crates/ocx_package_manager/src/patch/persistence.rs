// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Persistence primitive for `__ocx.patch` descriptor blobs, written to the CAS
//! blob store so they survive offline and stay auditable
//! (`adr_infrastructure_patches.md` §"Settled open questions" #2).

use ocx_oci::{Algorithm, Digest, PackageRef, tag::InternalTag};
use ocx_store::file_structure::BlobStore;

use super::{
    descriptor::{PATCH_DESCRIPTOR_LAYER_MEDIA_TYPE, PATCH_MANIFEST_ARTIFACT_TYPE, PatchDescriptor},
    error::PatchError,
};

// ── Size cap ─────────────────────────────────────────────────────────────────

/// Maximum allowed declared size (in bytes) for a patch descriptor layer blob;
/// bounds what a malicious registry can make us buffer before parsing (CWE-400).
const MAX_DESCRIPTOR_LAYER_BYTES: u64 = 1 << 20; // 1 MiB

// ── PersistedDigests ─────────────────────────────────────────────────────────

/// Digests of the two blobs persisted by [`persist_patch_descriptor`]:
/// the manifest JSON and the descriptor layer JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedDigests {
    /// SHA-256 digest of the raw manifest JSON blob.
    pub manifest_digest: Digest,
    /// SHA-256 digest of the descriptor layer JSON blob.
    pub layer_digest: Digest,
}

// ── Fetched blobs (intermediate transfer object) ──────────────────────────────

/// Raw bytes returned by [`fetch_patch_descriptor_blobs`] before persistence.
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
/// `patch_identifier`, returning `Ok(None)` when the tag does not exist ("looked,
/// no patch"). A digest on `patch_identifier` fetches that exact manifest, verified
/// against it. Reaches the network: never call it from `compose` or GC leaf paths.
///
/// # Errors
///
/// - [`PatchError::FetchFailed`] — a network error from the OCI client.
/// - [`PatchError::UnexpectedManifest`] — manifest was an image index or
///   otherwise unexpected shape.
/// - [`PatchError::UnexpectedArtifactType`] — artifact type did not match.
/// - [`PatchError::WrongLayerCount`] — manifest had zero or more than one layer.
/// - [`PatchError::UnexpectedLayerMediaType`] — layer media type did not match.
/// - [`PatchError::LayerSizeExceeded`] — declared layer size exceeds
///   [`MAX_DESCRIPTOR_LAYER_BYTES`].
pub async fn fetch_patch_descriptor_blobs(
    client: &ocx_oci::client::Client,
    patch_identifier: &PackageRef,
) -> Result<Option<FetchedDescriptorBlobs>, PatchError> {
    // Dialled as named, never index-routed: a descriptor is not a package and no index serves one.
    let tag_identifier = ocx_oci::OciIdentifier::passthrough(patch_identifier).clone_with_tag(InternalTag::PATCH_TAG);
    // `clone_with_tag` drops the digest; a snapshot fetch must stay pinned to it.
    let tag_identifier = match patch_identifier.digest() {
        Some(digest) => tag_identifier.clone_with_digest(digest),
        None => tag_identifier,
    };

    let artifact = match client
        .fetch_single_layer_artifact(
            &tag_identifier,
            PATCH_MANIFEST_ARTIFACT_TYPE,
            PATCH_DESCRIPTOR_LAYER_MEDIA_TYPE,
            MAX_DESCRIPTOR_LAYER_BYTES,
        )
        .await
        .map_err(map_fetch_error)?
    {
        Some(artifact) => artifact,
        None => return Ok(None),
    };

    Ok(Some(FetchedDescriptorBlobs {
        manifest_bytes: artifact.manifest_bytes,
        layer_bytes: artifact.layer_bytes,
        manifest_digest: artifact.manifest_digest,
        layer_digest: artifact.layer_digest,
    }))
}

/// HEADs the `__ocx.patch` manifest digest for `patch_identifier` without downloading it, comparable
/// against a recorded descriptor digest; `Ok(None)` when the tag does not exist.
///
/// # Errors
///
/// [`PatchError::FetchFailed`] — a network or auth error from the OCI client.
pub async fn probe_patch_descriptor_digest(
    client: &ocx_oci::client::Client,
    patch_identifier: &PackageRef,
) -> Result<Option<Digest>, PatchError> {
    let tag_identifier = ocx_oci::OciIdentifier::passthrough(patch_identifier).clone_with_tag(InternalTag::PATCH_TAG);
    client
        .probe_manifest_digest_addressed(&tag_identifier, ocx_oci::client::ReadAddressing::Mirrored)
        .await
        .map_err(|source| PatchError::FetchFailed { source })
}

/// Maps a [`ocx_oci::client::ClientError`] from
/// [`ocx_oci::client::Client::fetch_single_layer_artifact`] onto [`PatchError`].
fn map_fetch_error(error: ocx_oci::client::error::ClientError) -> PatchError {
    use ocx_oci::client::error::ArtifactFetchError;
    match ArtifactFetchError::classify(error, "__ocx.patch") {
        ArtifactFetchError::UnexpectedManifest { detail } => PatchError::UnexpectedManifest { detail },
        ArtifactFetchError::UnexpectedArtifactType { actual } => PatchError::UnexpectedArtifactType { actual },
        ArtifactFetchError::WrongLayerCount { count } => PatchError::WrongLayerCount { count },
        ArtifactFetchError::UnexpectedLayerMediaType { expected, actual } => {
            PatchError::UnexpectedLayerMediaType { expected, actual }
        }
        ArtifactFetchError::LayerSizeExceeded { declared, maximum } => {
            PatchError::LayerSizeExceeded { declared, maximum }
        }
        ArtifactFetchError::Other(source) => PatchError::FetchFailed { source },
    }
}

// ── Pure persistence primitive ────────────────────────────────────────────────

/// Writes manifest and layer blobs to the CAS store, then parses and returns
/// the [`PatchDescriptor`]; needs no network access. Both digests are
/// re-verified before any write.
///
/// # Errors
///
/// - [`PatchError::LayerDigestMismatch`] / [`PatchError::ManifestDigestMismatch`]
///   — a blob's SHA-256 does not match its declared digest.
/// - [`PatchError::BlobWriteFailed`] — a blob store write fails.
/// - [`PatchError::InvalidDescriptorJson`], [`PatchError::UnsupportedVersion`],
///   [`PatchError::DescriptorTooLarge`] — as from [`PatchDescriptor::from_json_bytes`].
pub async fn persist_patch_descriptor(
    blob_store: &BlobStore,
    registry: &str,
    manifest_digest: Digest,
    manifest_bytes: &[u8],
    layer_digest: Digest,
    layer_bytes: &[u8],
) -> Result<(PatchDescriptor, PersistedDigests), PatchError> {
    // `write_blob` never re-hashes and callers can build these from arbitrary bytes,
    // so skipping either check lets mismatched bytes into the CAS under a trusted digest.
    let computed_digest = Algorithm::Sha256.hash(layer_bytes);
    if computed_digest != layer_digest {
        return Err(PatchError::LayerDigestMismatch {
            declared: layer_digest.to_string(),
            computed: computed_digest.to_string(),
        });
    }

    let computed_manifest_digest = Algorithm::Sha256.hash(manifest_bytes);
    if computed_manifest_digest != manifest_digest {
        return Err(PatchError::ManifestDigestMismatch {
            declared: manifest_digest.to_string(),
            computed: computed_manifest_digest.to_string(),
        });
    }
    blob_store
        .write_blob(registry, &manifest_digest, manifest_bytes)
        .await
        .map_err(|source| PatchError::BlobWriteFailed { source: source.into() })?;

    blob_store
        .write_blob(registry, &layer_digest, layer_bytes)
        .await
        .map_err(|source| PatchError::BlobWriteFailed { source: source.into() })?;

    let descriptor = PatchDescriptor::from_json_bytes(layer_bytes)?;

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
            ocx_oci::Digest::try_from("sha256:0000000000000000000000000000000000000000000000000000000000000000")
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
            ocx_oci::Digest::try_from("sha256:0000000000000000000000000000000000000000000000000000000000000000")
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
        ocx_oci::Algorithm::Sha256.hash(bytes)
    }
}
