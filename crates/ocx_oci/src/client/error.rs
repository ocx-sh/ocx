// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

use crate::{Digest, OciIdentifier, PinnedOciIdentifier, native};

/// Errors that can occur during OCI client operations.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// Authentication with the registry failed.
    #[error("registry authentication failed: {0}")]
    Authentication(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// Digest mismatch between expected and actual content hash, for manifests and verified blobs.
    #[error("digest mismatch: expected '{expected}', got '{actual}'")]
    DigestMismatch { expected: String, actual: String },
    /// The transport delivered fewer bytes than the manifest-declared blob size.
    ///
    /// Kept apart from [`ClientError::DigestMismatch`], or every truncated transfer reads as the registry
    /// serving wrong content.
    #[error("short blob read: got {actual} of {expected} bytes")]
    ShortBlobRead { expected: u64, actual: u64 },
    /// A layer's decompressed output crossed the `cap`-byte decompression-bomb ceiling (CWE-400).
    #[error("decompressed layer exceeded the {cap}-byte cap (possible decompression bomb)")]
    DecompressionCapExceeded { cap: u64 },
    /// Expected an image manifest but got an image index or unknown type.
    #[error("expected an image manifest, got an image index")]
    UnexpectedManifestType,
    /// Manifest structure is invalid (e.g. wrong layer count, missing fields).
    #[error("invalid manifest: {0}")]
    InvalidManifest(String),
    /// The registry answered a manifest request with something that cannot be a manifest (a login portal, a
    /// proxy error page).
    #[error("registry did not answer with a manifest")]
    NotAManifest(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// A served image index violates the OCI image spec; refused before its bytes reach the index.
    #[error(transparent)]
    InvalidImageIndex(#[from] crate::manifest::InvalidImageIndex),
    /// A single-layer artifact's `artifactType` was not the expected one.
    #[error("unexpected artifact type: expected '{expected}', got {actual:?}")]
    UnexpectedArtifactType { expected: String, actual: Option<String> },
    /// A single-layer artifact manifest had zero or more than one layer.
    #[error("expected exactly one layer, got {count}")]
    WrongLayerCount { count: usize },
    /// A single-layer artifact's layer `mediaType` was not the expected one.
    #[error("unexpected layer media type: expected '{expected}', got '{actual}'")]
    UnexpectedLayerMediaType { expected: String, actual: String },
    /// A single-layer artifact's declared size exceeded the caller's ceiling, checked before any fetch.
    #[error("layer size {declared} exceeds the maximum allowed {maximum} bytes")]
    LayerSizeExceeded { declared: i64, maximum: u64 },
    /// A registry-supplied graph exceeded a traversal limit.
    ///
    /// Refused, never truncated: a copy that dropped a signature past the limit would report success for an
    /// unsigned target.
    #[error("{limit_kind} limit of {limit} exceeded (reached {actual}) while copying {subject}")]
    TraversalLimitExceeded {
        limit_kind: TraversalLimit,
        limit: usize,
        actual: usize,
        subject: String,
    },
    /// The requested manifest does not exist in the registry.
    #[error("manifest not found: {0}")]
    ManifestNotFound(String),
    /// The requested repository does not exist; callers treat this (a first publish) apart from
    /// [`ClientError::Registry`].
    #[error("repository not found: {0}")]
    RepositoryNotFound(String),
    /// A referenced blob does not exist: the looked-up image's `registry/repository[:tag]` plus the missing
    /// blob's digest.
    #[error("blob not found: {0}")]
    BlobNotFound(Box<PinnedOciIdentifier>),
    /// A registry operation failed.
    #[error("registry operation failed: {0}")]
    Registry(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// The registry named a destination the transport refuses: an off-origin upload `Location` (CWE-918)
    /// or a plaintext auth realm from an HTTPS registry (CWE-319).
    #[error("registry named a destination the transport refuses: {0}")]
    UnsafeDestination(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// The registry answered with a redirect the transport did not follow: a declined `https`->`http` hop,
    /// a declined mid-upload handoff, the hop limit, or a missing or unparseable `Location`.
    #[error("registry answered with a redirect the transport did not follow: {0}")]
    UnfollowedRedirect(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// A failure that may not repeat: connect or timeout, or a 429 / 502 / 503 / 504.
    ///
    /// Kept apart from [`ClientError::Registry`], which answers the same way on a rerun.
    #[error("transient registry failure: {0}")]
    RegistryTransient(#[source] Box<dyn std::error::Error + Send + Sync>),
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
    /// A digest string the registry served could not be parsed.
    #[error(transparent)]
    Digest(#[from] crate::digest::error::DigestError),
    /// An internal library error (e.g. codesign, archive processing).
    #[error("{0}")]
    Internal(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// A read routed through a configured mirror failed, with the routing that caused it.
    ///
    /// Never wraps a not-found sentinel, or callers matching it as "absent" turn a missing tag into a hard failure.
    #[error("fetching '{physical}' via mirror '{mirror}' configured for '{origin}'")]
    Mirrored {
        origin: String,
        mirror: String,
        physical: String,
        #[source]
        source: Box<ClientError>,
    },

    /// The reachable registry has no referrers path this operation can use; the raising site logs the cause.
    #[error("registry {registry} has no usable OCI referrers path for this subject")]
    ReferrersUnsupported { registry: String },
}

/// Which bounded traversal ran out of room, for [`ClientError::TraversalLimitExceeded`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraversalLimit {
    /// How deep a referrer-of-a-referrer chain is followed.
    ReferrerDepth,
    /// How many referrer manifests one leaf may carry, across the whole chain.
    ReferrersPerLeaf,
    /// How many distinct blobs one manifest may name.
    BlobsPerManifest,
}

impl std::fmt::Display for TraversalLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::ReferrerDepth => "referrer depth",
            Self::ReferrersPerLeaf => "referrers per leaf",
            Self::BlobsPerManifest => "blobs per manifest",
        })
    }
}

impl ClientError {
    /// Wrap any error as a [`ClientError::Internal`].
    pub fn internal(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Internal(Box::new(error))
    }

    /// Builds a [`ClientError::BlobNotFound`] naming the looked-up image and the missing blob's digest.
    ///
    /// Falls back to [`ClientError::Registry`] on an unparseable reference, unreachable after a HEAD succeeded.
    pub fn blob_not_found(image: &native::Reference, blob_digest: &Digest) -> Self {
        let identifier = match OciIdentifier::from_native(image.clone()) {
            Ok(id) => id.clone_with_digest(blob_digest.clone()),
            Err(e) => {
                debug_assert!(false, "unreachable after HEAD succeeded: {e}");
                return Self::Registry(Box::new(e));
            }
        };
        match PinnedOciIdentifier::try_from(identifier) {
            Ok(pinned) => Self::BlobNotFound(Box::new(pinned)),
            Err(e) => {
                debug_assert!(false, "unreachable after HEAD succeeded: {e}");
                Self::Registry(Box::new(e))
            }
        }
    }
}

// ── Shared artifact-fetch shape classification ──────────────────────────────

/// Shape classification of a [`crate::client::Client::fetch_single_layer_artifact`] failure, for callers
/// mapping it onto their own error enum.
#[derive(Debug)]
pub enum ArtifactFetchError {
    /// The manifest was not a single-image manifest.
    UnexpectedManifest { detail: String },
    /// The artifact type did not match what the caller expected.
    UnexpectedArtifactType { actual: Option<String> },
    /// The manifest had zero or more than one layer.
    WrongLayerCount { count: usize },
    /// The layer's `mediaType` did not match what the caller expected.
    UnexpectedLayerMediaType { expected: String, actual: String },
    /// The declared layer size exceeded the caller-supplied ceiling.
    LayerSizeExceeded { declared: i64, maximum: u64 },
    /// Every other `ClientError`.
    Other(ClientError),
}

impl ArtifactFetchError {
    /// Classifies `error`; `manifest_kind` names the artifact in the unexpected-manifest detail.
    pub fn classify(error: ClientError, manifest_kind: &str) -> Self {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The message must name all three: what was fetched, the mirror it went
    /// to, and the upstream that mirror stands in for. Any one alone leaves the
    /// reader unable to connect the failure to the config entry behind it.
    #[test]
    fn a_mirrored_failure_names_the_routing_in_its_message() {
        let rendered = ClientError::Mirrored {
            origin: "ghcr.io".to_string(),
            mirror: "artifactory.example.com".to_string(),
            physical: "artifactory.example.com/ghcr-remote/owner/tool:1.0".to_string(),
            source: Box::new(ClientError::NotAManifest(Box::new(std::io::Error::other(
                "unexpected content type 'text/html'",
            )))),
        }
        .to_string();

        assert!(rendered.contains("ghcr.io"), "must name the upstream, got: {rendered}");
        assert!(
            rendered.contains("artifactory.example.com"),
            "must name the mirror, got: {rendered}"
        );
        assert!(
            rendered.contains("ghcr-remote/owner/tool:1.0"),
            "must name the physical reference, got: {rendered}"
        );
    }
}
