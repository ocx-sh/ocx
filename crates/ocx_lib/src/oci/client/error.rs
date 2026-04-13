// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

use crate::cli::ExitCode;
use crate::cli::classify::ClassifyExitCode;
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
    /// Expected an image manifest but got an image index or unknown type.
    #[error("expected an image manifest, got an image index")]
    UnexpectedManifestType,
    /// Manifest structure is invalid (e.g. wrong layer count, missing fields).
    #[error("invalid manifest: {0}")]
    InvalidManifest(String),
    /// The requested manifest does not exist in the registry.
    #[error("manifest not found: {0}")]
    ManifestNotFound(String),
    /// A referenced blob does not exist in the registry.
    ///
    /// The identifier is the canonical OCX `registry/repository[:tag]@digest`
    /// form: the registry + repository of the image the lookup was
    /// issued against, plus the missing blob's digest. The advisory
    /// tag (when present) is the tag of the image that triggered the
    /// blob resolution — not the blob itself.
    #[error("Blob not found: {0}")]
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

impl ClassifyExitCode for ClientError {
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            Self::Authentication(_) => ExitCode::AuthError,
            Self::ManifestNotFound(_) | Self::BlobNotFound(_) => ExitCode::NotFound,
            Self::Io { .. } => ExitCode::IoError,
            // TODO: inspect inner source to refine (HTTP 429/503 → TempFail,
            // 401/403 → AuthError, timeout → TempFail). For v1, treat every
            // registry operation failure as Unavailable.
            Self::Registry(_) => ExitCode::Unavailable,
            Self::DigestMismatch { .. }
            | Self::UnexpectedManifestType
            | Self::InvalidManifest(_)
            | Self::Serialization(_)
            | Self::InvalidEncoding(_) => ExitCode::DataError,
            Self::Internal(_) => ExitCode::Failure,
        })
    }
}
