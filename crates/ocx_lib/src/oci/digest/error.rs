// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::cli::ExitCode;
use crate::cli::classify::ClassifyExitCode;

/// Errors that can occur when parsing a digest string.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DigestError {
    /// The digest string is not a valid OCI content digest.
    #[error("invalid package digest: {0}")]
    Invalid(String),
}

impl ClassifyExitCode for DigestError {
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            Self::Invalid(_) => ExitCode::DataError,
        })
    }
}
