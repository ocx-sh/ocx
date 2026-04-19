// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

use crate::cli::ClassifyExitCode;
use crate::cli::ExitCode;

/// Errors specific to package metadata, versioning, and description operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A package version string could not be parsed.
    #[error("invalid package version: {0}")]
    VersionInvalid(String),

    /// A logo file has an unsupported image format.
    #[error("unsupported logo format: {0}")]
    UnsupportedLogoFormat(String),

    /// A required path does not exist.
    #[error("required path does not exist: {}", .0.display())]
    RequiredPathMissing(PathBuf),
}

impl ClassifyExitCode for Error {
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            Self::VersionInvalid(_) | Self::UnsupportedLogoFormat(_) => ExitCode::DataError,
            Self::RequiredPathMissing(_) => ExitCode::NotFound,
        })
    }
}
