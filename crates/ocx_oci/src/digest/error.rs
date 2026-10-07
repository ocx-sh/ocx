// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// Errors that can occur when parsing a digest string.
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
pub enum DigestError {
    /// The digest string is not a valid OCI content digest.
    #[error("invalid package digest: {0}")]
    #[exit(
        DataError,
        slug = "invalid_digest",
        summary = "A digest is not a valid algorithm-prefixed hash"
    )]
    Invalid(String),
}
