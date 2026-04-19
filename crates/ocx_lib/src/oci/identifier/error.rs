// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::cli::ClassifyExitCode;
use crate::cli::ExitCode;

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
    /// The digest string is invalid (unsupported algorithm, wrong length, non-hex chars, etc.).
    #[error("invalid digest format")]
    DigestInvalidFormat,
    /// The identifier format is invalid (cannot be parsed).
    #[error("invalid format")]
    InvalidFormat,
    /// The identifier does not contain an explicit registry.
    #[error("identifier must include an explicit registry (e.g. 'ocx.sh/tool:1.0', not 'tool:1.0')")]
    MissingRegistry,
    /// The identifier contains a directory traversal segment (`.` or `..`).
    #[error("identifier must not use '.' or '..' as a path segment")]
    DirectoryTraversal,
    /// The identifier resolved to a Docker Hub default domain, which is not supported.
    #[error("Docker Hub default domain is not supported")]
    DockerHubDefault,
}

impl ClassifyExitCode for IdentifierError {
    fn classify(&self) -> Option<ExitCode> {
        // Every identifier parse failure is a malformed-input error.
        Some(ExitCode::DataError)
    }
}
