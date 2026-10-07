// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// An error that occurred while parsing an OCI identifier string.
#[derive(Debug, Clone, thiserror::Error, ocx_exit::Classify)]
#[error("invalid identifier '{}': {}", .input, .kind)]
#[exit(
    DataError,
    slug = "invalid_identifier",
    summary = "A package identifier does not parse"
)]
#[non_exhaustive]
pub struct IdentifierError {
    pub input: String,
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
    #[error("identifier cannot be empty")]
    Empty,
    #[error("repository must be lowercase")]
    UppercaseRepository,
    #[error("repository exceeds 255-character limit")]
    RepositoryTooLong,
    #[error("invalid digest format")]
    DigestInvalidFormat,
    /// The identifier format is invalid (cannot be parsed).
    #[error("invalid format")]
    InvalidFormat,
    #[error("identifier must include an explicit registry (e.g. 'ocx.sh/tool:1.0', not 'tool:1.0')")]
    MissingRegistry,
    #[error("identifier must not use '.' or '..' as a path segment")]
    DirectoryTraversal,
    #[error("Docker Hub default domain is not supported")]
    DockerHubDefault,
}
