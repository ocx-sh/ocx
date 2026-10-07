// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Classifying what a rejected `git push` said into a named [`super::ForgeError`], which
//! decides a published exit code.

use super::{CapabilityName, CheckStatus, ForgeError, PushAccess, Redacted};

/// What a recognised phrase resolves to *before* the preflight is consulted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Refusal {
    /// The branch moved; the caller re-fetches and retries (exit 75).
    NonFastForward,
    /// A leased force-push lost its lease (exit 75).
    StaleLease,
    /// The server refused a write the credential may not make:
    /// [`ForgeError::WriteCapabilityUnavailable`] (82) under an `unknown`
    /// `job-token-push` preflight, [`ForgeError::PushRefused`] (77) otherwise.
    PermissionOrCapability,
    /// Any other `pre-receive` refusal: always [`ForgeError::PushRefused`] (77).
    HookDeclined,
}

/// Where a needle has to appear for the row to match.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MatchScope {
    /// Anywhere in stdout or stderr, as a case-sensitive substring.
    Body,
    /// On a trimmed stderr line beginning `remote: `, so a phrase echoed by a branch name or
    /// banner does not match; git's own phrases and the 403 texts never carry that prefix.
    RemoteLine,
}

/// Every recorded refusal shape, **in precedence order**, with the scope its needle is
/// matched under.
///
/// Mirrors `OBSERVED_REFUSAL_STDERR` in `test/tests/git_http_fixture.py`; both sides assert
/// the arity, so a new row needs a fixture shape too.
// A job-token refusal also carries `(pre-receive hook declined)`, so moving that row up makes
// the 82 promotion unreachable.
// `not allowed to push` stays a substring so it also matches GitLab's protected-branch wording.
const REFUSAL_NEEDLES: [(&str, MatchScope, Refusal); 6] = [
    ("(stale info)", MatchScope::Body, Refusal::StaleLease),
    ("(fetch first)", MatchScope::Body, Refusal::NonFastForward),
    (
        "The requested URL returned error: 403",
        MatchScope::Body,
        Refusal::PermissionOrCapability,
    ),
    (
        "RPC failed; HTTP 403",
        MatchScope::Body,
        Refusal::PermissionOrCapability,
    ),
    (
        "not allowed to push",
        MatchScope::RemoteLine,
        Refusal::PermissionOrCapability,
    ),
    ("(pre-receive hook declined)", MatchScope::Body, Refusal::HookDeclined),
];

/// The `reason` [`ForgeError::PushRefused`] carries for a generic hook decline.
// A fixed string, never a body slice: `PushRefused` takes a bare `String`, so a slice would
// escape `Redacted` and could leak a secret.
const HOOK_DECLINED_REASON: &str = "pre-receive hook declined";

/// The `reason` [`ForgeError::PushRefused`] carries for a promotable phrase the preflight
/// had already read as a plain permission refusal.
const PERMISSION_REASON: &str = "the credential may not push to this project";

/// The `remedy` the promoted [`ForgeError::WriteCapabilityUnavailable`] carries.
// States what was observed, not a setting to change: the field was unreadable, so the
// setting may not exist on that instance.
const CAPABILITY_REMEDY: &str = "field unreadable (GitLab < 18.4 or hidden); push refused";

/// Name the [`ForgeError`] a rejected `git push` deserves.
///
/// `stderr` must be **uncapped**: the phrases sit at its tail, so a capped text turns every
/// recognised refusal into exit 1.
#[must_use]
pub fn classify_push_failure(
    status: String,
    stdout: &Redacted,
    stderr: Redacted,
    preflight: &PushAccess,
    branch: &str,
    repo: &str,
    remote: &str,
) -> ForgeError {
    // Credential rejection first: a push refused at `git-receive-pack` matches no row below
    // and would exit 1 instead of 80.
    if let Some(rejected) = classify_remote_failure(&stderr, remote, GitInvocation::Push) {
        return rejected;
    }

    let refusal = REFUSAL_NEEDLES
        .iter()
        .find(|&&(needle, scope, _)| match scope {
            // Both streams: `--porcelain` writes `(fetch first)` and `(stale info)` to stdout,
            // so reading stderr alone exits a non-fast-forward as 1.
            MatchScope::Body => stdout.as_str().contains(needle) || stderr.as_str().contains(needle),
            MatchScope::RemoteLine => stderr
                .as_str()
                .lines()
                .map(str::trim)
                .any(|line| line.starts_with("remote: ") && line.contains(needle)),
        })
        .map(|&(_, _, refusal)| refusal);

    let Some(refusal) = refusal else {
        return ForgeError::GitPushFailed { status, stderr };
    };
    let branch = branch.to_string();
    match refusal {
        Refusal::StaleLease => ForgeError::StaleLease { branch },
        Refusal::NonFastForward => ForgeError::NonFastForward { branch },
        Refusal::HookDeclined => ForgeError::PushRefused {
            branch,
            reason: HOOK_DECLINED_REASON.to_string(),
        },
        // Promote only on `== Unknown`, or a plain permission refusal blames a healthy instance.
        // Never `!= Passed`: `Skipped`, also returned for an absent row, means no job token.
        Refusal::PermissionOrCapability => {
            if preflight.status(CapabilityName::JobTokenPush) == CheckStatus::Unknown {
                ForgeError::WriteCapabilityUnavailable {
                    capability: CapabilityName::JobTokenPush,
                    repo: repo.to_string(),
                    remedy: CAPABILITY_REMEDY.to_string(),
                }
            } else {
                ForgeError::PushRefused {
                    branch,
                    reason: PERMISSION_REASON.to_string(),
                }
            }
        }
    }
}

/// Which invocations a recorded rejection is decisive for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RejectionScope {
    /// Decisive on any invocation: authentication itself was refused.
    EveryInvocation,
    /// Decisive on a fetch alone; a push's own classifier owns this text.
    FetchOnly,
}

/// Which git invocation's stderr is being read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitInvocation {
    /// `git push`.
    Push,
    /// Every other invocation; in practice the `fetch`.
    Fetch,
}

impl RejectionScope {
    /// Whether a row of this scope decides the outcome for `invocation`.
    fn covers(self, invocation: GitInvocation) -> bool {
        self == Self::EveryInvocation || invocation == GitInvocation::Fetch
    }
}

/// Every recorded shape of a **credential** refusal: needle, HTTP status, decisive scope.
///
/// Measured on git 2.54.0 under `LC_ALL=C`, `GIT_TERMINAL_PROMPT=0`, identical on push.
// Needles are phrases git prints, never a bare status number: git's output never carries `401`.
const CREDENTIAL_REJECTIONS: [(&str, u16, RejectionScope); 3] = [
    ("could not read Username for ", 401, RejectionScope::EveryInvocation),
    ("Authentication failed for ", 401, RejectionScope::EveryInvocation),
    ("The requested URL returned error: 403", 403, RejectionScope::FetchOnly),
];

/// The fixed `detail` a git-side credential rejection carries into
/// [`ForgeError::Status`].
// Opens with `": "` because the variant's format string appends it straight onto the URL.
// A fixed sentence, never a body slice: git's stderr can carry a secret inside forge bytes.
const CREDENTIAL_REJECTED_DETAIL: &str = ": the git remote rejected the credential";

/// The HTTP status a rejected credential earned on `invocation`, or `None`.
///
/// Safe on any invocation's stderr, since local plumbing cannot print these phrases; below
/// the git that honours `GIT_NO_LAZY_FETCH`, a `write-tree` lazy fetch can still match.
#[must_use]
pub fn credential_rejection_status(stderr: &Redacted, invocation: GitInvocation) -> Option<u16> {
    CREDENTIAL_REJECTIONS
        .iter()
        .find(|(needle, _, scope)| scope.covers(invocation) && stderr.as_str().contains(needle))
        .map(|&(_, status, _)| status)
}

/// Name a rejected credential as the [`ForgeError::Status`] 401 or 403 it was, so it exits 80.
///
/// `remote` is named in the error, so it must not carry userinfo.
#[must_use]
pub fn classify_remote_failure(stderr: &Redacted, remote: &str, invocation: GitInvocation) -> Option<ForgeError> {
    credential_rejection_status(stderr, invocation).map(|status| ForgeError::Status {
        url: remote.to_string(),
        status,
        detail: CREDENTIAL_REJECTED_DETAIL.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::git_command::redact;
    use crate::forge::{CapabilityName, CheckStatus};

    const BRANCH: &str = "ocx/claim/acme";
    const REPO: &str = "acme/index";

    /// Where a recorded phrase's text actually came from — the annotation C-044
    /// requires on every row, kept here rather than flattened away.
    ///
    /// Two of the six are real evidence about git's wording. The other four are
    /// written by the fixture itself (its own `pre-receive` hook, or its 403
    /// answers), so those rows prove this classifier's **wiring** and not the
    /// phrase, and stay unproved against a real forge until release gate 4. The
    /// distinction is carried into every failure message below; it is
    /// deliberately not turned into a count, because a count of "how many rows
    /// are real evidence" is a number this test would be writing for itself.
    #[derive(Clone, Copy, Debug)]
    enum Source {
        /// Emitted by the local `git` client, recorded against git 2.54.0.
        /// Gettext-translated, which is what `LC_ALL=C` is protecting.
        GitClient,
        /// Written by the fixture — its hook line, or its 403 answer. Proves
        /// the wiring, not the phrase.
        Fixture,
    }

    /// The outcome a case expects, at the granularity the variant carries.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Outcome {
        NonFastForward,
        StaleLease,
        Refused,
        Capability,
        Unrecognised,
        /// The credential never authenticated: `ForgeError::Status`, carrying
        /// the status the forge answered with. `error.rs`'s table is what turns
        /// it into exit 80, which is why the status is the payload and the exit
        /// code is not asserted here.
        Rejected(u16),
    }

    fn outcome_of(error: &ForgeError) -> Outcome {
        match error {
            ForgeError::NonFastForward { .. } => Outcome::NonFastForward,
            ForgeError::StaleLease { .. } => Outcome::StaleLease,
            ForgeError::PushRefused { .. } => Outcome::Refused,
            ForgeError::WriteCapabilityUnavailable { .. } => Outcome::Capability,
            ForgeError::GitPushFailed { .. } => Outcome::Unrecognised,
            ForgeError::Status { status, .. } => Outcome::Rejected(*status),
            other => panic!("the classifier may only produce the six contracted variants, got {other:?}"),
        }
    }

    /// A preflight whose `job-token-push` row reads `status`.
    fn preflight(status: CheckStatus) -> PushAccess {
        let mut access = PushAccess::skipped_all();
        access.record(CapabilityName::JobTokenPush, status, None);
        access
    }

    /// The only route from a `&str` to a [`Redacted`] a sibling module has:
    /// `redact` with no secrets. A struct literal here is `E0423`.
    fn redacted(text: &str) -> Redacted {
        redact(text, &[])
    }

    /// Classify `text` with a `job-token-push` row reading `status`.
    fn classify(text: &str, status: CheckStatus) -> ForgeError {
        classify_push_failure(
            STATUS.to_string(),
            &redacted(""),
            redacted(text),
            &preflight(status),
            BRANCH,
            REPO,
            REMOTE,
        )
    }

    /// git's own frame around a rejection line, in the shape git 2.54.0 prints
    /// it: a server banner first, the reject line in the middle, `error:` last.
    /// The phrases sit at the **tail**, which is the whole reason a head cap
    /// applied before classification would destroy them.
    fn push_stderr(line: &str) -> String {
        format!(
            "remote: Resolving deltas: 100% (3/3), done.\n\
             remote: \n\
             remote: View merge request for {BRANCH}:\n\
             remote:   http://127.0.0.1:41337/{REPO}/-/merge_requests/42\n\
             To http://127.0.0.1:41337/{REPO}.git\n\
             {line}\n\
             error: failed to push some refs to 'http://127.0.0.1:41337/{REPO}.git'\n"
        )
    }

    /// Every recorded refusal shape maps to its named variant, driven from the
    /// production table so the two cannot drift.
    ///
    /// The arity assertion is the load-bearing half: a seventh recorded shape in
    /// `OBSERVED_REFUSAL_STDERR` forces a seventh needle here or this reds,
    /// which is what stops a new refusal wording from landing silently on the
    /// fallback. The pairing is positional, so the table's **order** — which is
    /// its precedence — is pinned too. `recorded.contains(needle)` ties each
    /// needle back to the phrase the fixture actually observed, so a needle
    /// cannot be widened into something the producer never recorded.
    ///
    /// Run under an `unknown` preflight, the one status that separates the
    /// promotable set from the generic hook decline.
    ///
    /// Reds on: deleting or reordering a needle, changing a needle's text,
    /// changing a row's `Refusal`, or adding a seventh row.
    #[test]
    fn classifier_maps_each_phrase() {
        let expected = [
            (
                "(stale info)",
                "(stale info)",
                Source::GitClient,
                " ! [rejected]        HEAD -> ocx/claim/acme (stale info)",
                Outcome::StaleLease,
            ),
            (
                "(fetch first)",
                "(fetch first)",
                Source::GitClient,
                " ! [rejected]        HEAD -> ocx/claim/acme (fetch first)",
                Outcome::NonFastForward,
            ),
            (
                "The requested URL returned error: 403",
                "The requested URL returned error: 403",
                Source::Fixture,
                "fatal: unable to access 'http://127.0.0.1:41337/acme/index.git/': \
                 The requested URL returned error: 403",
                Outcome::Capability,
            ),
            (
                "RPC failed; HTTP 403",
                "RPC failed; HTTP 403",
                Source::Fixture,
                "error: RPC failed; HTTP 403",
                Outcome::Capability,
            ),
            (
                "not allowed to push",
                "You are not allowed to push code to this project.",
                Source::Fixture,
                "remote: GL-HOOK-ERR: You are not allowed to push code to this project.",
                Outcome::Capability,
            ),
            (
                "(pre-receive hook declined)",
                "(pre-receive hook declined)",
                Source::GitClient,
                " ! [remote rejected] HEAD -> ocx/claim/acme (pre-receive hook declined)",
                Outcome::Refused,
            ),
        ];

        assert_eq!(
            REFUSAL_NEEDLES.len(),
            expected.len(),
            "a recorded refusal shape has no needle, or a needle has no recorded shape; the table is {REFUSAL_NEEDLES:?}"
        );
        for (index, (needle, recorded, source, line, outcome)) in expected.into_iter().enumerate() {
            assert_eq!(
                REFUSAL_NEEDLES[index].0, needle,
                "row {index} moved or was rewritten; declaration order IS precedence here"
            );
            assert!(
                recorded.contains(needle),
                "row {index}'s needle is not drawn from the phrase the fixture recorded \
                 (needle {needle:?}, recorded {recorded:?}, source {source:?})"
            );
            let error = classify(&push_stderr(line), CheckStatus::Unknown);
            assert_eq!(
                outcome_of(&error),
                outcome,
                "row {index} ({needle:?}, source {source:?}) classified as {error:?}"
            );
        }
    }

    /// The promotion fires on `CheckStatus::Unknown` and on no other status.
    ///
    /// All three statuses against all three promotable phrases — nine cells,
    /// because the natural reading of the contract, `!= Passed`, is wrong for
    /// exactly one of them. `Skipped` says the push credential is not a job
    /// token, which is a *stronger* statement than `Unknown`, and it is also
    /// what `PushAccess::status` returns for a row that is absent, so promoting
    /// it would tell an operator whose instance is fine that their instance is
    /// too old.
    ///
    /// Reds on: rewriting the predicate as `!= CheckStatus::Passed` (the three
    /// `Skipped` cells flip to 82), as `== CheckStatus::Passed`, or dropping the
    /// status read entirely.
    #[test]
    fn promotion_fires_only_on_unknown_preflight() {
        let promotable = [
            "fatal: unable to access 'http://127.0.0.1:41337/acme/index.git/': \
             The requested URL returned error: 403",
            "error: RPC failed; HTTP 403",
            "remote: GL-HOOK-ERR: You are not allowed to push code to this project.",
        ];
        for line in promotable {
            for status in CheckStatus::ALL {
                let expected = if status == CheckStatus::Unknown {
                    Outcome::Capability
                } else {
                    Outcome::Refused
                };
                let error = classify(&push_stderr(line), status);
                assert_eq!(
                    outcome_of(&error),
                    expected,
                    "job-token-push {status} against {line:?} classified as {error:?}"
                );
            }
        }
    }

    /// S-018: the preflight read the capability and it passed, so the very same
    /// refusal text is an ordinary permission refusal.
    ///
    /// Named separately from the nine-cell walk above because it is the
    /// scenario, not a cell: it is what "the promotion is driven by the
    /// preflight, never by the phrase" means when an operator reads it. It also
    /// pins the branch and the classifier-owned `reason`.
    ///
    /// Reds on: promoting on the phrase alone.
    #[test]
    fn passed_preflight_with_same_line_is_77() {
        let line = "remote: GL-HOOK-ERR: You are not allowed to push code to this project.";
        match classify(&push_stderr(line), CheckStatus::Passed) {
            ForgeError::PushRefused { branch, reason } => {
                assert_eq!(branch, BRANCH, "the refusal names the branch the caller pushed");
                assert_eq!(
                    reason, PERMISSION_REASON,
                    "a promotable phrase under a read preflight is a permission refusal"
                );
            }
            other => panic!("a passed preflight must not promote; got {other:?}"),
        }
    }

    /// An unrecognised refusal reaches the operator verbatim rather than being
    /// forced into a category it does not belong to.
    ///
    /// The payload is the **whole** redacted text, unaltered: the one variant
    /// that promises to pass a server's own words through is also the one that
    /// cannot pass a secret through with them, so nothing here re-derives or
    /// re-slices it. Its exit code (`None` → 1) is `error.rs`'s own table's
    /// assertion, not this file's — a second copy here would be a second
    /// definition of the same contract.
    ///
    /// Reds on: matching an empty needle (every body then classifies), or
    /// rebuilding the payload from anything but the input.
    #[test]
    fn unrecognised_stderr_is_exit_1_redacted() {
        let text = push_stderr("error: remote unpack failed: unable to create temporary object directory");
        match classify(&text, CheckStatus::Unknown) {
            ForgeError::GitPushFailed { status, stderr } => {
                assert_eq!(
                    stderr.as_str(),
                    text,
                    "the fallback passes the redacted text through unchanged"
                );
                assert_eq!(
                    status, STATUS,
                    "…and names the exit status, so an unclassified failure is diagnosable at all"
                );
            }
            other => panic!("an unmodelled refusal must fall through; got {other:?}"),
        }
    }

    /// git's own three phrases are decided before the preflight is consulted, so
    /// the status cannot change them.
    ///
    /// The cell that matters is `(fetch first)` under `unknown`: a classifier
    /// that read the status before matching the phrase would turn every
    /// retryable race on an older GitLab into a terminal 82 and kill the retry
    /// convergence. `(pre-receive hook declined)` alone stays 77 under
    /// `unknown` for the same reason — the promotion is gated on the promotable
    /// set, not on "any refusal".
    ///
    /// Reds on: hoisting the status check above the phrase match, or promoting
    /// on any refusal under `unknown`.
    #[test]
    fn non_promotable_phrases_ignore_the_preflight() {
        let cases = [
            (
                " ! [rejected]        HEAD -> ocx/claim/acme (fetch first)",
                Outcome::NonFastForward,
            ),
            (
                " ! [rejected]        HEAD -> ocx/claim/acme (stale info)",
                Outcome::StaleLease,
            ),
            (
                " ! [remote rejected] HEAD -> ocx/claim/acme (pre-receive hook declined)",
                Outcome::Refused,
            ),
        ];
        for (line, expected) in cases {
            for status in CheckStatus::ALL {
                let error = classify(&push_stderr(line), status);
                assert_eq!(
                    outcome_of(&error),
                    expected,
                    "{line:?} under job-token-push {status} classified as {error:?}"
                );
                match &error {
                    ForgeError::NonFastForward { branch }
                    | ForgeError::StaleLease { branch }
                    | ForgeError::PushRefused { branch, .. } => {
                        assert_eq!(branch, BRANCH, "the outcome names the branch the caller pushed");
                    }
                    other => panic!("unexpected variant {other:?}"),
                }
                if let ForgeError::PushRefused { reason, .. } = &error {
                    assert_eq!(reason, HOOK_DECLINED_REASON, "the hook decline's reason is a constant");
                }
            }
        }
    }

    /// The recorded multi-phrase body: a job-token refusal carries the
    /// promotable line **and** `(pre-receive hook declined)` at once.
    ///
    /// This is the pair the fixture records, not a body invented here, and it is
    /// the case that decides whether the 82 promotion is reachable in production
    /// at all. An earlier draft of this design shipped the opposite order and
    /// the promotion could never fire.
    ///
    /// Reds on: swapping the promotable rows below the generic hook-decline row.
    /// Every single-phrase case stays green under that swap, which is why this
    /// case exists.
    #[test]
    fn a_promotable_phrase_outranks_the_generic_hook_decline() {
        let body = push_stderr(
            "remote: GL-HOOK-ERR: You are not allowed to push code to this project.\n\
              ! [remote rejected] HEAD -> ocx/claim/acme (pre-receive hook declined)",
        );
        assert_eq!(
            outcome_of(&classify(&body, CheckStatus::Unknown)),
            Outcome::Capability,
            "the combined body must promote, or the 82 arm is dead code in production"
        );
        assert_eq!(
            outcome_of(&classify(&body, CheckStatus::Passed)),
            Outcome::Refused,
            "and under a read preflight the same combined body is an ordinary refusal"
        );
    }

    /// `(stale info)` is matched before `(fetch first)`.
    ///
    /// Both map to exit 75, so an exit-code assertion could not tell the arms
    /// apart — and the retry paths differ: a stale lease rebuilds the lease, a
    /// non-fast-forward re-fetches. `(stale info)` is reachable only under
    /// `--force-with-lease` and is therefore the more specific signal.
    ///
    /// Reds on: swapping the first two table rows. An exit-code assertion would
    /// stay green under that swap, which is why every assertion in this file is
    /// on the variant.
    #[test]
    fn a_stale_lease_outranks_a_non_fast_forward() {
        let body = push_stderr(
            " ! [rejected]        HEAD -> ocx/claim/acme (stale info)\n\
              ! [rejected]        HEAD -> ocx/claim/acme (fetch first)",
        );
        assert_eq!(
            outcome_of(&classify(&body, CheckStatus::Passed)),
            Outcome::StaleLease,
            "a body carrying both reject reasons is the leased one"
        );
    }

    /// One row, two needles, one outcome — including the recorded body that
    /// carries both 403 texts at once.
    ///
    /// The receive-pack arm's stderr contains the `info/refs` arm's phrase
    /// verbatim (the fixture asserts that asymmetry), so the two are only ever
    /// separable one way. Giving them different outcomes would make the combined
    /// body classify one way and the plain `info/refs` body the other.
    ///
    /// Reds on: giving `RPC failed; HTTP 403` an outcome of its own, or dropping
    /// either needle.
    #[test]
    fn both_403_texts_classify_alike_including_the_combined_body() {
        let bodies = [
            "fatal: unable to access 'http://127.0.0.1:41337/acme/index.git/': \
             The requested URL returned error: 403",
            "error: RPC failed; HTTP 403",
            "error: RPC failed; HTTP 403 curl 22 The requested URL returned error: 403",
        ];
        for line in bodies {
            assert_eq!(
                outcome_of(&classify(&push_stderr(line), CheckStatus::Unknown)),
                Outcome::Capability,
                "{line:?} did not reach the promotable arm"
            );
            assert_eq!(
                outcome_of(&classify(&push_stderr(line), CheckStatus::Skipped)),
                Outcome::Refused,
                "{line:?} promoted on a status that is not unknown"
            );
        }
    }

    /// Bodies that look like a promotable refusal and are not.
    ///
    /// Every row is run under `unknown`, the status where promotion is armed, so
    /// a widened matcher shows up as an 82 an operator cannot act on. Each row
    /// names the widening it forbids:
    ///
    /// * a branch called `not-allowed-to-push` echoed in git's reject line —
    ///   forbids normalising separators before matching;
    /// * the spaced phrase on a line that is not a `remote:` line — forbids
    ///   dropping the line anchor, which is the whole narrowing against a
    ///   server-composed banner;
    /// * an object count containing `403` — forbids shortening either 403 needle
    ///   to the bare number;
    /// * the phrase in a different case — forbids a case-insensitive match,
    ///   which buys nothing and widens the two rows above;
    /// * `pre-receive hook declined` in prose without git's parentheses —
    ///   forbids the contract's bare spelling in place of the recorded one.
    ///
    /// Reds on: any of those five widenings.
    #[test]
    fn near_miss_bodies_do_not_promote() {
        let cases = [
            (
                " ! [remote rejected] HEAD -> not-allowed-to-push (pre-receive hook declined)",
                Outcome::Refused,
            ),
            (
                "hint: the pre-push guide explains why you are not allowed to push from a fork",
                Outcome::Unrecognised,
            ),
            (
                "remote: Writing objects: 100% (403/403), done.\n\
                 error: remote unpack failed: index-pack abnormal exit",
                Outcome::Unrecognised,
            ),
            (
                "remote: GL-HOOK-ERR: You Are Not Allowed To Push code to this project.",
                Outcome::Unrecognised,
            ),
            (
                "remote: GL-HOOK-ERR: the pre-receive hook declined to run at all",
                Outcome::Unrecognised,
            ),
        ];
        for (line, expected) in cases {
            let error = classify(&push_stderr(line), CheckStatus::Unknown);
            assert_eq!(outcome_of(&error), expected, "{line:?} classified as {error:?}");
        }
    }

    /// The `remote:` anchor is tested on the trimmed line, so neither a `\r` nor
    /// leading whitespace defeats it.
    ///
    /// The leading-whitespace half carries the reachable red. The CRLF half
    /// guards a matcher that compares whole lines rather than searching within
    /// one; git writes LF on every platform, so a CRLF body is a fixture
    /// artefact rather than a git one, and `trim()` removes the whole class for
    /// one call.
    ///
    /// Reds on: testing the prefix against the raw, untrimmed line.
    #[test]
    fn the_remote_line_anchor_survives_crlf_and_leading_whitespace() {
        let bodies = [
            "remote: GL-HOOK-ERR: You are not allowed to push code to this project.\r\n\
             error: failed to push some refs\r\n",
            "   remote: GL-HOOK-ERR: You are not allowed to push code to this project.\n\
             error: failed to push some refs\n",
        ];
        for body in bodies {
            assert_eq!(
                outcome_of(&classify(body, CheckStatus::Unknown)),
                Outcome::Capability,
                "the anchor rejected {body:?}"
            );
        }
    }

    /// A push that failed with nothing to say degrades to the bare form rather
    /// than to a category it did not earn.
    ///
    /// Whitespace-only and empty are one arm: the capturing helper hands over
    /// whatever git wrote, and "wrote nothing" and "was never captured" are
    /// indistinguishable here. The rendered message is asserted because
    /// `GitPushFailed`'s `Display` interpolates the payload — an operator gets
    /// `git push failed: ` and must not get anything worse.
    ///
    /// Reds on: treating an empty needle as a match (`str::contains("")` is true
    /// for every haystack), which classifies the empty body as a refusal.
    #[test]
    fn empty_and_whitespace_only_stderr_degrade_to_the_bare_form() {
        for text in ["", "   \n\t\n"] {
            match classify(text, CheckStatus::Unknown) {
                ForgeError::GitPushFailed { status, stderr } => {
                    assert_eq!(stderr.as_str(), text, "the empty body is passed through as it arrived");
                    let rendered = ForgeError::GitPushFailed { status, stderr }.to_string();
                    assert!(
                        rendered.starts_with("git push failed (") && rendered.contains(STATUS),
                        "an empty payload must still render the bare sentence, and it names the \
                         exit status so an unclassified failure is diagnosable, got {rendered:?}"
                    );
                }
                other => panic!("an empty body has no phrase to match; got {other:?}"),
            }
        }
    }

    /// The ref-status line on **stdout** classifies on its own, with stderr
    /// empty.
    ///
    /// This is the shipped shape after `--porcelain`: git writes the per-ref
    /// verdict to stdout as `!\t<refspec>\t[rejected] (fetch first)` and the
    /// prose frame to stderr. It is also the exact state CI was observed in —
    /// a rejected push whose stderr arrived empty on both Linux and macOS,
    /// where reading stderr alone made an ordinary concurrent-writer rejection
    /// an unclassified exit 1.
    ///
    /// RED: drop `stdout.as_str().contains(needle) ||` from the `Body` arm of
    /// the needle scan — the row falls through to `Unrecognised`, which is the
    /// defect this pair exists to keep out.
    #[test]
    fn a_porcelain_reject_on_stdout_classifies_with_an_empty_stderr() {
        let porcelain = "To file:///tmp/remote\n\
             !\trefs/heads/claim:refs/heads/claim\t[rejected] (fetch first)\n\
             Done\n";
        let classified = classify_push_failure(
            STATUS.to_string(),
            &redacted(porcelain),
            redacted(""),
            &preflight(CheckStatus::Unknown),
            BRANCH,
            REPO,
            REMOTE,
        );
        assert_eq!(
            outcome_of(&classified),
            Outcome::NonFastForward,
            "the verdict is on stdout under `--porcelain`; an empty stderr must not lose it, got {classified:?}"
        );

        // The stale-lease reason travels the same channel, so the two rows move
        // together or neither does.
        let leased = "To file:///tmp/remote\n\
             !\trefs/heads/claim:refs/heads/claim\t[rejected] (stale info)\n\
             Done\n";
        assert_eq!(
            outcome_of(&classify_push_failure(
                STATUS.to_string(),
                &redacted(leased),
                redacted(""),
                &preflight(CheckStatus::Unknown),
                BRANCH,
                REPO,
                REMOTE,
            )),
            Outcome::StaleLease,
            "a lease refusal is a stdout verdict too"
        );
    }

    /// Neither channel carrying a phrase still degrades to the fallback — and
    /// the fallback now names the exit status.
    ///
    /// The positive control for the row above: it proves the stdout arm reads
    /// the *needle* rather than answering `NonFastForward` for any non-empty
    /// stdout at all.
    #[test]
    fn a_porcelain_success_line_on_stdout_is_not_a_refusal() {
        let porcelain = "To file:///tmp/remote\n\
             \trefs/heads/claim:refs/heads/claim\t[up to date]\n\
             Done\n";
        assert_eq!(
            outcome_of(&classify_push_failure(
                STATUS.to_string(),
                &redacted(porcelain),
                redacted(""),
                &preflight(CheckStatus::Unknown),
                BRANCH,
                REPO,
                REMOTE,
            )),
            Outcome::Unrecognised,
            "a stdout body with no refusal phrase must not manufacture a verdict"
        );
    }

    /// A redaction marker landing mid-phrase degrades the run, and can never
    /// upgrade it.
    ///
    /// `redact` masks any non-empty secret and enforces no minimum length, so a
    /// push credential whose value happens to be `push` rewrites
    /// `not allowed to push code` into `not allowed to [redacted] code` and the
    /// promotable match is destroyed. That is **fail-safe, not immune**: the run
    /// lands on exit 1 with the diagnosis still legible, never on a wrong exit
    /// code, because `[redacted]` is a fixed literal containing no phrase and so
    /// can never *create* a match. Both halves are asserted here rather than the
    /// immunity being assumed. The minimum-length guard belongs in `redact`
    /// itself, which is another package's file.
    ///
    /// Reds on: stripping `[redacted]` markers before matching (the first half),
    /// or a redaction replacement text that contains a phrase (the second half
    /// is the standing argument for keeping the literal inert).
    #[test]
    fn a_redaction_landing_mid_phrase_degrades_and_never_promotes() {
        let shredded = redact(
            "remote: GL-HOOK-ERR: You are not allowed to push code to this project.",
            &["push"],
        );
        assert!(
            shredded.as_str().contains("[redacted]"),
            "the premise of this test is that the secret really landed inside the phrase, got {shredded}"
        );
        assert_eq!(
            outcome_of(&classify_push_failure(
                STATUS.to_string(),
                &redacted(""),
                shredded,
                &preflight(CheckStatus::Unknown),
                BRANCH,
                REPO,
                REMOTE
            )),
            Outcome::Unrecognised,
            "a shredded phrase must degrade to the fallback, never to a wrong exit code"
        );

        let masked = redact(
            "error: remote unpack failed: object write error",
            &["object write error"],
        );
        assert_eq!(
            outcome_of(&classify_push_failure(
                STATUS.to_string(),
                &redacted(""),
                masked,
                &preflight(CheckStatus::Unknown),
                BRANCH,
                REPO,
                REMOTE
            )),
            Outcome::Unrecognised,
            "the replacement literal must not be able to manufacture a match"
        );
    }

    /// The classifier reads the **whole** text, not a capped head of it.
    ///
    /// The in-tree capping precedent is a 300-character *head* cap; a push's
    /// phrases sit at the tail, behind a server banner that GitLab writes on
    /// every push. So a cap applied before the match turns every recognised
    /// refusal into exit 1 — and every other test in this file stays green,
    /// because their bodies are short. The first assertion is the control: it
    /// proves this body's head really does lose the phrase, so the second
    /// assertion is discriminating rather than decorative.
    ///
    /// Reds on: capping the text at the top of the classifier instead of on the
    /// payload placed in `GitPushFailed`.
    #[test]
    fn the_classifier_runs_on_uncapped_text() {
        // Mirrors `error.rs`'s `STATUS_DETAIL_CAP`, which is private to that module.
        const HEAD_CAP: usize = 300;
        let banner =
            "remote: View merge request for ocx/claim/acme: http://127.0.0.1:41337/acme/index/-/merge_requests/42\n"
                .repeat(4);
        let body = format!(
            "{banner}remote: GL-HOOK-ERR: You are not allowed to push code to this project.\n\
              ! [remote rejected] HEAD -> ocx/claim/acme (pre-receive hook declined)\n"
        );
        let head: String = body.chars().take(HEAD_CAP).collect();
        assert!(
            !head.contains("not allowed to push") && !head.contains("(pre-receive hook declined)"),
            "the control failed: a {HEAD_CAP}-character head of this body still carries a phrase, \
             so the case cannot discriminate a cap-first classifier"
        );
        assert_eq!(
            outcome_of(&classify(&body, CheckStatus::Unknown)),
            Outcome::Capability,
            "a long body classified as though its tail had been cut off"
        );
    }

    /// `PushRefused`'s `reason` is drawn from this file's own constants and is
    /// never sliced out of the body.
    ///
    /// The type guarantee `Redacted` buys covers `GitCommandFailed` and
    /// `GitPushFailed`; `PushRefused` takes a bare `String`, so the only thing
    /// keeping forge bytes out of it is the closed set. The body here carries a
    /// secret-shaped token that the redactor was never told about — exactly what
    /// a `credential.helper` on the operator's own box can leave in git's output
    /// — and none of it may reach the rendered message.
    ///
    /// Reds on: setting `reason` from `stderr.as_str()`, or from any slice of
    /// the matched line.
    #[test]
    fn push_refused_reason_is_classifier_owned_never_sliced_from_the_body() {
        let leak = "glpat-notarealvalue";
        let body = push_stderr(&format!(
            "remote: GL-HOOK-ERR: rejected for {leak}\n\
              ! [remote rejected] HEAD -> ocx/claim/acme (pre-receive hook declined)"
        ));
        match classify(&body, CheckStatus::Unknown) {
            ForgeError::PushRefused { branch, reason } => {
                assert_eq!(branch, BRANCH);
                assert_eq!(reason, HOOK_DECLINED_REASON, "the reason is a constant, not a slice");
                let rendered = ForgeError::PushRefused { branch, reason }.to_string();
                assert!(
                    !rendered.contains(leak) && !rendered.contains("GL-HOOK-ERR"),
                    "server bytes reached an unprotected field: {rendered:?}"
                );
            }
            other => panic!("a generic hook decline is a refusal; got {other:?}"),
        }
    }

    /// The promoted 82 names the row that was unreadable, the repository, and
    /// S-017's two-signal remedy.
    ///
    /// There are two different 82s and they carry two different remedies. The
    /// preflight's fires when the job-token field reads `false` and names
    /// Settings → CI/CD → Job token permissions, because ocx read the setting.
    /// This one fires when the field could not be read *and* the push was then
    /// refused, so it reports both observations instead of naming a setting that
    /// may not exist on that instance. Handing an operator the wrong one of the
    /// two sends them to a page they cannot use.
    ///
    /// Reds on: emitting C-029's Settings text here, or naming a capability
    /// other than `job-token-push`.
    #[test]
    fn the_capability_refusal_carries_s017_two_signal_remedy() {
        let line = "remote: GL-HOOK-ERR: You are not allowed to push code to this project.";
        match classify(&push_stderr(line), CheckStatus::Unknown) {
            ForgeError::WriteCapabilityUnavailable {
                capability,
                repo,
                remedy,
            } => {
                assert_eq!(
                    capability,
                    CapabilityName::JobTokenPush,
                    "the promotion is about the job-token-push row and no other"
                );
                assert_eq!(repo, REPO, "the refusal names the repository the push targeted");
                assert_eq!(remedy, CAPABILITY_REMEDY);
                assert!(
                    remedy.contains("unreadable") && remedy.contains("refused"),
                    "S-017's remedy states both signals; got {remedy:?}"
                );
                assert!(
                    !remedy.contains("Job token permissions"),
                    "this is not the preflight's 82 — its Settings remedy belongs to the readable-false path"
                );
            }
            other => panic!("an unknown preflight must promote; got {other:?}"),
        }
    }

    // ── The non-push credential rejection (exit 80) ──────────────────────────

    /// The remote every case below is opened against. Carries no credential,
    /// which is the property that makes it safe to name in an error.
    const REMOTE: &str = "https://gitlab.example/acme/index.git";

    /// The exit status the classifier's fallback carries, for the tests that do
    /// not care which one it was.
    const STATUS: &str = "exit status: 1";

    /// The three recorded rejection shapes, verbatim, as git 2.54.0 printed them
    /// against a loopback server answering `GET /info/refs`.
    ///
    /// Written out here rather than assembled from [`CREDENTIAL_REJECTIONS`]:
    /// an expectation built from the table it is checking agrees with any
    /// mistake in it. The needle is asserted to be a substring of the recording
    /// instead, which is the same discipline the six-row refusal walk applies.
    const RECORDED_REJECTIONS: [(&str, u16); 3] = [
        (
            "fatal: could not read Username for 'https://gitlab.example': terminal prompts disabled",
            401,
        ),
        (
            "remote: HTTP Basic: Access denied\n\
             fatal: Authentication failed for 'https://gitlab.example/acme/index.git/'",
            401,
        ),
        (
            "remote: HTTP Basic: Access denied\n\
             fatal: unable to access 'https://gitlab.example/acme/index.git/': \
             The requested URL returned error: 403",
            403,
        ),
    ];

    /// Every recorded rejection resolves to the status the forge answered with,
    /// driven from the production table so the two cannot drift.
    ///
    /// The arity assertion is the load-bearing half: a fourth recorded shape
    /// forces a fourth needle or this reds, which is what stops a new wording
    /// from silently landing back on the generic exit-1 path.
    ///
    /// Reds on: deleting any row from `CREDENTIAL_REJECTIONS`; widening a needle
    /// to something the recording does not contain; changing a row's status.
    #[test]
    fn every_recorded_credential_rejection_names_its_status() {
        assert_eq!(
            CREDENTIAL_REJECTIONS.len(),
            RECORDED_REJECTIONS.len(),
            "a recorded rejection has no needle, or a needle has no recording; the table is {CREDENTIAL_REJECTIONS:?}"
        );
        for (index, (recorded, status)) in RECORDED_REJECTIONS.into_iter().enumerate() {
            let (needle, table_status, _) = CREDENTIAL_REJECTIONS[index];
            assert!(
                recorded.contains(needle),
                "row {index}'s needle {needle:?} is not drawn from the phrase git printed: {recorded:?}"
            );
            assert_eq!(
                table_status, status,
                "row {index} pairs the needle with the wrong status"
            );
            assert_eq!(
                credential_rejection_status(&redacted(recorded), GitInvocation::Fetch),
                Some(status),
                "row {index} ({needle:?}) did not classify"
            );
        }
    }

    /// A rejected credential becomes the 401/403 the forge answered with, which
    /// is what the published claim table promises exit 80 for.
    ///
    /// Asserted on the variant and its fields rather than on an exit code: the
    /// mapping from 401/403 to `AuthError` is `error.rs`'s table's contract, and
    /// a second copy here would be a second definition of it.
    ///
    /// Reds on: returning `None` for a recorded shape (the caller then builds
    /// `GitCommandFailed`, which is unclassified and exits 1), or on naming
    /// anything but the remote in `url`.
    #[test]
    fn a_rejected_credential_is_the_forge_status_it_really_was() {
        for (recorded, expected) in RECORDED_REJECTIONS {
            match classify_remote_failure(&redacted(recorded), REMOTE, GitInvocation::Fetch) {
                Some(ForgeError::Status { url, status, detail }) => {
                    assert_eq!(status, expected, "{recorded:?} carried the wrong status");
                    assert_eq!(url, REMOTE, "the error names the remote the workspace was opened for");
                    assert_eq!(
                        detail, CREDENTIAL_REJECTED_DETAIL,
                        "the detail is a constant, not a slice"
                    );
                }
                other => panic!("{recorded:?} must classify as a forge status; got {other:?}"),
            }
        }
    }

    /// A plumbing failure that says nothing about a credential falls through, so
    /// the caller still builds its `GitCommandFailed`.
    ///
    /// The permissive half, and it is not optional: a classifier that answered
    /// `Some` for everything would pass every assertion above while turning
    /// every unreadable object into exit 80.
    ///
    /// The last two rows are the adjacent texts most likely to be swept up by a
    /// widened needle — a bare `403` inside an ordinary progress line, and a
    /// server banner that merely mentions authentication.
    ///
    /// Reds on: matching a bare `401`/`403`; matching `Authentication` alone;
    /// matching an empty needle.
    #[test]
    fn an_ordinary_plumbing_failure_does_not_look_like_a_rejected_credential() {
        let cases = [
            "fatal: not a valid object name: refs/heads/claim",
            "error: unable to create temporary object directory",
            "remote: Enumerating objects: 403, done.",
            "remote: Authentication succeeded for this project.",
            "",
        ];
        for text in cases {
            assert_eq!(
                credential_rejection_status(&redacted(text), GitInvocation::Fetch),
                None,
                "{text:?} must not read as a rejected credential"
            );
            assert!(
                classify_remote_failure(&redacted(text), REMOTE, GitInvocation::Fetch).is_none(),
                "{text:?} must not become a forge status"
            );
        }
    }

    /// A 403 on a push still reaches the documented 77/82 verdict rather than
    /// the 80 the same text earns on a fetch.
    ///
    /// The two tables share the `403` needle **and a push now does consult the
    /// credential table first**, so this is the assertion holding the whole
    /// scope column up: nothing but [`RejectionScope::FetchOnly`] stops that row
    /// from hijacking the capability verdict. The call-graph argument that used
    /// to protect it — "a push never reaches [`classify_remote_failure`]" — was
    /// retired when the push started routing through it, which is exactly the
    /// kind of silent load transfer a test written against the old reason would
    /// have missed.
    ///
    /// Reds on: widening the 403 row to [`RejectionScope::EveryInvocation`], or
    /// moving the 403 needle out of [`REFUSAL_NEEDLES`] into the credential
    /// table.
    #[test]
    fn a_403_on_a_push_keeps_its_documented_verdict() {
        let line = "fatal: unable to access 'http://127.0.0.1:41337/acme/index.git/': \
                    The requested URL returned error: 403";
        assert_eq!(
            outcome_of(&classify(&push_stderr(line), CheckStatus::Unknown)),
            Outcome::Capability,
            "the push arm's 403 is a capability verdict, not an authentication one"
        );
        assert_eq!(
            outcome_of(&classify(&push_stderr(line), CheckStatus::Passed)),
            Outcome::Refused,
            "and a read preflight makes it a permission refusal, still not an authentication one"
        );
    }

    /// **The defect this file exists to close.** A credential the forge rejects
    /// at `git-receive-pack` is the 401 it really was, on the push path exactly
    /// as on the fetch.
    ///
    /// Production only ever meets this shape. The index project is public, so
    /// GitLab serves its `upload-pack` to anyone: a run whose credential is
    /// wrong fetches successfully, builds its commit, and is refused at the
    /// push. Classifying the fetch alone therefore fixed the shape that never
    /// fires and left the one that always does falling through to
    /// [`ForgeError::GitPushFailed`] — unclassified, exit 1, against a claim
    /// table promising 80.
    ///
    /// Driven through the **push** frame ([`push_stderr`]) rather than a bare
    /// line, because the phrase git prints sits at the tail behind GitLab's
    /// banner, and through both preflight states, because the credential
    /// question is answered before the preflight is ever read.
    ///
    /// **Reds on** (both measured end to end against the acceptance fixture,
    /// which reproduces the original exit 1 verbatim): scoping the
    /// `could not read Username for ` row to [`RejectionScope::FetchOnly`], or
    /// making [`RejectionScope::covers`] answer `false` for
    /// [`GitInvocation::Push`].
    ///
    /// The obvious third mutation — deleting the [`classify_remote_failure`]
    /// call from [`classify_push_failure`] — **cannot** red anything, and that
    /// is worth knowing rather than rediscovering: it leaves
    /// [`GitInvocation::Push`] constructed nowhere, so `-D dead-code` fails the
    /// build before a test runs. A build that does not compile is not a red
    /// test, and mutating the data is what discriminates here.
    #[test]
    fn a_credential_rejected_at_the_push_is_the_forge_status_it_really_was() {
        let recorded = [
            "fatal: could not read Username for 'http://127.0.0.1:41337': terminal prompts disabled",
            "remote: HTTP Basic: Access denied\n\
             fatal: Authentication failed for 'http://127.0.0.1:41337/acme/index.git/'",
        ];
        for line in recorded {
            for status in [CheckStatus::Unknown, CheckStatus::Passed] {
                assert_eq!(
                    outcome_of(&classify(&push_stderr(line), status)),
                    Outcome::Rejected(401),
                    "{line:?} under a {status:?} preflight must read as authentication refused"
                );
            }
        }
    }
}
