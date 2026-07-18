// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

use crate::cli::ClassifyExitCode;
use crate::cli::ExitCode;
use crate::oci::{Digest, Identifier, PinnedIdentifier, native};

/// Errors that can occur during OCI client operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ClientError {
    /// Authentication with the registry failed.
    #[error("registry authentication failed: {0}")]
    Authentication(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// Digest mismatch between expected and actual content hash.
    /// Fires for manifest digests and for verified blob digests.
    #[error("digest mismatch: expected '{expected}', got '{actual}'")]
    DigestMismatch { expected: String, actual: String },
    /// The decompressed output of a layer exceeded the decompression-bomb cap
    /// (CWE-400) before extraction completed. The compressed stream is rejected
    /// rather than allowed to exhaust disk. `cap` is the byte ceiling that was
    /// crossed.
    #[error("decompressed layer exceeded the {cap}-byte cap (possible decompression bomb)")]
    DecompressionCapExceeded { cap: u64 },
    /// Expected an image manifest but got an image index or unknown type.
    #[error("expected an image manifest, got an image index")]
    UnexpectedManifestType,
    /// Manifest structure is invalid (e.g. wrong layer count, missing fields).
    #[error("invalid manifest: {0}")]
    InvalidManifest(String),
    /// A single-layer artifact manifest's `artifactType` did not match what
    /// the caller expected. Raised by
    /// [`crate::oci::Client::fetch_single_layer_artifact`].
    #[error("unexpected artifact type: expected '{expected}', got {actual:?}")]
    UnexpectedArtifactType { expected: String, actual: Option<String> },
    /// A single-layer artifact manifest had zero or more than one layer.
    /// Raised by [`crate::oci::Client::fetch_single_layer_artifact`].
    #[error("expected exactly one layer, got {count}")]
    WrongLayerCount { count: usize },
    /// A single-layer artifact's layer `mediaType` did not match what the
    /// caller expected. Raised by
    /// [`crate::oci::Client::fetch_single_layer_artifact`].
    #[error("unexpected layer media type: expected '{expected}', got '{actual}'")]
    UnexpectedLayerMediaType { expected: String, actual: String },
    /// A single-layer artifact's declared layer size exceeded the
    /// caller-supplied ceiling (CWE-400 pre-check, before any bytes are
    /// fetched). Raised by [`crate::oci::Client::fetch_single_layer_artifact`].
    #[error("layer size {declared} exceeds the maximum allowed {maximum} bytes")]
    LayerSizeExceeded { declared: i64, maximum: u64 },
    /// The requested manifest does not exist in the registry.
    #[error("manifest not found: {0}")]
    ManifestNotFound(String),
    /// The requested repository does not exist in the registry.
    ///
    /// Distinct from [`ClientError::Registry`] so callers can treat an
    /// authoritative "repository absent" (e.g. first publish to a new
    /// repository) differently from a transient registry failure.
    #[error("repository not found: {0}")]
    RepositoryNotFound(String),
    /// A referenced blob does not exist in the registry.
    ///
    /// The identifier is the canonical OCX `registry/repository[:tag]@digest`
    /// form: the registry + repository of the image the lookup was
    /// issued against, plus the missing blob's digest. The advisory
    /// tag (when present) is the tag of the image that triggered the
    /// blob resolution — not the blob itself.
    #[error("blob not found: {0}")]
    BlobNotFound(PinnedIdentifier),
    /// A registry operation failed.
    #[error("registry operation failed: {0}")]
    Registry(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// File I/O error with path context.
    #[error("I/O error for '{}': {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// JSON serialization or deserialization failed.
    #[error("serialization error: {0}")]
    Serialization(#[source] serde_json::Error),
    /// Invalid UTF-8 encoding encountered.
    #[error("invalid UTF-8 encoding: {0}")]
    InvalidEncoding(#[source] std::string::FromUtf8Error),
    /// An internal library error (e.g. codesign, archive processing).
    #[error("{0}")]
    Internal(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// The registry rejected the request with HTTP 401.
    ///
    /// Distinct from [`Self::Authentication`] (credential resolution failure);
    /// this is the registry actively refusing after credentials were sent.
    #[error("registry {registry} returned 401 unauthorized")]
    Unauthorized {
        registry: String,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    /// The registry rejected the request with HTTP 403.
    #[error("registry {registry} returned 403 forbidden")]
    Forbidden {
        registry: String,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    /// The registry rate-limited the request with HTTP 429.
    ///
    /// `retry_after` carries the parsed `Retry-After` header value in seconds
    /// when the registry supplied one. Absent when the header was missing or
    /// unparseable — callers default to a local backoff policy.
    #[error("registry {registry} rate-limited the request")]
    RateLimited {
        registry: String,
        retry_after: Option<u64>,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    /// The registry is unavailable (HTTP 5xx, network failures, timeouts).
    #[error("registry {registry} is unavailable")]
    ServiceUnavailable {
        registry: String,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    /// The registry does not implement the OCI Referrers API and has no
    /// fallback-tag referrers index. Distinct from [`Self::ServiceUnavailable`]:
    /// the registry is reachable but the endpoint is not served.
    #[error("registry {registry} does not support the OCI Referrers API")]
    ReferrersUnsupported { registry: String },
}

impl ClientError {
    /// Wrap any error as a [`ClientError::Internal`].
    pub fn internal(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Internal(Box::new(error))
    }

    /// Builds a [`ClientError::BlobNotFound`] from the image the lookup
    /// was issued against and the missing blob's digest.
    ///
    /// The image's own digest (if any) is dropped — the stored identifier
    /// carries the *blob* digest, which is what was actually missing.
    /// Falls back to [`ClientError::Registry`] if the image reference
    /// cannot produce a well-formed [`PinnedIdentifier`]. This path is
    /// unreachable after a HEAD succeeded against the registry: the
    /// transport has already used `image` to issue a real HTTP request,
    /// so the reference is known-valid by construction. The debug
    /// assertions fire loudly in dev builds to catch any regression.
    pub fn blob_not_found(image: &native::Reference, blob_digest: &Digest) -> Self {
        let identifier = match Identifier::try_from(image.clone()) {
            Ok(id) => id.clone_with_digest(blob_digest.clone()),
            Err(e) => {
                debug_assert!(false, "unreachable after HEAD succeeded: {e}");
                return Self::Registry(Box::new(e));
            }
        };
        match PinnedIdentifier::try_from(identifier) {
            Ok(pinned) => Self::BlobNotFound(pinned),
            Err(e) => {
                debug_assert!(false, "unreachable after HEAD succeeded: {e}");
                Self::Registry(Box::new(e))
            }
        }
    }
}

// ── Shared artifact-fetch shape classification ──────────────────────────────

/// Intermediate classification of a [`ClientError`] returned by
/// [`crate::oci::client::Client::fetch_single_layer_artifact`], shared by
/// every domain that maps the fetch failure onto its own error taxonomy —
/// `crate::patch::persistence` and `crate::managed_config::persistence` had
/// byte-identical match arms translating `ClientError`'s shape-validation
/// variants onto their own (identically-shaped) domain enum. Domains keep
/// their own error type (one enum per module); this only factors out the
/// `ClientError` classification itself.
#[derive(Debug)]
pub(crate) enum ArtifactFetchError {
    /// The manifest was not a single-image manifest (image index or
    /// otherwise unexpected shape).
    UnexpectedManifest { detail: String },
    /// The artifact type did not match what the caller expected.
    UnexpectedArtifactType { actual: Option<String> },
    /// The manifest had zero or more than one layer.
    WrongLayerCount { count: usize },
    /// The layer's `mediaType` did not match what the caller expected.
    UnexpectedLayerMediaType { expected: String, actual: String },
    /// The declared layer size exceeded the caller-supplied ceiling.
    LayerSizeExceeded { declared: i64, maximum: u64 },
    /// Every other `ClientError` — the caller's own catch-all (network, auth,
    /// registry failures, etc).
    Other(ClientError),
}

impl ArtifactFetchError {
    /// Classifies `error`. `manifest_kind` is spliced into the
    /// `UnexpectedManifestType` detail message (e.g. `"__ocx.patch"`,
    /// `"managed config"`) so each domain's message stays specific.
    pub(crate) fn classify(error: ClientError, manifest_kind: &str) -> Self {
        match error {
            ClientError::UnexpectedManifestType => Self::UnexpectedManifest {
                detail: format!("expected image manifest for {manifest_kind}, got image index"),
            },
            ClientError::UnexpectedArtifactType { actual, .. } => Self::UnexpectedArtifactType { actual },
            ClientError::WrongLayerCount { count } => Self::WrongLayerCount { count },
            ClientError::UnexpectedLayerMediaType { expected, actual } => {
                Self::UnexpectedLayerMediaType { expected, actual }
            }
            ClientError::LayerSizeExceeded { declared, maximum } => Self::LayerSizeExceeded { declared, maximum },
            ClientError::InvalidManifest(detail) => Self::UnexpectedManifest { detail },
            source => Self::Other(source),
        }
    }
}

impl ClassifyExitCode for ClientError {
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            Self::Authentication(_) | Self::Unauthorized { .. } => ExitCode::AuthError,
            Self::ManifestNotFound(_) | Self::BlobNotFound(_) | Self::RepositoryNotFound(_) => ExitCode::NotFound,
            Self::Io { .. } => ExitCode::IoError,
            // TODO: inspect inner source to refine (HTTP 429/503 → TempFail,
            // 401/403 → AuthError, timeout → TempFail). For v1, treat every
            // registry operation failure as Unavailable.
            Self::Registry(_) => ExitCode::Unavailable,
            Self::Forbidden { .. } => ExitCode::PermissionDenied,
            Self::RateLimited { .. } => ExitCode::TempFail,
            Self::ServiceUnavailable { .. } => ExitCode::Unavailable,
            Self::ReferrersUnsupported { .. } => ExitCode::ReferrersUnsupported,
            Self::DigestMismatch { .. }
            | Self::DecompressionCapExceeded { .. }
            | Self::UnexpectedManifestType
            | Self::InvalidManifest(_)
            | Self::UnexpectedArtifactType { .. }
            | Self::WrongLayerCount { .. }
            | Self::UnexpectedLayerMediaType { .. }
            | Self::LayerSizeExceeded { .. }
            | Self::Serialization(_)
            | Self::InvalidEncoding(_) => ExitCode::DataError,
            Self::Internal(_) => ExitCode::Failure,
        })
    }
}
