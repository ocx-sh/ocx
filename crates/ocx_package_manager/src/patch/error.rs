// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Error variants for the patch domain (descriptor types, matcher, persistence).

/// Errors raised while working with patch descriptors.
#[derive(Debug, thiserror::Error)]
pub enum PatchError {
    /// The descriptor JSON was not valid UTF-8, could not be parsed, or failed
    /// structural validation (e.g. extra fields, wrong field types).
    #[error("invalid patch descriptor JSON")]
    InvalidDescriptorJson {
        /// The underlying JSON parse failure.
        #[source]
        source: serde_json::Error,
    },

    /// The descriptor's numeric `version` is unknown to this OCX — a newer OCX
    /// published it.
    #[error("unsupported patch descriptor version {version}")]
    UnsupportedVersion {
        /// The numeric version discriminant read from the descriptor.
        version: u32,
    },

    /// The `patches.snapshot.json` on disk carries a format version this OCX
    /// does not read.
    #[error("patch snapshot '{path}' has format version {found}, expected {expected}; re-run `ocx patch freeze`")]
    UnsupportedSnapshotVersion {
        /// The snapshot file that could not be read.
        path: String,
        /// The `version` value found in the file.
        found: u64,
        /// The only version this OCX reads and writes.
        expected: u64,
    },

    /// A network fetch of the `__ocx.patch` manifest or layer blob failed.
    // Keep the `ClientError` as `#[source]`: exit-code classification downcasts through it.
    #[error("failed to fetch patch descriptor from registry")]
    FetchFailed {
        /// The underlying OCI client error.
        #[source]
        source: ocx_oci::client::error::ClientError,
    },

    /// The OCI manifest for the `__ocx.patch` tag was not a single-image
    /// manifest (it was an image index, or had an unexpected structure).
    #[error("unexpected manifest shape for patch descriptor: {detail}")]
    UnexpectedManifest {
        /// Human-readable detail about the shape mismatch.
        detail: String,
    },

    /// The artifact type on the manifest did not match the expected
    /// `application/vnd.sh.ocx.patch.v1` value.
    #[error("unexpected patch manifest artifact type: {actual:?}")]
    UnexpectedArtifactType {
        /// The artifact type that was actually present (or `None` if absent).
        actual: Option<String>,
    },

    /// The descriptor manifest had no layers or more than one layer. A patch
    /// descriptor must carry exactly one descriptor-layer blob.
    #[error("patch manifest must have exactly one layer, got {count}")]
    WrongLayerCount {
        /// Actual number of layers found.
        count: usize,
    },

    /// The layer's `mediaType` did not match the expected
    /// `application/vnd.sh.ocx.patch.descriptor.v1+json` value.
    #[error("unexpected patch descriptor layer media type: expected '{expected}', got '{actual}'")]
    UnexpectedLayerMediaType {
        /// The expected media type value.
        expected: String,
        /// The actual media type from the manifest layer descriptor.
        actual: String,
    },

    /// The declared size of the descriptor layer blob exceeded the enforced ceiling.
    #[error("patch descriptor layer size {declared} exceeds the maximum allowed {maximum} bytes")]
    LayerSizeExceeded {
        /// The size declared in the manifest layer descriptor.
        declared: i64,
        /// The enforced ceiling in bytes.
        maximum: u64,
    },

    /// The SHA-256 digest of the layer bytes does not match the manifest's.
    #[error("patch descriptor layer digest mismatch: declared '{declared}', computed '{computed}'")]
    LayerDigestMismatch {
        /// The digest declared in the manifest descriptor.
        declared: String,
        /// The digest actually computed from the fetched bytes.
        computed: String,
    },

    /// The SHA-256 digest of the manifest bytes does not match the digest the
    /// caller declared for them.
    #[error("patch descriptor manifest digest mismatch: declared '{declared}', computed '{computed}'")]
    ManifestDigestMismatch {
        /// The digest the caller declared for the manifest bytes.
        declared: String,
        /// The digest actually computed from the manifest bytes.
        computed: String,
    },

    /// The descriptor's rules count or packages-per-rule count exceeded the
    /// enforced maximum.
    #[error("patch descriptor exceeds structural limits: {detail}")]
    DescriptorTooLarge {
        /// Human-readable detail about which limit was exceeded.
        detail: String,
    },

    /// A blob store write failed while persisting the manifest or descriptor
    /// layer bytes.
    #[error("failed to persist patch descriptor blob")]
    BlobWriteFailed {
        /// The underlying blob-store error.
        #[source]
        source: crate::Error,
    },

    /// A required tier's recorded descriptor is gone from the registry. Only `ocx patch sync` may
    /// record it absent, so the last known descriptor stands.
    #[error("patch descriptor '{identifier}' is no longer in the registry; run `ocx patch sync` to record it absent")]
    DescriptorVanished {
        /// The descriptor source that answered not-found.
        identifier: Box<ocx_oci::PackageRef>,
    },

    /// A companion carries no recorded patch-tier pin and `--offline` forbids
    /// resolving its tag to discover one. `--frozen` is not a cause: a
    /// companion resolves live under it.
    #[error(
        "offline mode refused to resolve unpinned companion '{identifier}'; run `ocx patch sync` without --offline"
    )]
    PolicyBlocked {
        /// The companion whose tag could not be resolved.
        identifier: Box<ocx_oci::PackageRef>,
    },

    /// `ocx patch sync` advances the pins an active patch snapshot freezes, so the two conflict.
    #[error("`ocx patch sync` cannot advance pins while OCX_PATCH_SNAPSHOT is set; unset it to advance pins")]
    SnapshotActive,

    /// A descriptor the active patch snapshot pins is not in the local store, and `--offline`
    /// forbids fetching it by its frozen digest.
    #[error(
        "patch descriptor '{identifier}' pinned by the patch snapshot at {digest} is not in the local store; run without --offline to fetch it"
    )]
    SnapshotDescriptorMissing {
        /// The descriptor source the snapshot pins.
        identifier: Box<ocx_oci::PackageRef>,
        /// The manifest digest the snapshot pins it at.
        digest: ocx_oci::Digest,
    },
}
