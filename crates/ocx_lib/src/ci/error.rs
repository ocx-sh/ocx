// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// Errors specific to CI environment export operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A required CI environment variable (e.g. `$GITHUB_PATH`) is not set.
    #[error("CI environment variable '${0}' is not set. Is this running inside a CI system?")]
    MissingEnv(String),
    /// A file I/O error occurred while writing to a CI runtime file.
    #[error("Failed to write CI file '{path}': {source}")]
    File {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
}
