// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use super::auth_type::AuthType;
use crate::cli::ClassifyExitCode;
use crate::cli::ExitCode;

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
    /// [`ClientError`](crate::oci::client::error::ClientError), so the probe
    /// classifies the same way the rest of the binary classifies the same wire
    /// failure.
    #[error("could not verify credentials against registry '{registry}'")]
    ProbeFailed {
        registry: String,
        #[source]
        source: Box<crate::oci::client::error::ClientError>,
    },
}

impl ClassifyExitCode for AuthError {
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            Self::InvalidType(_) | Self::MissingEnv(_, _) => ExitCode::ConfigError,
            Self::DockerCredentialRetrieval(_) => ExitCode::AuthError,
            Self::WriteConfigFailed { .. } => ExitCode::IoError,
            Self::Helper(inner) => match inner {
                docker_credential::CredentialRetrievalError::NotOnPath { .. }
                | docker_credential::CredentialRetrievalError::UnsafePath { .. } => ExitCode::ConfigError,
                docker_credential::CredentialRetrievalError::Timeout { .. } => ExitCode::TempFail,
                docker_credential::CredentialRetrievalError::InvalidJson(_) => ExitCode::DataError,
                _ => ExitCode::AuthError,
            },
            Self::NoCredentialStoreAvailable => ExitCode::ConfigError,
            Self::LoginRejected { .. } => ExitCode::AuthError,
            // Provenance only: the probe adds context, the wire failure keeps
            // its own code. Anything else would put a second taxonomy for the
            // same failures beside the one every other command uses.
            Self::ProbeFailed { source, .. } => return source.classify(),
        })
    }
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
    use std::path::PathBuf;

    fn ec(e: AuthError) -> Option<ExitCode> {
        e.classify()
    }

    #[test]
    fn write_config_failed_classifies_to_io_error() {
        let e = AuthError::WriteConfigFailed {
            path: PathBuf::from("/tmp/x"),
            source: std::io::Error::other("e"),
        };
        assert_eq!(ec(e), Some(ExitCode::IoError));
    }

    #[test]
    fn helper_not_on_path_classifies_to_config_error() {
        let e = AuthError::Helper(Helper::NotOnPath { name: "x".into() });
        assert_eq!(ec(e), Some(ExitCode::ConfigError));
    }

    #[test]
    fn helper_unsafe_path_classifies_to_config_error() {
        let e = AuthError::Helper(Helper::UnsafePath {
            name: "x".into(),
            path: PathBuf::from("/tmp/x"),
        });
        assert_eq!(ec(e), Some(ExitCode::ConfigError));
    }

    #[test]
    fn helper_timeout_classifies_to_temp_fail() {
        let e = AuthError::Helper(Helper::Timeout { seconds: 30 });
        assert_eq!(ec(e), Some(ExitCode::TempFail));
    }

    #[test]
    fn helper_invalid_json_classifies_to_data_error() {
        let invalid = serde_json::from_str::<serde_json::Value>("not json").expect_err("must err");
        let e = AuthError::Helper(Helper::InvalidJson(invalid));
        assert_eq!(ec(e), Some(ExitCode::DataError));
    }

    #[test]
    fn helper_other_classifies_to_auth_error() {
        let e = AuthError::Helper(Helper::HelperCommunicationError);
        assert_eq!(ec(e), Some(ExitCode::AuthError));
    }

    #[test]
    fn no_credential_store_available_classifies_to_config_error() {
        let e = AuthError::NoCredentialStoreAvailable;
        assert_eq!(ec(e), Some(ExitCode::ConfigError));
    }

    #[test]
    fn login_rejected_classifies_to_auth_error() {
        let e = AuthError::LoginRejected {
            registry: "ghcr.io".into(),
        };
        assert_eq!(ec(e), Some(ExitCode::AuthError));
    }

    /// A probe that never completed must NOT read as 80. Asserting two
    /// different wrapped failures is what pins the delegation — a single case
    /// passes just as well against a hardcoded constant — and asserting
    /// `!= AuthError` is what pins the split from `LoginRejected`, which is the
    /// bug: exit 80 routes a CI script into "refresh credentials", and the
    /// retry with a fresh credential fails identically forever.
    #[test]
    fn a_failed_probe_delegates_its_exit_code_and_never_reads_as_rejected_credentials() {
        use crate::oci::client::error::ClientError;

        let probe = |source: ClientError| AuthError::ProbeFailed {
            registry: "localhost:5000".into(),
            source: Box::new(source),
        };

        assert_eq!(
            ec(probe(ClientError::RegistryTransient(Box::new(std::io::Error::other(
                "connect refused"
            ))))),
            Some(ExitCode::TempFail),
        );
        assert_eq!(
            ec(probe(ClientError::UnsafeDestination(Box::new(std::io::Error::other(
                "plaintext realm"
            ))))),
            Some(ExitCode::DataError),
        );
        assert_ne!(
            ec(probe(ClientError::RegistryTransient(Box::new(std::io::Error::other(
                "connect refused"
            ))))),
            Some(ExitCode::AuthError),
            "the credential was never judged, so this must not classify as an auth failure"
        );
    }

    #[test]
    fn helper_output_too_large_classifies_to_auth_error() {
        let e = AuthError::Helper(Helper::OutputTooLarge { cap_bytes: 65536 });
        assert_eq!(ec(e), Some(ExitCode::AuthError));
    }

    #[test]
    fn helper_helper_failure_classifies_to_auth_error() {
        let e = AuthError::Helper(Helper::HelperFailure {
            helper: "test".into(),
            stdout: "".into(),
            stderr: "boom".into(),
        });
        assert_eq!(ec(e), Some(ExitCode::AuthError));
    }

    #[test]
    fn helper_helper_communication_error_classifies_to_auth_error() {
        let e = AuthError::Helper(Helper::HelperCommunicationError);
        assert_eq!(ec(e), Some(ExitCode::AuthError));
    }
}
