// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use super::auth_type::AuthType;

/// Errors that can occur during authentication.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// The provided authentication type string is not recognized.
    #[error("invalid authentication type '{}', valid types are: {}", .0, AuthType::valid_strings().join(", "))]
    InvalidType(String),
    /// A required environment variable for the given auth type is not set.
    #[error("authentication type '{}' requires environment variable '{}' to be set", .0, .1)]
    MissingEnv(AuthType, String),
    /// Failed to retrieve credentials from the Docker credential store.
    #[error("failed to retrieve Docker credentials: {0}")]
    DockerCredentialRetrieval(#[source] crate::native::DockerCredentialRetrievalError),
    /// Failed to read/write `~/.docker/config.json`.
    #[error("failed to write credential store at {path}")]
    WriteConfigFailed {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Underlying credential helper subprocess error.
    #[error("credential helper error")]
    Helper(#[source] docker_credential::CredentialRetrievalError),
    /// No credential store available: no helper on PATH AND user declined plaintext fallback.
    #[error("no credential store available; install a docker-credential-* helper or set credsStore")]
    NoCredentialStoreAvailable,
    /// Registry returned 401 for the supplied credentials during login-time verification.
    #[error("registry '{registry}' rejected credentials")]
    LoginRejected { registry: String },
    /// The login-time probe never completed, so the credential was never judged.
    ///
    /// Split from [`Self::LoginRejected`] because a TLS handshake against a
    /// plain-HTTP registry, a DNS failure and a genuine 401 demand three
    /// different actions. Reporting all three as "rejected credentials" at
    /// exit 80 routes a CI script into the refresh-credentials branch, whose
    /// retry then fails identically forever — and tells a human to rotate a
    /// secret that never left the machine.
    ///
    /// The exit code is delegated to the wrapped
    /// [`ClientError`](crate::client::error::ClientError), so the probe
    /// classifies the same way the rest of the binary classifies the same wire
    /// failure.
    #[error("could not verify credentials against registry '{registry}'")]
    ProbeFailed {
        registry: String,
        #[source]
        source: Box<crate::client::error::ClientError>,
    },
}

// ─────────────────────────── tests ───────────────────────────
//
// One test per row in the Error Taxonomy table of `plan_ocx_login.md`. Pins
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
