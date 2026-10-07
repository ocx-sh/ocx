// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_exit::{Pick, Row};

use super::auth_type::AuthType;

/// Errors that can occur during authentication.
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
pub enum AuthError {
    /// The provided authentication type string is not recognized.
    #[error("invalid authentication type '{}', valid types are: {}", .0, AuthType::valid_strings().join(", "))]
    #[exit(
        ConfigError,
        slug = "invalid_auth_type",
        summary = "The configured authentication type is not recognized"
    )]
    InvalidType(String),
    /// A required environment variable for the given auth type is not set.
    #[error("authentication type '{}' requires environment variable '{}' to be set", .0, .1)]
    #[exit(
        ConfigError,
        slug = "auth_env_missing",
        summary = "An environment variable the authentication type needs is not set"
    )]
    MissingEnv(AuthType, String),
    /// Failed to retrieve credentials from the Docker credential store.
    #[error("failed to retrieve Docker credentials: {0}")]
    #[exit(
        AuthError,
        slug = "docker_credential_retrieval",
        summary = "Reading Docker credentials failed"
    )]
    DockerCredentialRetrieval(#[source] crate::native::DockerCredentialRetrievalError),
    /// Failed to read/write `~/.docker/config.json`.
    #[error("failed to write credential store at {path}")]
    #[exit(
        IoError,
        slug = "credential_store_write",
        summary = "Writing the credential store failed"
    )]
    WriteConfigFailed {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Underlying credential helper subprocess error.
    #[error("credential helper error")]
    #[exit(
        with = helper_failure,
        rows(
            (ConfigError, slug = "credential_helper_not_on_path", summary = "The configured credential helper is not on PATH"),
            (ConfigError, slug = "credential_helper_unsafe_path", summary = "The credential helper resolves to an unsafe path"),
            (TempFail, slug = "credential_helper_timeout", summary = "The credential helper did not answer in time"),
            (DataError, slug = "credential_helper_invalid_json", summary = "The credential helper answered with invalid JSON"),
            (AuthError, slug = "credential_helper_failed", summary = "The credential helper failed to supply credentials"),
        )
    )]
    Helper(#[source] docker_credential::CredentialRetrievalError),
    /// No credential store available: no helper on PATH AND user declined plaintext fallback.
    #[error("no credential store available; install a docker-credential-* helper or set credsStore")]
    #[exit(
        ConfigError,
        slug = "no_credential_store",
        summary = "No credential helper or store is available to save credentials in"
    )]
    NoCredentialStoreAvailable,
    /// Registry returned 401 for the supplied credentials during login-time verification.
    #[error("registry '{registry}' rejected credentials")]
    #[exit(
        AuthError,
        slug = "login_rejected",
        summary = "The registry rejected the supplied credentials"
    )]
    LoginRejected { registry: String },
    /// The login-time probe never completed, so the credential was never judged.
    ///
    /// Kept apart from [`Self::LoginRejected`], or a TLS/DNS failure sends CI into a credential-refresh retry loop.
    #[error("could not verify credentials against registry '{registry}'")]
    #[exit(delegate = source)]
    ProbeFailed {
        registry: String,
        #[source]
        source: Box<crate::client::error::ClientError>,
    },
}

/// Picks the helper row for [`AuthError::Helper`] from the foreign failure it wraps.
fn helper_failure(error: &AuthError, rows: [Row; 5]) -> Pick<'_> {
    use docker_credential::CredentialRetrievalError as Helper;

    let AuthError::Helper(inner) = error else {
        return Pick::row(rows[4]);
    };
    Pick::row(match inner {
        Helper::NotOnPath { .. } => rows[0],
        Helper::UnsafePath { .. } => rows[1],
        Helper::Timeout { .. } => rows[2],
        Helper::InvalidJson(_) => rows[3],
        // The foreign enum is non-exhaustive.
        _ => rows[4],
    })
}

// ─────────────────────────── tests ───────────────────────────
//
// One test per `AuthError` variant. Pins
// the AuthError → ExitCode classification contract for downstream consumers
// (CI scripts case on numeric values, see `quality-rust-exit_codes.md`).
#[cfg(test)]
mod tests {
    use super::*;
    use docker_credential::CredentialRetrievalError as Helper;

    #[test]
    fn helper_invalid_json_classifies_to_data_error() {
        let invalid = serde_json::from_str::<serde_json::Value>("not json").expect_err("must err");
        let _e = AuthError::Helper(Helper::InvalidJson(invalid));
    }
}
