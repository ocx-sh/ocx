// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Error variants for the patch domain (descriptor types, matcher, persistence).

use crate::cli::{ClassifyExitCode, ExitCode};

/// Errors raised while working with patch descriptors.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PatchError {
    /// The descriptor JSON was not valid UTF-8, could not be parsed, or failed
    /// structural validation (e.g. extra fields, wrong field types).
    ///
    /// # Version rejection
    ///
    /// Unknown `version` discriminants that were not caught by the explicit
    /// pre-parse step surface here via `serde_repr`'s deserialization rejection.
    /// The explicit check in [`crate::patch::descriptor::PatchDescriptor::from_json_bytes`]
    /// tries to return [`PatchError::UnsupportedVersion`] for unknown numeric
    /// version values before the full serde parse; any remaining shape mismatches
    /// fall back to this variant.
    #[error("invalid patch descriptor JSON")]
    InvalidDescriptorJson {
        /// The underlying JSON parse failure.
        #[source]
        source: serde_json::Error,
    },

    /// The `version` field in the descriptor carries a numeric value this OCX
    /// version does not understand. Callers should treat this as a
    /// forward-compatibility signal: a newer OCX version published the descriptor.
    ///
    /// Returned by [`crate::patch::descriptor::PatchDescriptor::from_json_bytes`]
    /// when the raw `version` integer cannot be matched to a known
    /// [`crate::patch::descriptor::PatchDescriptorVersion`] discriminant.
    #[error("unsupported patch descriptor version {version}")]
    UnsupportedVersion {
        /// The numeric version discriminant read from the descriptor.
        version: u32,
    },

    /// The `patches.snapshot.json` on disk carries a format version this OCX
    /// does not read.
    ///
    /// Raised by [`PatchSnapshot::read`](crate::patch::PatchSnapshot::read).
    /// There is deliberately no reader for an older generation: a snapshot is
    /// derived from local state that `ocx patch freeze` re-resolves offline in
    /// seconds, so rewriting it is cheaper and less ambiguous than carrying a
    /// second parse path whose output could disagree with a fresh freeze.
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
    ///
    /// Preserves the full [`crate::oci::client::ClientError`] source chain so
    /// callers can downcast for exit-code classification (auth failure, network
    /// unavailable, not-found, etc.).
    #[error("failed to fetch patch descriptor from registry")]
    FetchFailed {
        /// The underlying OCI client error.
        #[source]
        source: crate::oci::client::error::ClientError,
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

    /// The declared size of the descriptor layer blob exceeded the enforced
    /// ceiling. A conforming patch descriptor is a small JSON document; a
    /// large declared size signals a misconfigured or malicious manifest.
    #[error("patch descriptor layer size {declared} exceeds the maximum allowed {maximum} bytes")]
    LayerSizeExceeded {
        /// The size declared in the manifest layer descriptor.
        declared: i64,
        /// The enforced ceiling in bytes.
        maximum: u64,
    },

    /// The SHA-256 digest of the layer bytes does not match the digest
    /// declared in the manifest. This indicates corruption or a tampered blob.
    #[error("patch descriptor layer digest mismatch: declared '{declared}', computed '{computed}'")]
    LayerDigestMismatch {
        /// The digest declared in the manifest descriptor.
        declared: String,
        /// The digest actually computed from the fetched bytes.
        computed: String,
    },

    /// The SHA-256 digest of the manifest bytes does not match the digest the
    /// caller declared for them. Distinct from [`Self::LayerDigestMismatch`] so
    /// callers (and logs) can tell which blob failed verification.
    #[error("patch descriptor manifest digest mismatch: declared '{declared}', computed '{computed}'")]
    ManifestDigestMismatch {
        /// The digest the caller declared for the manifest bytes.
        declared: String,
        /// The digest actually computed from the manifest bytes.
        computed: String,
    },

    /// The descriptor's rules count or packages-per-rule count exceeded the
    /// enforced maximum. This guards against quadratic dedup scan cost from a
    /// malformed or malicious descriptor.
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

    /// A companion carries no recorded patch-tier pin and `--offline` forbids
    /// resolving its tag to discover one.
    ///
    /// Raised by
    /// [`install_companion`](crate::package_manager::PackageManager::install_companion):
    /// pulling an already-pinned digest stays allowed, but learning a NEW
    /// tag→digest binding needs the network. `--frozen` is deliberately NOT a
    /// cause — it scopes to the package tier, and a companion resolves live
    /// under it.
    ///
    /// The remedy names `ocx patch sync` because it is the ONLY command that
    /// writes a companion pin — the package-tier `ocx index update` never does.
    #[error(
        "offline mode refused to resolve unpinned companion '{identifier}'; run `ocx patch sync` without --offline"
    )]
    PolicyBlocked {
        /// The companion whose tag could not be resolved.
        identifier: Box<crate::oci::Identifier>,
    },
}

impl ClassifyExitCode for PatchError {
    fn classify(&self) -> Option<ExitCode> {
        match self {
            // Network fetch failures delegate to the inner OCI client error's
            // classification so auth (80), unavailable (69), etc. propagate.
            Self::FetchFailed { source } => source.classify(),
            // Blob-write failures are I/O errors.
            Self::BlobWriteFailed { .. } => Some(ExitCode::IoError),
            // A deliberate local policy refused the resolution — not a fault.
            Self::PolicyBlocked { .. } => Some(ExitCode::PolicyBlocked),
            // Descriptor shape / version / parse issues = malformed data.
            Self::InvalidDescriptorJson { .. }
            | Self::UnsupportedVersion { .. }
            | Self::UnsupportedSnapshotVersion { .. }
            | Self::UnexpectedManifest { .. }
            | Self::UnexpectedArtifactType { .. }
            | Self::WrongLayerCount { .. }
            | Self::UnexpectedLayerMediaType { .. }
            | Self::LayerSizeExceeded { .. }
            | Self::LayerDigestMismatch { .. }
            | Self::ManifestDigestMismatch { .. }
            | Self::DescriptorTooLarge { .. } => Some(ExitCode::DataError),
        }
    }
}
