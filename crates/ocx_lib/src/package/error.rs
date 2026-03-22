// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

/// Errors specific to package metadata, versioning, and description operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A package version string could not be parsed.
    #[error("Invalid package version: {0}")]
    VersionInvalid(String),

    /// A logo file has an unsupported image format.
    #[error("Unsupported logo format: {0}")]
    UnsupportedLogoFormat(String),

    /// A required path does not exist.
    #[error("Required path does not exist: {}", .0.display())]
    RequiredPathMissing(PathBuf),
}
