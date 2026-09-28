// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Error taxonomy for the forge REST client.

use super::{CapabilityName, ForgeKind, Redacted, WriteTransport};

/// Failures raised by the forge client; no variant carries the token.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ForgeError {
    /// A repository coordinate string is not in `[HOST/]NAMESPACE/PROJECT` form.
    #[error("invalid repository coordinate {value}, expected [HOST/]NAMESPACE/PROJECT")]
    InvalidRepoCoordinate { value: String },

    /// A nested namespace on a forge whose namespaces are a single segment.
    #[error("{forge} has no nested namespaces, but {namespace} is nested")]
    NestedNamespaceUnsupported { forge: String, namespace: String },

    /// A forge kind could not be derived from a host and none was given.
    ///
    /// Never probed: a wrong guess sends the announce credential to the wrong API.
    #[error(
        "cannot tell which forge {host} is; pass --forge github or --forge gitlab, or write the host out if {host} is a group name (gitlab.com/{host}/...)"
    )]
    ForgeKindUnknown { host: String },

    /// A fork was requested into the namespace that already owns the upstream, which no forge can do.
    #[error(
        "{upstream} already lives under {namespace}, which cannot fork it: omit --fork to announce from a branch on the index repository itself"
    )]
    SelfForkRefused { upstream: String, namespace: String },

    /// `--fork` named a different host than `--index-repo`.
    ///
    /// Refused, never reinterpreted: the client addresses the index's host, so the
    /// fork would resolve to a repository the operator did not name.
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

    /// The forge answered with a non-success HTTP status; build `detail` with [`status_detail`].
    ///
    /// The git transport also raises this for a rejected credential, with a fixed
    /// `detail`: the stderr it holds can carry the credential itself.
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

    /// A fork's parent is not the upstream (a same-named stranger); refused before any write.
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

    /// A verified fork is not owned by the requested owner.
    #[error("fork owner {actual} does not match the requested owner {expected}")]
    ForkOwnerMismatch { expected: String, actual: String },

    /// A fork did not become ready within the bounded readiness deadline.
    #[error("fork not ready within {deadline_secs}s")]
    ForkNotReady { deadline_secs: u64 },

    /// A compare response carried a `status` value the client does not model.
    /// Never guessed: read as "not ahead" it strands a committed announce with no pull request.
    #[error("forge compare {url} returned an unmodelled status {status}")]
    UnknownCompareStatus { url: String, status: String },

    /// The credential cannot push to the repository the fork-free announce path commits to.
    ///
    /// Raised by an up-front probe, not the first rejected write: GitHub answers that
    /// write with 404, indistinguishable mid-sequence from the fresh-fork race
    /// [`super::GitHubForge::commit_files`] retries for.
    #[error("no push access to {repo}: the announce credential is missing write (push) permission on that repository")]
    PushAccessDenied { repo: String },

    /// A fast-forward-only ref update was rejected because a concurrent announce
    /// advanced the branch; the caller re-reads the head, regenerates, and retries.
    #[error("ref update for branch {branch} is not a fast-forward")]
    NonFastForward { branch: String },

    /// A commit onto a fork 404ed through its git-data retries while its base commit
    /// lived in another repository, which is what a fork behind upstream looks like.
    #[error(
        "git write onto fork {fork} failed with 404: the base commit is not reachable there, which is what a fork behind upstream looks like — syncing {branch} from upstream reported: {sync}"
    )]
    ForkBaseUnreachable { fork: String, branch: String, sync: String },

    /// A write transport this forge cannot serve, refused by
    /// [`super::ForgeKind::validate_transport`] before any network call.
    #[error("the {transport} write transport is not supported on {forge}; drop --transport to write over the API")]
    TransportUnsupported {
        forge: ForgeKind,
        transport: WriteTransport,
    },

    /// One operation the selected transport cannot perform, such as a fork operation
    /// over git; [`Self::TransportUnsupported`] refuses a whole transport.
    #[error("{operation} is not available over the {transport} write transport")]
    TransportOperationUnsupported {
        operation: String,
        transport: WriteTransport,
    },

    /// The credential may not call the forge's users API at all (a GitLab CI job token).
    ///
    /// Not "account not found": no lookup is possible, so the owner must be given as `LOGIN:ID`.
    #[error(
        "the forge users API is not reachable with this credential; give the owner as LOGIN:ID so no lookup is needed"
    )]
    UsersApiUnavailable,

    /// `git` is absent, unusable, or older than the git transport's floor; raised before any network call.
    #[error("the git write transport cannot run: {reason}")]
    GitUnavailable { reason: String },

    /// A rendered merge-request push option carried a value the git wire forbids.
    ///
    /// `reason` must never echo the value: it is operator-influenced, and printing it
    /// re-injects it into the CI log.
    /// Unclassified (exit 1) like [`Self::GitCommandFailed`]: no flag reaches this value, so
    /// [`ExitCode::UsageError`](ocx_exit::ExitCode::UsageError) would wrongly blame the command line.
    #[error("the push option {key} carries a value the git wire forbids: {reason}")]
    PushOptionRefused { key: &'static str, reason: String },

    /// A `git` plumbing step failed for a reason nothing models.
    ///
    /// `stderr` stays [`Redacted`], never `String`: git's stderr can carry a secret
    /// inside forge-controlled bytes, and `redact` is the only way to build one.
    #[error("git {command} failed with {status}: {stderr}")]
    GitCommandFailed {
        command: String,
        status: String,
        stderr: Redacted,
    },

    /// `git push` failed for a reason the stderr classifier does not recognise.
    ///
    /// The server's words pass through verbatim, so `stderr` stays [`Redacted`].
    #[error("git push failed ({status}): {stderr}")]
    GitPushFailed { status: String, stderr: Redacted },

    /// A leased force-push was refused because the branch moved since it was read;
    /// the git counterpart of [`Self::NonFastForward`].
    #[error("the branch {branch} moved since it was read, so the leased force-push was refused")]
    StaleLease { branch: String },

    /// The server refused the push for a reason that is not a capability gate
    /// (a protected branch or a pre-receive hook).
    #[error("the push to {branch} was refused by the server: {reason}")]
    PushRefused { branch: String, reason: String },

    /// A capability the selected write transport needs is disabled on the project
    /// or unavailable on the instance.
    ///
    /// Only an administrator can fix it, hence its own exit code. `remedy` names the
    /// setting, and both projects when the check compared two.
    #[error("{capability} is unavailable on {repo}: {remedy}")]
    WriteCapabilityUnavailable {
        capability: CapabilityName,
        repo: String,
        remedy: String,
    },

    /// The push succeeded but no merge request appeared within the confirmation
    /// bound; a rerun picks up a late one without duplicating it.
    #[error(
        "the push succeeded but no merge request appeared within {deadline_secs}s; rerun the command to pick up one the server created late"
    )]
    MergeRequestUnconfirmed { deadline_secs: u64 },
}

/// Whether `status` is a forge-side fault (5xx) rather than a refusal.
///
/// The one spelling for the binary's exit classification and
/// `git_workspace::confirm_merge_request`, so they cannot disagree on which statuses mean the forge broke.
#[must_use]
pub fn is_server_fault(status: u16) -> bool {
    (500..=599).contains(&status)
}

/// Characters of a forge error body [`status_detail`] keeps, so a large or hostile body cannot flood a CI log.
const STATUS_DETAIL_CAP: usize = 300;

/// Render a non-success response body as the `detail` of [`ForgeError::Status`], empty when the body is.
///
/// `token` is replaced first: a forge-controlled body can echo a request header
/// back, and the cap bounds the volume of that leak, not its content.
/// Truncates on character boundaries; a byte slice mid-UTF-8 panics.
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

#[cfg(test)]
mod tests {
    use super::*;
    use ocx_exit::ExitCode;
    // The redactor is not re-exported from `forge` — only the type it produces
    // is — so the fixture reaches it by module path. That it has to is the
    // point: this is the only construction site either `stderr`-bearing variant
    // has, and it goes through the masking like every later one will.
    use super::super::git_command::redact;

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

    // The exit-code table over these same eleven rows is
    // `ocx::exit::ocx_announce::tests::forge_error_exit_code_table`, beside the
    // `ClassifyExitCode` impl it pins. The fixture stays here because
    // `new_error_messages_follow_style` reads the same rows.

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
