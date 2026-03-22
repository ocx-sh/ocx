// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// An error that occurred while parsing an OCI identifier string.
#[derive(Debug, Clone, thiserror::Error)]
#[error("invalid identifier '{}': {}", .input, .kind)]
#[non_exhaustive]
pub struct IdentifierError {
    /// The raw input that failed to parse.
    pub input: String,
    /// The specific reason parsing failed.
    pub kind: IdentifierErrorKind,
}

impl IdentifierError {
    pub fn new(input: impl Into<String>, kind: IdentifierErrorKind) -> Self {
        Self {
            input: input.into(),
            kind,
        }
    }
}

/// The specific reason an identifier string failed to parse.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum IdentifierErrorKind {
    /// The input string was empty.
    #[error("identifier cannot be empty")]
    Empty,
    /// The repository portion contains uppercase characters.
    #[error("repository must be lowercase")]
    UppercaseRepository,
    /// The repository portion exceeds the 255-character limit.
    #[error("repository exceeds 255-character limit")]
    RepositoryTooLong,
    /// The digest uses an unsupported algorithm.
    #[error("unsupported digest algorithm '{0}'")]
    DigestUnsupported(String),
    /// The digest hex string has an invalid length.
    #[error("digest {algorithm} hex must be {expected} chars, got {actual}")]
    DigestInvalidLength {
        algorithm: String,
        expected: usize,
        actual: usize,
    },
    /// The digest string has an invalid format (missing prefix, non-hex chars, etc.).
    #[error("invalid digest format")]
    DigestInvalidFormat,
    /// The identifier format is invalid (cannot be parsed).
    #[error("invalid format")]
    InvalidFormat,
    /// The identifier resolved to a Docker Hub default domain, which is not supported.
    #[error("Docker Hub default domain is not supported")]
    DockerHubDefault,
}
