// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::cli::ExitCode;
use crate::cli::classify::ClassifyExitCode;

/// Errors specific to file structure operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// An identifier was expected to carry a digest but did not.
    #[error("identifier requires a digest: {0}")]
    MissingDigest(String),
}

impl ClassifyExitCode for Error {
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            Self::MissingDigest(_) => ExitCode::DataError,
        })
    }
}
