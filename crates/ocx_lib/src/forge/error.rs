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
    /// A repository coordinate string is not in `[HOST/]NAMESPACE/PROJECT` form.
    #[error("invalid repository coordinate {value}, expected [HOST/]NAMESPACE/PROJECT")]
    InvalidRepoCoordinate { value: String },

    /// A coordinate names a nested namespace on a forge whose namespaces are a
    /// single segment. Refused where the rule belongs — the coordinate type is
    /// forge-neutral, so only the client knows its own forge cannot nest.
    #[error("{forge} has no nested namespaces, but {namespace} is nested")]
    NestedNamespaceUnsupported { forge: String, namespace: String },

    /// A forge kind could not be derived from a host and none was given.
    ///
    /// Deliberately not a probe: guessing a forge kind from an unknown hostname
    /// and guessing wrong sends the announce credential to the wrong API in the
    /// wrong header. The publisher declares it instead.
    #[error(
        "cannot tell which forge {host} is; pass --forge github or --forge gitlab, or write the host out if {host} is a group name (gitlab.com/{host}/...)"
    )]
    ForgeKindUnknown { host: String },

    /// A fork was requested into the namespace that already owns the upstream.
    ///
    /// No forge can fork a repository into the namespace that owns it, so this
    /// would fail deep inside the fork API with an opaque status. The fork-free
    /// path is what this publisher wants.
    #[error(
        "{upstream} already lives under {namespace}, which cannot fork it: omit --fork to announce from a branch on the index repository itself"
    )]
    SelfForkRefused { upstream: String, namespace: String },

    /// `--fork` named a different host than `--index-repo`.
    ///
    /// A fork always lives on the same instance as the repository it forks, and
    /// the client is built for the index's host alone — so a differing fork host
    /// was silently ignored and the fork addressed on the index's instance
    /// instead, writing to a repository the operator did not name. Refused
    /// rather than reinterpreted.
    #[error(
        "--fork is on {fork_host} but --index-repo is on {index_host}; a fork lives on the same instance as its upstream"
    )]
    ForkHostMismatch { fork_host: String, index_host: String },

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
    ///
    /// `detail` carries the forge's own reason, which lives in the response body
    /// (`{"message": "..."}`) and nowhere else — a bare status code sends the
    /// reader to the forge's web UI to find out what a 422 meant. Build it with
    /// [`status_detail`] so the body is trimmed, length-capped, and reduced to
    /// the empty string when there is nothing worth showing.
    #[error("forge returned HTTP status {status} for {url}{detail}")]
    Status { url: String, status: u16, detail: String },

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

    /// A fork's own path is not in `namespace/project` form.
    #[error("fork path {full_path} is not in namespace/project form")]
    MalformedForkFullName { full_path: String },

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

    /// The credential cannot push a branch to the repository the fork-free
    /// announce path commits to.
    ///
    /// Raised by an up-front probe rather than by the first rejected write:
    /// GitHub answers an unauthorised write with 404 as readily as 403, and a
    /// 404 mid-sequence is indistinguishable from the fresh-fork provisioning
    /// race [`super::GitHubForge::commit_files`] retries for.
    #[error("no push access to {repo}: the announce credential is missing write (push) permission on that repository")]
    PushAccessDenied { repo: String },

    /// A fast-forward-only ref update was rejected — a concurrent announce
    /// advanced the branch (design register C4, compare-and-swap). The caller
    /// re-reads the new head, regenerates, and retries.
    #[error("ref update for branch {branch} is not a fast-forward")]
    NonFastForward { branch: String },

    /// A commit onto a fork exhausted its git-data retries on a 404 while its
    /// base commit lived in another repository — what a fork left behind
    /// upstream looks like from the git-data API, since the base object then
    /// reaches the fork only through the shared fork network.
    ///
    /// Replaces the bare [`Self::Status`] this used to surface, which named an
    /// endpoint and nothing else and so pointed every investigation at
    /// credentials, permissions, or the index repository — none of which are
    /// involved.
    #[error(
        "git write onto fork {fork} failed with 404: the base commit is not reachable there, which is what a fork behind upstream looks like — syncing {branch} from upstream reported: {sync}"
    )]
    ForkBaseUnreachable { fork: String, branch: String, sync: String },
}

/// How many characters of a forge's error body [`status_detail`] keeps.
///
/// Enough for a forge's own JSON error message, capped so a forge returning
/// something large — or hostile — cannot flood a CI log through an error path.
const STATUS_DETAIL_CAP: usize = 300;

/// Render a non-success response body as the `detail` of [`ForgeError::Status`].
///
/// Empty (not `"..."`, not `"<none>"`) when the body has nothing to say, so the
/// message degrades exactly to the bare `status for url` form it replaced and is
/// never *worse* than reporting the status alone. Truncation is on character
/// boundaries — a body cut mid-UTF-8 would panic on slicing.
///
/// `token` is the announce credential, and every occurrence of it is replaced
/// before anything else happens. The body is forge-controlled text: a reverse
/// proxy or a hostile self-hosted endpoint can echo a request header back in an
/// error body, and this value is rendered into `Display` and logged on the retry
/// path. The cap bounds the volume of such a leak but not its content, so the
/// secret is removed rather than merely shortened (design register X6). An empty
/// token (the `--out` path) redacts nothing.
#[must_use]
pub fn status_detail(body: &[u8], token: &str) -> String {
    let body = String::from_utf8_lossy(body);
    let body = if token.is_empty() {
        body
    } else {
        std::borrow::Cow::Owned(body.replace(token, "[redacted]"))
    };
    let body = body.trim();
    if body.is_empty() {
        return String::new();
    }
    match body.char_indices().nth(STATUS_DETAIL_CAP) {
        Some((end, _)) => format!(": {}... [truncated]", &body[..end]),
        None => format!(": {body}"),
    }
}

impl ClassifyExitCode for ForgeError {
    fn classify(&self) -> Option<ExitCode> {
        match self {
            // 401/403 — the bearer `OCX_ANNOUNCE_TOKEN` is missing, revoked,
            // or lacks scope. The fix is a credential, not a config file or a
            // retry (design register C13).
            Self::Status { status, .. } if *status == 401 || *status == 403 => Some(ExitCode::AuthError),
            // Same class as the 403 above, reached by a probe instead of a
            // rejected write: the fix is a credential with more permission.
            Self::PushAccessDenied { .. } => Some(ExitCode::AuthError),
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
            // A malformed invocation, not a failure of the run: the operator
            // named a self-hosted host without saying which forge runs there,
            // asked GitHub for a nested namespace it cannot express, or pointed
            // `--fork` at the namespace that already owns the index. Each is
            // fixed by editing the command line, which is what `EX_USAGE` means
            // — and a CI wrapper must be able to tell "your flags are wrong"
            // from "the forge said no".
            Self::ForgeKindUnknown { .. }
            | Self::NestedNamespaceUnsupported { .. }
            | Self::SelfForkRefused { .. }
            | Self::ForkHostMismatch { .. }
            | Self::InvalidRepoCoordinate { .. } => Some(ExitCode::UsageError),
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
            detail: String::new(),
        };
        assert_eq!(error.classify(), Some(ExitCode::AuthError));
    }

    /// The credential can come back inside a forge's own error body — a reverse
    /// proxy echoing the request headers is the ordinary way — and `detail` is
    /// rendered into `Display` and logged on the retry path. The cap bounds how
    /// much of such a body is kept, not whether the secret is in it.
    #[test]
    fn status_detail_redacts_the_token() {
        let token = "glpat-notarealtokenvalue";
        let body = format!(r#"{{"message":"bad PRIVATE-TOKEN: {token}"}}"#);
        let detail = status_detail(body.as_bytes(), token);
        assert!(!detail.contains(token), "the credential survived redaction: {detail}");
        assert!(detail.contains("[redacted]"), "expected a redaction marker in {detail}");
        // The surrounding diagnosis must survive — redaction that eats the
        // message leaves the operator with nothing to act on.
        assert!(detail.contains("bad PRIVATE-TOKEN"), "the message was lost: {detail}");
    }

    /// The falsifying half: with no token to match, the same body is untouched.
    /// Without this the redaction assertion above could pass on a function that
    /// blanks every body.
    #[test]
    fn status_detail_keeps_a_body_that_holds_no_token() {
        let detail = status_detail(br#"{"message":"404 Project Not Found"}"#, "glpat-notarealtokenvalue");
        assert_eq!(detail, r#": {"message":"404 Project Not Found"}"#);
        assert!(!detail.contains("[redacted]"));
    }

    #[test]
    fn a_malformed_invocation_maps_to_usage_error() {
        for error in [
            ForgeError::ForgeKindUnknown {
                host: "git.example.com".to_string(),
            },
            ForgeError::NestedNamespaceUnsupported {
                forge: "GitHub".to_string(),
                namespace: "acme/platform".to_string(),
            },
            ForgeError::SelfForkRefused {
                upstream: "acme/index".to_string(),
                namespace: "acme".to_string(),
            },
            ForgeError::ForkHostMismatch {
                fork_host: "gitlab.com".to_string(),
                index_host: "gitlab.example.com".to_string(),
            },
            ForgeError::InvalidRepoCoordinate {
                value: "gitlab.com@evil.example/acme/index".to_string(),
            },
        ] {
            assert_eq!(
                error.classify(),
                Some(ExitCode::UsageError),
                "{error} must exit 64 — it is fixed by editing the command line"
            );
        }
    }

    #[test]
    fn status_403_maps_to_auth_error() {
        let error = ForgeError::Status {
            url: "https://api.github.com/user".to_string(),
            status: 403,
            detail: String::new(),
        };
        assert_eq!(error.classify(), Some(ExitCode::AuthError));
    }

    #[test]
    fn status_429_maps_to_temp_fail() {
        let error = ForgeError::Status {
            url: "https://api.github.com/repos/x/y/git/blobs".to_string(),
            status: 429,
            detail: String::new(),
        };
        assert_eq!(error.classify(), Some(ExitCode::TempFail));
    }

    #[test]
    fn server_error_statuses_map_to_unavailable() {
        for status in [500, 502, 503, 599] {
            let error = ForgeError::Status {
                url: "https://api.github.com/repos/x/y/forks".to_string(),
                status,
                detail: String::new(),
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
                detail: String::new(),
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
