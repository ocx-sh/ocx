// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

use crate::cli::ExitCode;
use crate::cli::classify::ClassifyExitCode;

/// An error that occurred while loading or saving a profile manifest.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProfileError {
    /// The manifest file could not be read or written.
    #[error("profile manifest I/O error for '{}': {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The manifest JSON could not be parsed or serialized.
    #[error("profile manifest JSON error for '{}': {source}", path.display())]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    /// The manifest version is not supported by this version of ocx.
    #[error(
        "unsupported profile manifest version {version} in '{}' (supported: {supported}). \
         A newer version of ocx may be required.",
        path.display()
    )]
    UnsupportedVersion {
        path: PathBuf,
        version: u32,
        supported: u32,
    },
    /// The profile manifest is locked by another process.
    #[error("profile manifest '{}' is locked by another process", path.display())]
    Locked { path: PathBuf },
    /// Content mode requires a digest on the identifier for direct object store resolution.
    #[error("content mode requires a digest for '{identifier}'")]
    ContentModeRequiresDigest { identifier: String },
}

impl ClassifyExitCode for ProfileError {
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            Self::Io { .. } => ExitCode::IoError,
            Self::Json { .. } | Self::UnsupportedVersion { .. } | Self::ContentModeRequiresDigest { .. } => {
                ExitCode::DataError
            }
            Self::Locked { .. } => ExitCode::TempFail,
        })
    }
}
