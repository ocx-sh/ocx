// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// Errors specific to CI environment export operations.
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
#[exit(family = "CiError")]
pub enum Error {
    /// A required CI environment variable (e.g. `$GITHUB_PATH`) is not set.
    #[error("CI environment variable '${0}' is not set; is this running inside a CI system")]
    #[exit(
        ConfigError,
        slug = "ci_missing_env",
        summary = "A CI environment variable the export needs is not set"
    )]
    MissingEnv(String),
    /// Writing a CI runtime file failed.
    #[error("failed to write CI file '{path}': {source}")]
    #[exit(IoError, slug = "ci_file_write", summary = "Writing a CI export file failed")]
    File {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Writing a pathless CI export stream (the GitLab stdout sink) failed.
    #[error("failed to write CI export stream: {0}")]
    #[exit(IoError, slug = "ci_export_write", summary = "Writing the CI export stream failed")]
    Write(#[source] std::io::Error),
}
