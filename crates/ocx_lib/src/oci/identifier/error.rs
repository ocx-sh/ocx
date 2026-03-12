// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// An error that occurred while parsing an OCI identifier string.
#[derive(Debug, Clone)]
pub struct IdentifierError {
    /// The raw input that failed to parse.
    pub input: String,
    /// The specific reason parsing failed.
    pub kind: IdentifierErrorKind,
}

/// The specific reason an identifier string failed to parse.
#[derive(Debug, Clone)]
pub enum IdentifierErrorKind {
    /// The input string was empty.
    Empty,
    /// The repository portion contains uppercase characters.
    UppercaseRepository,
    /// The repository portion exceeds the 255-character limit.
    RepositoryTooLong,
    /// The digest uses an unsupported algorithm.
    DigestUnsupported(String),
    /// The digest hex string has an invalid length.
    DigestInvalidLength {
        algorithm: String,
        expected: usize,
        actual: usize,
    },
    /// The digest string has an invalid format (missing prefix, non-hex chars, etc.).
    DigestInvalidFormat,
    /// The identifier format is invalid (cannot be parsed).
    InvalidFormat,
    /// The identifier resolved to a Docker Hub default domain, which is not supported.
    DockerHubDefault,
}

impl std::fmt::Display for IdentifierError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid identifier '{}': ", self.input)?;
        match &self.kind {
            IdentifierErrorKind::Empty => write!(f, "identifier cannot be empty"),
            IdentifierErrorKind::UppercaseRepository => {
                write!(f, "repository must be lowercase")
            }
            IdentifierErrorKind::RepositoryTooLong => {
                write!(f, "repository exceeds 255-character limit")
            }
            IdentifierErrorKind::DigestUnsupported(algo) => {
                write!(f, "unsupported digest algorithm '{algo}'")
            }
            IdentifierErrorKind::DigestInvalidLength {
                algorithm,
                expected,
                actual,
            } => write!(f, "digest {algorithm} hex must be {expected} chars, got {actual}"),
            IdentifierErrorKind::DigestInvalidFormat => write!(f, "invalid digest format"),
            IdentifierErrorKind::InvalidFormat => write!(f, "invalid format"),
            IdentifierErrorKind::DockerHubDefault => {
                write!(f, "Docker Hub default domain is not supported")
            }
        }
    }
}

impl std::error::Error for IdentifierError {}
