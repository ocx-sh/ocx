// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// Errors that can occur when parsing a digest string.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DigestError {
    /// The digest string is not a valid OCI content digest.
    #[error("Invalid package digest: {0}")]
    Invalid(String),
}
