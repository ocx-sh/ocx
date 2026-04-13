// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::cli::ExitCode;
use crate::cli::classify::ClassifyExitCode;

/// Errors specific to CI environment export operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A required CI environment variable (e.g. `$GITHUB_PATH`) is not set.
    #[error("CI environment variable '${0}' is not set; is this running inside a CI system")]
    MissingEnv(String),
    /// A file I/O error occurred while writing to a CI runtime file.
    #[error("failed to write CI file '{path}': {source}")]
    File {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl ClassifyExitCode for Error {
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            Self::MissingEnv(_) => ExitCode::ConfigError,
            Self::File { .. } => ExitCode::IoError,
        })
    }
}
