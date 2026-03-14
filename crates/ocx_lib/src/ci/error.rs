// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// Errors specific to CI environment export operations.
#[derive(Debug)]
pub enum Error {
    /// A required CI environment variable (e.g. `$GITHUB_PATH`) is not set.
    MissingEnv(String),
    /// A file I/O error occurred while writing to a CI runtime file.
    File(std::path::PathBuf, std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::MissingEnv(name) => {
                write!(
                    f,
                    "CI environment variable '${name}' is not set. Is this running inside a CI system?"
                )
            }
            Error::File(path, error) => write!(f, "Failed to write CI file '{}': {error}", path.display()),
        }
    }
}

impl std::error::Error for Error {}
