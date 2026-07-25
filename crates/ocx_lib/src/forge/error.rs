// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Error taxonomy for the forge REST client.

use crate::cli::{ClassifyExitCode, ExitCode};

/// Failures raised by the forge client.
///
/// The token is never carried in any variant — messages reference URLs and
/// HTTP status codes only, never the bearer credential (design register X6).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ForgeError {
    /// A repository coordinate string is not in `owner/repo` form.
    #[error("invalid repository coordinate {value}, expected owner/repo")]
    InvalidRepoCoordinate { value: String },

    /// The no-redirect forge HTTP client could not be constructed.
    #[error("failed to build the forge HTTP client")]
    ClientBuild {
        #[source]
        source: reqwest::Error,
    },

    /// A request never completed (connect, TLS, timeout, or read failure).
    #[error("forge request to {url} failed")]
    Transport {
        url: String,
        #[source]
        source: reqwest::Error,
    },

    /// The forge answered with a non-success HTTP status.
    #[error("forge returned HTTP status {status} for {url}")]
    Status { url: String, status: u16 },

    /// A request body could not be serialized before sending.
    #[error("failed to encode a forge request body")]
    RequestEncode {
        #[source]
        source: serde_json::Error,
    },

    /// A success response body could not be parsed as JSON.
    #[error("failed to decode the forge response from {url}")]
    Decode {
        url: String,
        #[source]
        source: serde_json::Error,
    },

    /// A success response body lacked a field the client needs.
    #[error("forge response from {url} is missing the field {field}")]
    MissingField { url: String, field: String },

    /// A fork's parent does not match the upstream repository — a same-named
    /// stranger repository (design register X5, refuse before any write).
    #[error("fork parent {actual} does not match upstream {expected}")]
    ForkParentMismatch { expected: String, actual: String },

    /// A fork response carries no parent to verify against the upstream.
    #[error("fork response carries no parent to verify against upstream {expected}")]
    ForkParentAbsent { expected: String },

    /// A fork response lacked an identity field.
    #[error("fork response is missing the field {field}")]
    ForkFieldMissing { field: String },

    /// A fork's `full_name` is not in `owner/repo` form.
    #[error("fork full_name {full_name} is not in owner/repo form")]
    MalformedForkFullName { full_name: String },

    /// A verified fork is not owned by the requested owner (S12 shared-fork
    /// path — the returned identity must live under the requested owner).
    #[error("fork owner {actual} does not match the requested owner {expected}")]
    ForkOwnerMismatch { expected: String, actual: String },

    /// A fork did not become ready within the bounded readiness deadline.
    #[error("fork not ready within {deadline_secs}s")]
    ForkNotReady { deadline_secs: u64 },

    /// A compare response carried a `status` value the client does not model.
    /// Ancestry is never guessed: an unmodelled value would otherwise read as
    /// "not ahead" and strand a committed announce with no pull request
    /// (design register C6 amendment).
    #[error("forge compare {url} returned an unmodelled status {status}")]
    UnknownCompareStatus { url: String, status: String },

    /// A fast-forward-only ref update was rejected — a concurrent announce
    /// advanced the branch (design register C4, compare-and-swap). The caller
    /// re-reads the new head, regenerates, and retries.
    #[error("ref update for branch {branch} is not a fast-forward")]
    NonFastForward { branch: String },
}

impl ClassifyExitCode for ForgeError {
    fn classify(&self) -> Option<ExitCode> {
        match self {
            // 401/403 — the bearer `OCX_ANNOUNCE_TOKEN` is missing, revoked,
            // or lacks scope. The fix is a credential, not a config file or a
            // retry (design register C13).
            Self::Status { status, .. } if *status == 401 || *status == 403 => Some(ExitCode::AuthError),
            // 429 — a secondary rate limit. The request was well-formed and the
            // credential is fine; the same call succeeds after a backoff, so a
            // CI wrapper must be able to tell it apart from bad input.
            Self::Status { status, .. } if *status == 429 => Some(ExitCode::TempFail),
            // 5xx — a forge-side incident. The request completed, so it is not
            // `Transport`, but the forge is just as unavailable and a retry is
            // just as reasonable; without this it is indistinguishable from
            // malformed input at exit 1.
            Self::Status { status, .. } if (500..=599).contains(status) => Some(ExitCode::Unavailable),
            // A request never completed (connect, TLS, timeout, DNS, or read
            // failure) — the forge itself is unreachable.
            Self::Transport { .. } => Some(ExitCode::Unavailable),
            // A persistent non-fast-forward (heavy branch contention the one
            // in-announce retry did not clear) is a transient failure the
            // caller may retry (design register C4).
            Self::NonFastForward { .. } => Some(ExitCode::TempFail),
            // Every other status code and every other variant is not yet
            // classified beyond the sysexits default (`ExitCode::Failure`).
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `reqwest::Error` exposes no public constructor; a malformed URL fails
    /// `RequestBuilder::build()` synchronously (no network access), giving a
    /// real value to wrap in `Transport` for the classification test.
    fn transport_error() -> reqwest::Error {
        reqwest::Client::new()
            .get("not a valid url")
            .build()
            .expect_err("a malformed URL must fail to build without any network access")
    }

    #[test]
    fn status_401_maps_to_auth_error() {
        let error = ForgeError::Status {
            url: "https://api.github.com/user".to_string(),
            status: 401,
        };
        assert_eq!(error.classify(), Some(ExitCode::AuthError));
    }

    #[test]
    fn status_403_maps_to_auth_error() {
        let error = ForgeError::Status {
            url: "https://api.github.com/user".to_string(),
            status: 403,
        };
        assert_eq!(error.classify(), Some(ExitCode::AuthError));
    }

    #[test]
    fn status_429_maps_to_temp_fail() {
        let error = ForgeError::Status {
            url: "https://api.github.com/repos/x/y/git/blobs".to_string(),
            status: 429,
        };
        assert_eq!(error.classify(), Some(ExitCode::TempFail));
    }

    #[test]
    fn server_error_statuses_map_to_unavailable() {
        for status in [500, 502, 503, 599] {
            let error = ForgeError::Status {
                url: "https://api.github.com/repos/x/y/forks".to_string(),
                status,
            };
            assert_eq!(
                error.classify(),
                Some(ExitCode::Unavailable),
                "HTTP {status} is a forge-side incident, retryable"
            );
        }
    }

    #[test]
    fn status_other_is_unclassified() {
        // A 404 in particular stays unclassified: the indeterminate-compare
        // fall-through (C6 amendment) deliberately rides on it.
        for status in [404, 422] {
            let error = ForgeError::Status {
                url: "https://api.github.com/repos/x/y/forks".to_string(),
                status,
            };
            assert_eq!(error.classify(), None, "HTTP {status} has no dedicated exit code");
        }
    }

    #[test]
    fn transport_failure_maps_to_unavailable() {
        let error = ForgeError::Transport {
            url: "https://api.github.com/user".to_string(),
            source: transport_error(),
        };
        assert_eq!(error.classify(), Some(ExitCode::Unavailable));
    }

    #[test]
    fn non_fast_forward_maps_to_temp_fail() {
        let error = ForgeError::NonFastForward {
            branch: "indexbot-announce-acme-widget".to_string(),
        };
        assert_eq!(error.classify(), Some(ExitCode::TempFail));
    }
}
