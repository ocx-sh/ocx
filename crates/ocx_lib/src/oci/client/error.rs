// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

/// Errors that can occur during OCI client operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ClientError {
    /// Authentication with the registry failed.
    #[error("Registry authentication failed: {0}")]
    Authentication(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// Manifest digest mismatch between expected and actual.
    #[error("Manifest digest mismatch: expected '{expected}', got '{actual}'")]
    DigestMismatch { expected: String, actual: String },
    /// Expected an image manifest but got an image index or unknown type.
    #[error("Expected an image manifest, got an image index")]
    UnexpectedManifestType,
    /// Manifest structure is invalid (e.g. wrong layer count, missing fields).
    #[error("Invalid manifest: {0}")]
    InvalidManifest(String),
    /// The requested manifest does not exist in the registry.
    #[error("Manifest not found: {0}")]
    ManifestNotFound(String),
    /// A referenced blob does not exist in the registry.
    ///
    /// `digest_str` is the stringified OCI digest (e.g.
    /// `sha256:<hex>`). It is already flattened from [`crate::oci::Digest`]
    /// at the call site, so the field name reflects that callers
    /// cannot assume it is re-parseable back into a structured type.
    #[error("Blob not found in registry '{registry}': {digest_str}")]
    BlobNotFound { registry: String, digest_str: String },
    /// A registry operation failed.
    #[error("Registry operation failed: {0}")]
    Registry(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// File I/O error with path context.
    #[error("I/O error for '{}': {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// JSON serialization or deserialization failed.
    #[error("Serialization error: {0}")]
    Serialization(#[source] serde_json::Error),
    /// Invalid UTF-8 encoding encountered.
    #[error("Invalid UTF-8 encoding: {0}")]
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
}
