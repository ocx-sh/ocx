// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use super::auth_type::AuthType;
use crate::cli::ExitCode;
use crate::cli::classify::ClassifyExitCode;

/// Errors that can occur during authentication.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AuthError {
    /// The provided authentication type string is not recognized.
    #[error("invalid authentication type '{}', valid types are: {}", .0, AuthType::valid_strings().join(", "))]
    InvalidType(String),
    /// A required environment variable for the given auth type is not set.
    #[error("authentication type '{}' requires environment variable '{}' to be set", .0, .1)]
    MissingEnv(AuthType, String),
    /// Failed to retrieve credentials from the Docker credential store.
    #[error("failed to retrieve Docker credentials: {0}")]
    DockerCredentialRetrieval(#[source] crate::oci::native::DockerCredentialRetrievalError),
}

impl ClassifyExitCode for AuthError {
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            Self::InvalidType(_) | Self::MissingEnv(_, _) => ExitCode::ConfigError,
            Self::DockerCredentialRetrieval(_) => ExitCode::AuthError,
        })
    }
}
