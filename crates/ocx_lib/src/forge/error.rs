// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Error taxonomy for the forge REST client.

use super::{CapabilityName, ForgeKind, Redacted, WriteTransport};
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
    ///
    /// **The REST client is not the only producer.** `git` is an HTTP client
    /// too, so a credential the forge rejects over the git write transport is
    /// the same 401/403 arriving through a different reader — see
    /// `git_stderr::credential_rejection_status`, which recovers the status from
    /// what `git` printed. That producer supplies a fixed `detail` of its own
    /// rather than a response body it never sees, because the bytes it *does*
    /// hold are the ones a credential can arrive inside.
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

    /// A write transport this forge cannot serve was selected.
    ///
    /// Pure and up front: refused by [`super::ForgeKind::validate_transport`]
    /// before any client is built and before any network call, so the operator
    /// learns it from the command line rather than from a failed write.
    #[error("the {transport} write transport is not supported on {forge}; drop --transport to write over the API")]
    TransportUnsupported {
        forge: ForgeKind,
        transport: WriteTransport,
    },

    /// An operation the selected transport cannot perform was reached.
    ///
    /// Distinct from [`Self::TransportUnsupported`], which refuses the transport
    /// as a whole: here the transport is legitimate and one operation on it is
    /// not. The fork operations under the git transport are the live case — the
    /// credential that transport exists for cannot reach the fork API at all.
    #[error("{operation} is not available over the {transport} write transport")]
    TransportOperationUnsupported {
        operation: String,
        transport: WriteTransport,
    },

    /// The credential may not call the forge's users API at all.
    ///
    /// A GitLab CI job token reads repository content but no user identities.
    /// Deliberately **not** the same answer as an account that does not exist:
    /// there is no lookup to be had, so a bare `LOGIN` can never become an id
    /// and the operator must write the pair out.
    #[error(
        "the forge users API is not reachable with this credential; give the owner as LOGIN:ID so no lookup is needed"
    )]
    UsersApiUnavailable,

    /// `git` is absent, unusable, or older than the floor the git write
    /// transport needs.
    ///
    /// Raised before any network call, so a run that cannot possibly succeed
    /// costs nothing and reaches no forge.
    #[error("the git write transport cannot run: {reason}")]
    GitUnavailable { reason: String },

    /// A rendered merge-request push option carried a value the git wire must
    /// never be handed, and the push was refused before any process was spawned.
    ///
    /// Raised by the push-option renderer alone, so it is a *pre-spawn* refusal:
    /// nothing reached a pkt-line, no ref moved, and no forge was contacted.
    ///
    /// **`reason` must never echo the offending value.** The guard fires exactly
    /// when C-067's structured-values-only rule has already been broken upstream,
    /// so at that moment the value is operator-influenced text — putting it in a
    /// message re-injects it into the CI log, which for an ESC byte is a second,
    /// actively hostile sink. Naming the key and the single offending codepoint
    /// is diagnosis; reproducing the value is a leak.
    ///
    /// Deliberately **unclassified** (`classify` → `None` → exit 1), like
    /// [`Self::GitCommandFailed`] and [`Self::GitPushFailed`]: under C-067 no
    /// operator flag reaches this value, so a fire means an ocx-side invariant
    /// broke. That is an internal error with no remedy a caller can branch on,
    /// and emphatically not [`ExitCode::UsageError`] — telling an operator to fix
    /// their command line would be a lie.
    #[error("the push option {key} carries a value the git wire forbids: {reason}")]
    PushOptionRefused { key: &'static str, reason: String },

    /// A `git` plumbing step failed for a reason nothing models.
    ///
    /// `stderr` is [`Redacted`], not `String`, and that is the whole guarantee:
    /// `redact` is its only constructor, so the masking cannot be forgotten at
    /// a construction site the way a doc comment asking for it can. It matters
    /// here more than anywhere else in this taxonomy because `git`'s stderr is
    /// the one channel where a secret arrives *inside* forge-controlled bytes —
    /// a credential helper or a server echoing a request header back — rather
    /// than beside them. Length-capping stays the capturing helper's, which
    /// bounds the volume; the type bounds the content.
    #[error("git {command} failed with {status}: {stderr}")]
    GitCommandFailed {
        command: String,
        status: String,
        stderr: Redacted,
    },

    /// `git push` failed for a reason the stderr classifier does not recognise.
    ///
    /// The catch-all under the recognised refusals below, so an unmodelled
    /// server message reaches the operator verbatim rather than being forced
    /// into a category it does not belong to. "Verbatim" is exactly why the
    /// type matters: `stderr` is [`Redacted`] and capped as for
    /// [`Self::GitCommandFailed`], so the one variant that promises to pass a
    /// server's own words through is also the one that cannot pass a secret
    /// through with them.
    #[error("git push failed ({status}): {stderr}")]
    GitPushFailed { status: String, stderr: Redacted },

    /// A leased force-push was refused because the branch moved since it was
    /// read.
    ///
    /// The git transport's spelling of the compare-and-swap
    /// [`Self::NonFastForward`] expresses over REST: a concurrent run advanced
    /// the branch between the fetch and the push, so the rebuild is regenerated
    /// against the winning head rather than overwriting it.
    #[error("the branch {branch} moved since it was read, so the leased force-push was refused")]
    StaleLease { branch: String },

    /// The server refused the push for a reason that is not a capability gate —
    /// a protected branch, or a pre-receive hook.
    #[error("the push to {branch} was refused by the server: {reason}")]
    PushRefused { branch: String, reason: String },

    /// A capability the selected write transport needs is disabled on the
    /// project or unavailable on the instance.
    ///
    /// The remedy is never in the caller's hands — an administrator changes a
    /// project setting, or the instance is upgraded — which is what separates
    /// this from a caller-side policy refusal and why it carries its own exit
    /// code. `remedy` names the setting to change, and where the check compared
    /// two projects it names both, since neither path alone tells an operator
    /// which one to edit.
    #[error("{capability} is unavailable on {repo}: {remedy}")]
    WriteCapabilityUnavailable {
        capability: CapabilityName,
        repo: String,
        remedy: String,
    },

    /// The push succeeded but no merge request appeared within the confirmation
    /// bound.
    ///
    /// The server creates a push-option merge request asynchronously, so a slow
    /// instance outruns the poll while the branch is already published. Rerunning
    /// picks up a request that arrived late; nothing is lost and nothing is
    /// duplicated.
    #[error(
        "the push succeeded but no merge request appeared within {deadline_secs}s; rerun the command to pick up one the server created late"
    )]
    MergeRequestUnconfirmed { deadline_secs: u64 },
}

/// Whether `status` is a forge-side fault — a 5xx the forge itself answered
/// with, as opposed to a refusal of the request.
///
/// One range literal with two readers, deliberately. [`ForgeError::classify`]
/// maps it to [`ExitCode::Unavailable`], which tells a CI wrapper the forge is
/// down and the run never happened. The merge-request confirmation poll
/// (`git_workspace::confirm_merge_request`) needs the *same* predicate to reach
/// the opposite conclusion: there, the push has already landed, so a fault is
/// folded into "not yet" and the poll's own
/// [`ForgeError::MergeRequestUnconfirmed`] is the answer. Two spellings of
/// `500..=599` would be two rules, free to drift into disagreeing about which
/// statuses mean the forge broke.
#[must_use]
pub(super) fn is_server_fault(status: u16) -> bool {
    (500..=599).contains(&status)
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
            Self::Status { status, .. } if is_server_fault(*status) => Some(ExitCode::Unavailable),
            // A request never completed (connect, TLS, timeout, DNS, or read
            // failure) — the forge itself is unreachable.
            Self::Transport { .. } => Some(ExitCode::Unavailable),
            // A persistent non-fast-forward (heavy branch contention the one
            // in-announce retry did not clear) is a transient failure the
            // caller may retry (design register C4). A stale lease is the git
            // transport's spelling of the same race, and an unconfirmed merge
            // request is a server that was simply slower than the poll — all
            // three are answered by running the command again.
            Self::NonFastForward { .. } | Self::StaleLease { .. } | Self::MergeRequestUnconfirmed { .. } => {
                Some(ExitCode::TempFail)
            }
            // `git` is missing or too old. Nothing about the invocation is
            // wrong and no credential is involved: the host lacks a tool the
            // run needs, which is exactly what `EX_UNAVAILABLE` means. Raised
            // before any network call.
            Self::GitUnavailable { .. } => Some(ExitCode::Unavailable),
            // A protected branch or a pre-receive hook said no. The credential
            // authenticated fine and the request was well-formed; the caller
            // simply may not write there — the same reading `EX_NOPERM` carries
            // for a filesystem `EPERM`.
            Self::PushRefused { .. } => Some(ExitCode::PermissionDenied),
            // A capability the transport needs is disabled on the project or
            // absent from the instance. Deliberately not 80 (the credential is
            // valid), not 81 (no local policy refused anything) and not 69 (the
            // forge is up and answering): the remedy is an administrator
            // changing a setting, which is a state a pipeline must be able to
            // tell apart from all three.
            Self::WriteCapabilityUnavailable { .. } => Some(ExitCode::ForgeCapabilityUnavailable),
            // A malformed invocation, not a failure of the run: the operator
            // named a self-hosted host without saying which forge runs there,
            // asked GitHub for a nested namespace it cannot express, or pointed
            // `--fork` at the namespace that already owns the index. Each is
            // fixed by editing the command line, which is what `EX_USAGE` means
            // — and a CI wrapper must be able to tell "your flags are wrong"
            // from "the forge said no".
            //
            // The three transport-shaped refusals join them for the same
            // reason. `TransportUnsupported` and `TransportOperationUnsupported`
            // are both fixed by changing `--transport` or dropping `--fork`, and
            // `UsersApiUnavailable` is fixed by writing the owner as `LOGIN:ID`
            // — none of them is a failure of the forge or of the credential.
            Self::ForgeKindUnknown { .. }
            | Self::NestedNamespaceUnsupported { .. }
            | Self::SelfForkRefused { .. }
            | Self::ForkHostMismatch { .. }
            | Self::InvalidRepoCoordinate { .. }
            | Self::TransportUnsupported { .. }
            | Self::TransportOperationUnsupported { .. }
            | Self::UsersApiUnavailable => Some(ExitCode::UsageError),
            // Every other status code and every other variant is not yet
            // classified beyond the sysexits default (`ExitCode::Failure`).
            // `GitCommandFailed` and `GitPushFailed` land here **deliberately**:
            // each is an unrecognised failure of a plumbing step, with no remedy
            // a caller could branch on, so inventing a code for either would be
            // worse than exit 1 with git's own message attached.
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // The redactor is not re-exported from `forge` — only the type it produces
    // is — so the fixture reaches it by module path. That it has to is the
    // point: this is the only construction site either `stderr`-bearing variant
    // has, and it goes through the masking like every later one will.
    use super::super::git_command::redact;

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

    /// The eleven variants the git write transport adds — C-018's ten, plus
    /// [`ForgeError::PushOptionRefused`] (DX-27) — each beside the exit code the
    /// ADR's exit-code table names for it.
    ///
    /// One fixture, read by two tests. The exit-code table and the message
    /// style guard must cover the same eleven variants, and a second
    /// hand-written list is a second definition of "the eleven", free to drift
    /// from this one.
    fn new_variants_with_exit_codes() -> Vec<(ForgeError, Option<ExitCode>)> {
        vec![
            (
                ForgeError::TransportUnsupported {
                    forge: ForgeKind::GitHub,
                    transport: WriteTransport::Git,
                },
                Some(ExitCode::UsageError),
            ),
            (
                ForgeError::TransportOperationUnsupported {
                    operation: "ensure_fork".to_string(),
                    transport: WriteTransport::Git,
                },
                Some(ExitCode::UsageError),
            ),
            (ForgeError::UsersApiUnavailable, Some(ExitCode::UsageError)),
            (
                ForgeError::GitUnavailable {
                    reason: "git 2.30.9 is below the 2.31 floor the git write transport needs".to_string(),
                },
                Some(ExitCode::Unavailable),
            ),
            (
                ForgeError::GitCommandFailed {
                    command: "fetch".to_string(),
                    status: "exit status 128".to_string(),
                    stderr: redact("fatal: could not read from the remote repository", &[]),
                },
                None,
            ),
            (
                ForgeError::GitPushFailed {
                    status: "exit status: 1".to_string(),
                    stderr: redact("remote: a refusal shape no classifier models", &[]),
                },
                None,
            ),
            // DX-27. Unclassified for the same reason as the two above, and
            // asserted as `None` rather than omitted so that a later hand
            // dropping it into the `UsageError` arm — the tempting wrong answer,
            // because the message reads like bad input — reds here instead of
            // shipping. The `reason` names the key and the codepoint and never
            // the value; see the variant's own doc comment.
            (
                ForgeError::PushOptionRefused {
                    key: "merge_request.title",
                    reason: "byte 12 is U+001B, outside the printable ASCII the wire carries".to_string(),
                },
                None,
            ),
            (
                ForgeError::StaleLease {
                    branch: "indexbot-claim-acme".to_string(),
                },
                Some(ExitCode::TempFail),
            ),
            (
                ForgeError::PushRefused {
                    branch: "main".to_string(),
                    reason: "the branch is protected".to_string(),
                },
                Some(ExitCode::PermissionDenied),
            ),
            (
                ForgeError::WriteCapabilityUnavailable {
                    capability: CapabilityName::JobTokenPush,
                    repo: "acme/index".to_string(),
                    remedy: "enable Settings > CI/CD > Job token permissions on acme/index".to_string(),
                },
                Some(ExitCode::ForgeCapabilityUnavailable),
            ),
            (
                ForgeError::MergeRequestUnconfirmed { deadline_secs: 30 },
                Some(ExitCode::TempFail),
            ),
        ]
    }

    /// Every one of the git write transport's eleven new variants against the
    /// ADR's exit-code table.
    ///
    /// [`ForgeError::GitCommandFailed`], [`ForgeError::GitPushFailed`] and
    /// [`ForgeError::PushOptionRefused`] are **deliberately unclassified** and
    /// are asserted as `None` rather than omitted: each is an unrecognised
    /// failure of a plumbing step or a broken ocx-side invariant, with no remedy
    /// a caller could branch on, so a later accidental classification must red
    /// here rather than ship silently.
    ///
    /// The two worth reading twice are 86 and 77.
    /// [`ForgeError::WriteCapabilityUnavailable`] is exit 86, a *capability
    /// gate* whose remedy is an administrator's; [`ForgeError::PushRefused`] is
    /// exit 77, a *permission refusal* against a credential that authenticated
    /// fine. Collapsing them would hide the one state a pipeline cannot act on
    /// from the one it can.
    ///
    /// The arity is asserted over the very array the rows are read from — a
    /// count written beside a separate enumeration is a budget, not a pairing,
    /// and would not notice a dropped row.
    ///
    /// Reds on: dropping any row (proved), and on moving a variant to a
    /// different arm of `classify` (proved with `PushRefused` 77 -> 86).
    #[test]
    fn forge_error_exit_code_table() {
        let table = new_variants_with_exit_codes();
        assert_eq!(
            table.len(),
            11,
            "the git write transport adds eleven variants and every one of them is classified here, including the three unclassified"
        );
        for (error, expected) in table {
            assert_eq!(error.classify(), expected, "{error} must classify as {expected:?}");
        }
    }

    /// The style this repository already holds library error messages to, from
    /// the Rust API Guidelines' `C-GOOD-ERR` and applied the same way by
    /// `oci::sign::error`'s `sign_error_kind_display_rules`: a concise
    /// lowercase message with no trailing punctuation, acronyms **and proper
    /// nouns** keeping their canonical case, and no redundant `error:` prefix
    /// (the `Error` trait already categorises the line).
    ///
    /// Read from the same fixture as the exit-code table so the two cannot
    /// cover different sets of eleven.
    ///
    /// **This test is deliberately stricter than the rule it enforces, and the
    /// gap is in the proper nouns.** [`is_sentence_case`] readmits an
    /// all-uppercase acronym and nothing else, so the very messages the cited
    /// precedent asserts as *compliant* — `"Fulcio rejected the CSR as
    /// malformed"`, `"Rekor transparency log unavailable"` — would red here.
    /// None of the eleven variants below opens with one today, which is why the
    /// narrower predicate is green.
    ///
    /// The direction is the safe one: a false red, which an author sees and
    /// must answer, rather than a false green, which ships. But the answer is
    /// **not** to relax the assertion or drop the first-word check — either
    /// silences the whole style guard to admit one word. Widen
    /// [`is_sentence_case`] instead, with an explicit allowlist of the proper
    /// nouns this taxonomy is permitted to open with (`GitHub`, `GitLab`,
    /// `Fulcio`, `Rekor`), so that admitting a new one stays a decision someone
    /// wrote down.
    ///
    /// Reds on: sentence-casing any of the eleven `#[error(...)]` strings, or
    /// giving one a trailing period.
    #[test]
    fn new_error_messages_follow_style() {
        let table = new_variants_with_exit_codes();
        assert_eq!(
            table.len(),
            11,
            "the style rule covers all eleven of the git write transport's new variants"
        );
        for (error, _) in table {
            let message = error.to_string();
            assert!(!message.is_empty(), "an error variant rendered nothing");
            assert!(
                !message.ends_with(['.', '!', '?']),
                "C-GOOD-ERR wants no trailing punctuation: {message}"
            );
            let first_word = message.split_whitespace().next().unwrap_or_default();
            assert!(
                !is_sentence_case(first_word),
                "C-GOOD-ERR wants a lowercase first word (an all-caps acronym is fine): {message}"
            );
            assert!(
                !message.to_ascii_lowercase().starts_with("error:"),
                "`Error` already categorises the line, so the prefix is redundant: {message}"
            );
        }
    }

    /// Whether `word` is Sentence-case: an initial capital followed by at least
    /// one lowercase letter. An all-uppercase word (`HTTP`, `CI`, `I/O`) is an
    /// acronym and keeps its canonical case, which the guideline allows.
    ///
    /// A proper noun (`GitLab`, `Fulcio`) is Sentence-case by this predicate and
    /// so reads as a violation, even though the guideline permits it. That is a
    /// deliberate narrowing, not an oversight — see the caller for why, and for
    /// the allowlist that is the sanctioned way to widen it.
    fn is_sentence_case(word: &str) -> bool {
        let mut characters = word.chars();
        characters.next().is_some_and(char::is_uppercase) && characters.any(char::is_lowercase)
    }
}
