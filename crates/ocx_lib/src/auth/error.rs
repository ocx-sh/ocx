// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use super::auth_type::AuthType;

/// Errors that can occur during authentication.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AuthError {
    /// The provided authentication type string is not recognized.
    #[error("Invalid authentication type '{}', valid types are: {}", .0, AuthType::valid_strings().join(", "))]
    InvalidType(String),
    /// A required environment variable for the given auth type is not set.
    #[error("Authentication type '{}' requires environment variable '{}' to be set", .0, .1)]
    MissingEnv(AuthType, String),
    /// Failed to retrieve credentials from the Docker credential store.
    #[error("Failed to retrieve Docker credentials: {0}")]
    DockerCredentialRetrieval(#[source] crate::oci::native::DockerCredentialRetrievalError),
}
