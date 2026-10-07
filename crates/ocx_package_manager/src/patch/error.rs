// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Error variants for the patch domain (descriptor types, matcher, persistence).

/// Errors raised while working with patch descriptors.
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
pub enum PatchError {
    /// The descriptor JSON was not valid UTF-8, could not be parsed, or failed
    /// structural validation (e.g. extra fields, wrong field types).
    #[error("invalid patch descriptor JSON")]
    #[exit(
        DataError,
        slug = "patch_descriptor_invalid_json",
        summary = "A patch descriptor is not valid JSON of the expected shape"
    )]
    InvalidDescriptorJson {
        /// The underlying JSON parse failure.
        #[source]
        source: serde_json::Error,
    },

    /// The descriptor's numeric `version` is unknown to this OCX — a newer OCX
    /// published it.
    #[error("unsupported patch descriptor version {version}")]
    #[exit(
        DataError,
        slug = "patch_descriptor_unsupported_version",
        summary = "A patch descriptor has a version this build does not understand"
    )]
    UnsupportedVersion {
        /// The numeric version discriminant read from the descriptor.
        version: u32,
    },

    /// The `patches.snapshot.json` on disk carries a format version this OCX
    /// does not read.
    #[error("patch snapshot '{path}' has format version {found}, expected {expected}; re-run `ocx patch freeze`")]
    #[exit(
        DataError,
        slug = "patch_snapshot_unsupported_version",
        summary = "The patch snapshot has a format version this build does not understand"
    )]
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
    #[exit(delegate = source)]
    FetchFailed {
        /// The underlying OCI client error.
        #[source]
        source: ocx_oci::client::error::ClientError,
    },

    /// The OCI manifest for the `__ocx.patch` tag was not a single-image
    /// manifest (it was an image index, or had an unexpected structure).
    #[error("unexpected manifest shape for patch descriptor: {detail}")]
    #[exit(
        DataError,
        slug = "patch_unexpected_manifest",
        summary = "A patch descriptor manifest has an unexpected shape"
    )]
    UnexpectedManifest {
        /// Human-readable detail about the shape mismatch.
        detail: String,
    },

    /// The artifact type on the manifest did not match the expected
    /// `application/vnd.sh.ocx.patch.v1` value.
    #[error("unexpected patch manifest artifact type: {actual:?}")]
    #[exit(
        DataError,
        slug = "patch_unexpected_artifact_type",
        summary = "A patch descriptor manifest carries an unexpected artifact type"
    )]
    UnexpectedArtifactType {
        /// The artifact type that was actually present (or `None` if absent).
        actual: Option<String>,
    },

    /// The descriptor manifest had no layers or more than one layer. A patch
    /// descriptor must carry exactly one descriptor-layer blob.
    #[error("patch manifest must have exactly one layer, got {count}")]
    #[exit(
        DataError,
        slug = "patch_wrong_layer_count",
        summary = "A patch descriptor manifest carries other than exactly one layer"
    )]
    WrongLayerCount {
        /// Actual number of layers found.
        count: usize,
    },

    /// The layer's `mediaType` did not match the expected
    /// `application/vnd.sh.ocx.patch.descriptor.v1+json` value.
    #[error("unexpected patch descriptor layer media type: expected '{expected}', got '{actual}'")]
    #[exit(
        DataError,
        slug = "patch_unexpected_layer_media_type",
        summary = "A patch descriptor layer carries an unexpected media type"
    )]
    UnexpectedLayerMediaType {
        /// The expected media type value.
        expected: String,
        /// The actual media type from the manifest layer descriptor.
        actual: String,
    },

    /// The declared size of the descriptor layer blob exceeded the enforced ceiling.
    #[error("patch descriptor layer size {declared} exceeds the maximum allowed {maximum} bytes")]
    #[exit(
        DataError,
        slug = "patch_layer_size_exceeded",
        summary = "A patch descriptor layer declares a size above the allowed maximum"
    )]
    LayerSizeExceeded {
        /// The size declared in the manifest layer descriptor.
        declared: i64,
        /// The enforced ceiling in bytes.
        maximum: u64,
    },

    /// The SHA-256 digest of the layer bytes does not match the manifest's.
    #[error("patch descriptor layer digest mismatch: declared '{declared}', computed '{computed}'")]
    #[exit(
        DataError,
        slug = "patch_layer_digest_mismatch",
        summary = "A patch descriptor layer does not hash to its declared digest"
    )]
    LayerDigestMismatch {
        /// The digest declared in the manifest descriptor.
        declared: String,
        /// The digest actually computed from the fetched bytes.
        computed: String,
    },

    /// The SHA-256 digest of the manifest bytes does not match the digest the
    /// caller declared for them.
    #[error("patch descriptor manifest digest mismatch: declared '{declared}', computed '{computed}'")]
    #[exit(
        DataError,
        slug = "patch_manifest_digest_mismatch",
        summary = "A patch descriptor manifest does not hash to its declared digest"
    )]
    ManifestDigestMismatch {
        /// The digest the caller declared for the manifest bytes.
        declared: String,
        /// The digest actually computed from the manifest bytes.
        computed: String,
    },

    /// The descriptor's rules count or packages-per-rule count exceeded the
    /// enforced maximum.
    #[error("patch descriptor exceeds structural limits: {detail}")]
    #[exit(
        DataError,
        slug = "patch_descriptor_too_large",
        summary = "A patch descriptor exceeds its structural limits"
    )]
    DescriptorTooLarge {
        /// Human-readable detail about which limit was exceeded.
        detail: String,
    },

    /// A blob store write failed while persisting the manifest or descriptor
    /// layer bytes.
    #[error("failed to persist patch descriptor blob")]
    #[exit(
        IoError,
        slug = "patch_blob_write_failed",
        summary = "Persisting a patch descriptor blob failed"
    )]
    BlobWriteFailed {
        /// The underlying blob-store error.
        #[source]
        source: crate::Error,
    },

    /// A required tier's recorded descriptor is gone from the registry. Only `ocx patch sync` may
    /// record it absent, so the last known descriptor stands.
    #[error("patch descriptor '{identifier}' is no longer in the registry; run `ocx patch sync` to record it absent")]
    #[exit(
        NotFound,
        slug = "patch_descriptor_vanished",
        summary = "A pinned patch descriptor is no longer in the registry"
    )]
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
    #[exit(
        PolicyBlocked,
        slug = "patch_policy_blocked",
        summary = "A local policy refused fetching a patch descriptor"
    )]
    PolicyBlocked {
        /// The companion whose tag could not be resolved.
        identifier: Box<ocx_oci::PackageRef>,
    },

    /// `ocx patch sync` advances the pins an active patch snapshot freezes, so the two conflict.
    #[error("`ocx patch sync` cannot advance pins while OCX_PATCH_SNAPSHOT is set; unset it to advance pins")]
    #[exit(
        ConfigError,
        slug = "patch_snapshot_active",
        summary = "Patch pins cannot advance while OCX_PATCH_SNAPSHOT is set"
    )]
    SnapshotActive,

    /// A descriptor the active patch snapshot pins is not in the local store, and `--offline`
    /// forbids fetching it by its frozen digest.
    #[error(
        "patch descriptor '{identifier}' pinned by the patch snapshot at {digest} is not in the local store; run without --offline to fetch it"
    )]
    #[exit(
        NotFound,
        slug = "patch_snapshot_descriptor_missing",
        summary = "The patch snapshot names a descriptor that is not stored locally"
    )]
    SnapshotDescriptorMissing {
        /// The descriptor source the snapshot pins.
        identifier: Box<ocx_oci::PackageRef>,
        /// The manifest digest the snapshot pins it at.
        digest: ocx_oci::Digest,
    },

    /// A registered project's `ocx.toml` could not be read or parsed, so `ocx patch freeze`
    /// cannot tell which companions that project's tagged bases discover.
    #[error("cannot freeze patches: project config '{path}' is unreadable")]
    #[exit(
        ConfigError,
        slug = "patch_project_config_unreadable",
        summary = "The project config patches would freeze into is unreadable"
    )]
    ProjectConfigUnreadable {
        /// The `ocx.toml` that failed.
        path: std::path::PathBuf,
        /// The read or parse failure.
        #[source]
        source: Box<ocx_project::Error>,
    },
}
