// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Classifying what a rejected `git push` said into a named [`super::ForgeError`].
//!
//! Pure — no I/O, no clock, no environment — but **not a function of the text
//! alone**: four of the five outcomes carry data stderr does not hold (the
//! branch, the repository, the capability, the remedy), so the preflight and
//! both names are parameters. Worth reading alone because it decides a published
//! exit code.
//!
//! The one rule a later reader most needs: an HTTP 403 or a `remote:` line
//! containing "not allowed to push" is promoted to a capability refusal **only
//! when the write preflight reported `job-token-push` as
//! [`super::CheckStatus::Unknown`], and on no other status**. The promotion is
//! driven by what was checked, never by the phrase — the same text with the
//! preflight reporting `passed` is an ordinary permission refusal, and
//! conflating the two would report "your instance is too old" at an operator
//! whose instance is fine. [`super::CheckStatus::Skipped`] lands on 77 with
//! `passed`: it says the row does not apply to this run — the push credential is
//! not a job token — which is a *stronger* statement than `unknown`, and it is
//! also what [`super::PushAccess::status`] returns for an absent row, so the
//! exact `== Unknown` predicate makes that fallback safe by construction and no
//! defensive arm is written for it.
//!
//! Three further facts the table encodes, each of which a reader of the contract
//! prose alone would get wrong:
//!
//! * **Precedence is mandatory, not defensive.** A job-token refusal carries
//!   *both* the promotable line and `(pre-receive hook declined)` in one body —
//!   the fixture records that pair — so a declaration-order table that put the
//!   generic hook decline first would make the promotion unreachable in
//!   production. The order is `(stale info)`, `(fetch first)`, the promotable
//!   set, the generic hook decline, then the fallback.
//! * **An HTTP 403 is two texts**, not one. The `info/refs` arm and the
//!   receive-pack arm print different client-side strings sharing no substring
//!   beyond `403`; both are needles, both resolve alike, and neither is ever
//!   shortened to the bare number, which appears in ordinary object counts.
//! * **The `remote: ` prefix anchors one needle only.** `send-pack` adds it to
//!   every hook-written line, so it is the whole narrowing against a phrase
//!   echoed from a branch name or a server banner — but git's own three phrases
//!   and both 403 texts never carry it, so requiring it everywhere would break
//!   four of six.
//!
//! Two ordering facts that live outside this file but decide whether it works:
//!
//! * The classifier runs on the **uncapped** redacted text. A cap applied before
//!   the match silently destroys the table's inputs — the in-tree precedent is a
//!   300-character *head* cap while these phrases sit at the tail, behind the
//!   server's `remote:` banner — and turns every recognised refusal into exit 1.
//!   Any cap belongs on the payload placed in [`super::ForgeError::GitPushFailed`].
//! * `LC_ALL=C` protects **three** of the six phrases, not all six.
//!   `(fetch first)`, `(stale info)` and `(pre-receive hook declined)` are git's
//!   own and are gettext-translated; the `info/refs` 403 is libcurl's and the
//!   receive-pack 403 is remote-curl's, neither translated, and the
//!   not-allowed line is written by the server and never touched by the client's
//!   locale. The promotable arm is therefore locale-independent on both its 403
//!   legs.

use super::{CapabilityName, CheckStatus, ForgeError, PushAccess, Redacted};

/// What a recognised phrase resolves to *before* the preflight is consulted.
///
/// Separated from [`ForgeError`] because three of the four outcomes need a
/// branch, a repository or a remedy the table does not hold, and because one of
/// them — [`Self::PermissionOrCapability`] — is not an outcome at all until the
/// preflight has been read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Refusal {
    /// The branch moved; the caller re-fetches and retries (exit 75).
    NonFastForward,
    /// A leased force-push lost its lease (exit 75). Listed *before*
    /// [`Self::NonFastForward`] because it is reachable only under
    /// `--force-with-lease` and is therefore strictly the more specific signal,
    /// and because the two share an exit code and can only be told apart by the
    /// variant.
    StaleLease,
    /// The server refused a write the credential may not make. **Promotable**:
    /// [`ForgeError::WriteCapabilityUnavailable`] (86) under an `unknown`
    /// `job-token-push` preflight, [`ForgeError::PushRefused`] (77) otherwise.
    PermissionOrCapability,
    /// A hook said no for a reason that is not a capability gate — a protected
    /// branch, or any other `pre-receive` refusal. Always
    /// [`ForgeError::PushRefused`] (77); there is no "protected-branch phrase"
    /// to look for, because GitLab's protected-branch line *contains* "not
    /// allowed to push" and is therefore a member of the promotable set rather
    /// than a discriminator against it.
    HookDeclined,
}

/// Where in the stderr a needle has to appear for the row to match.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MatchScope {
    /// Anywhere in the body, as a case-sensitive substring.
    Body,
    /// On a line that, once trimmed, begins `remote: ` — the prefix `send-pack`
    /// adds to every line a server-side hook wrote.
    RemoteLine,
}

/// Every recorded refusal shape, **in precedence order**, with the scope its
/// needle is matched under.
///
/// The six rows mirror `RefusalShape` / `OBSERVED_REFUSAL_STDERR` in
/// `test/tests/git_http_fixture.py`, whose own test drives all six against a
/// real `git` and asserts each published phrase appears verbatim in the stderr
/// that shape produced. That is the producer; this is the consumer, and the
/// arity is asserted on both sides so a seventh recorded shape cannot land
/// silently on the fallback.
///
/// Five needles are the recorded phrase verbatim. The sixth is the case-
/// sensitive substring `not allowed to push` of
/// `You are not allowed to push code to this project.`, deliberately shorter
/// than the recording: GitLab's protected-branch wording is
/// `You are not allowed to push code to protected branches on this project.`,
/// and the substring is what covers both without a second row.
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
///
/// A classifier-owned constant, never a slice of the body:
/// [`ForgeError::PushRefused`] takes a bare `String`, so the guarantee
/// [`Redacted`] buys does not extend to it, and the only safe closure is one
/// this file writes. Same discipline [`super::CapabilityCheck`]'s `detail`
/// states for itself.
const HOOK_DECLINED_REASON: &str = "pre-receive hook declined";

/// The `reason` [`ForgeError::PushRefused`] carries when a promotable phrase was
/// seen but the preflight had already read the capability — the credential is
/// simply not allowed to write here.
const PERMISSION_REASON: &str = "the credential may not push to this project";

/// The `remedy` the promoted [`ForgeError::WriteCapabilityUnavailable`] carries.
///
/// **S-017's two-signal wording, and deliberately not C-029's.** There are two
/// different 86s: the preflight raises one when the job-token field reads
/// `false`, and its remedy names Settings → CI/CD → Job token permissions,
/// because ocx read the setting and knows which one to change. This one fires
/// when the field could not be read *and* the push was then refused — two
/// signals, neither conclusive alone — so the remedy has to say what was
/// observed rather than name a setting that may not exist on that instance.
const CAPABILITY_REMEDY: &str = "field unreadable (GitLab < 18.4 or hidden); push refused";

/// Name the [`ForgeError`] a rejected `git push` deserves.
///
/// `stderr` is taken **by value** and moved into
/// [`ForgeError::GitPushFailed`] on the fallback path: [`Redacted`] is not
/// `Clone` by design, and this file may not construct one — `redact` lives in
/// `git_command` and carries a `dead_code` expectation that a second production
/// caller would turn into a build error. The input arrives already redacted, so
/// there is nothing here to redact.
///
/// `preflight` is read for exactly one row, `job-token-push`, and only to decide
/// the promotion. `branch` and `repo` are the names the outcome variants carry;
/// neither is ever parsed out of the text. `remote` names the URL a rejected
/// credential is reported against.
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
    // **Authentication first, and it has to be first.** Every row below asks why
    // the server refused a write, which presumes the credential was let in at
    // all. A push the forge rejects at `git-receive-pack` never authenticated, so
    // it matches none of them and used to fall through to `GitPushFailed` —
    // unclassified, exit 1, against a claim table promising 80. The two sets are
    // disjoint in practice (a run that could not read a username reached no hook,
    // so no hook wrote a decline), which is why the order is safe as well as
    // necessary.
    if let Some(rejected) = classify_remote_failure(&stderr, remote, GitInvocation::Push) {
        return rejected;
    }

    // Declaration order IS precedence, and the scan runs on the whole text: a
    // job-token refusal carries a promotable line and `(pre-receive hook
    // declined)` in one body, so first-match-wins over this order is the only
    // thing that keeps the 86 arm reachable at all.
    let refusal = REFUSAL_NEEDLES
        .iter()
        .find(|&&(needle, scope, _)| match scope {
            // **Both channels, because `--porcelain` moves the ref-status line
            // onto stdout.** The reject reason (`(fetch first)`, `(stale info)`)
            // is written per-ref, and with `--porcelain` git writes that table
            // to stdout in a tab-separated form it documents for scripts, while
            // the prose `error:`/`hint:` frame stays on stderr. Reading only
            // stderr made the verdict depend on git's *prose*, which CI was
            // observed to emit as nothing at all — an empty body matches no
            // needle, so a plain non-fast-forward reached the operator as an
            // unclassified exit 1.
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
        // Exactly `== Unknown`, never `!= Passed`: `Skipped` says the push
        // credential is not a job token — a stronger statement than `unknown` —
        // and it is also what `PushAccess::status` returns for an absent row, so
        // the strict comparison is what makes that fallback safe.
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
///
/// The axis exists because **a 401 and a 403 do not mean the same thing on the
/// two paths**, and collapsing them was the defect. A 401 is authentication
/// refused outright — the credential never got in — which is exit 80 wherever it
/// happens. A 403 is *taken as* authenticated-and-then-forbidden, and on a push
/// that is the permission-or-capability verdict [`REFUSAL_NEEDLES`] already
/// resolves to 77 or 86 against the write preflight. On a fetch there is no
/// preflight and no capability alternative, so a 403 there is read as the
/// credential being refused.
///
/// **That reading is an assumption, not a proof.** git's stderr carries no
/// provenance for the status, so a 403 raised *before* the forge ever saw the
/// credential — by a WAF, a reverse proxy, or a repository policy — is
/// indistinguishable here from one raised after authentication succeeded. The
/// consequence is a wrong *reason*, never a wrong success: such a fetch reports
/// the credential refused (80) and such a push reports permission or capability
/// (77/86), when the true cause was an intermediary. Establishing the difference
/// needs positive evidence that the credential was accepted, which the git
/// subprocess boundary does not expose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RejectionScope {
    /// Decisive on any invocation: authentication itself was refused.
    EveryInvocation,
    /// Decisive on a fetch alone; a push's own classifier owns this text.
    FetchOnly,
}

/// Which git invocation's stderr is being read.
///
/// A caller-supplied fact rather than something inferred from the text: the same
/// bytes mean different things depending on which command produced them, and the
/// text cannot say which command that was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitInvocation {
    /// `git push`.
    Push,
    /// Every other invocation the workspace runs — in practice the `fetch`, the
    /// only other one that reaches the network.
    Fetch,
}

impl RejectionScope {
    /// Whether a row of this scope decides the outcome for `invocation`.
    fn covers(self, invocation: GitInvocation) -> bool {
        self == Self::EveryInvocation || invocation == GitInvocation::Fetch
    }
}

/// Every recorded shape of a **credential** refusal, paired with the HTTP status
/// the forge actually answered with and the invocations that status is decisive
/// for.
///
/// **One table, both paths.** The fetch and the push were classified separately
/// at first, and that shipped the defect this scope column removes: the
/// production index project is *public*, so GitLab serves its `upload-pack` to
/// anyone, and a run whose credential the forge rejects fetches successfully,
/// builds its commit, and is refused at `git-receive-pack`. Classifying the
/// fetch alone therefore fixed the rarer shape and left the common one exiting
/// 1 — the sibling-caller failure a per-path table invites by construction.
///
/// Kept distinct from [`REFUSAL_NEEDLES`] all the same, because the two answer
/// different questions: that table asks "why did the server refuse this write",
/// this one asks "was the credential let in at all". A push consults this one
/// first, and only reaches the refusal table when the answer is yes.
///
/// **Measured against git 2.54.0** under this transport's own child environment
/// (`LC_ALL=C`, `GIT_TERMINAL_PROMPT=0`), driving a loopback server that answers
/// `GET /info/refs` with each status:
///
/// | Answer | What git printed |
/// |---|---|
/// | 401, nothing to offer | `fatal: could not read Username for '<url>': terminal prompts disabled` |
/// | 401, a credential offered and rejected | `remote: HTTP Basic: Access denied` then `fatal: Authentication failed for '<url>'` |
/// | 403 | `fatal: unable to access '<url>': The requested URL returned error: 403` |
///
/// The push prints the **same** first line — confirmed end to end against the
/// acceptance fixture's authorization gate, which refuses `git-receive-pack`
/// while serving `git-upload-pack`: `git push failed: fatal: could not read
/// Username for '<url>': terminal prompts disabled`. That is why no new needle
/// was needed to fix the push path, only a scope on the rows that already
/// existed.
///
/// The bare number `401` appears in none of them, which is why the first two
/// rows are phrases rather than the code: a table written by symmetry with the
/// 403 row would match nothing, and every rejected credential would keep exiting
/// with the generic status.
///
/// The recorded residual runs the other way — libcurl's
/// `The requested URL returned error: 401` was **not** reproducible on the smart
/// HTTP path (git converts a 401 into a credential request before curl's message
/// escapes, on every `WWW-Authenticate` scheme tried and with a helper that
/// returns nothing), so no row is written for it: an unfalsifiable row reads as
/// coverage while providing none.
///
/// Two rows are git's own gettext-translated strings, protected by the `LC_ALL=C`
/// this transport sets on every child; the third is libcurl's and is not
/// translated. The `403` needle is deliberately the same literal
/// [`REFUSAL_NEEDLES`] carries, not a shortened one — the bare number appears in
/// ordinary object counts.
const CREDENTIAL_REJECTIONS: [(&str, u16, RejectionScope); 3] = [
    ("could not read Username for ", 401, RejectionScope::EveryInvocation),
    ("Authentication failed for ", 401, RejectionScope::EveryInvocation),
    ("The requested URL returned error: 403", 403, RejectionScope::FetchOnly),
];

/// The fixed `detail` a git-side credential rejection carries into
/// [`ForgeError::Status`].
///
/// Classifier-owned, never a slice of the body, and it opens with `": "` because
/// the variant's format string appends it straight onto the URL — the same
/// convention `error.rs`'s `status_detail` builds. `git`'s stderr is the one
/// channel where a secret arrives *inside* forge-controlled bytes, so the detail
/// that reaches an operator is a sentence this file writes rather than one a
/// server chose.
const CREDENTIAL_REJECTED_DETAIL: &str = ": the git remote rejected the credential";

/// The HTTP status a rejected credential earned on `invocation`, or `None` when
/// the text says something else.
///
/// Declaration order is precedence, as in [`classify_push_failure`], though the
/// three recorded shapes are disjoint in practice: a 401 that reached the
/// credential prompt cannot also carry curl's 403 line.
///
/// A local plumbing step cannot produce any of these phrases — `read-tree`,
/// `write-tree`, `commit-tree`, `hash-object`, `update-index`, `update-ref`,
/// `rev-parse` and `rev-list` speak to no server — so a caller reading a
/// non-push invocation's stderr does not have to know which of them touched the
/// network to use this safely.
///
/// **True once `git_command`'s `GIT_NO_LAZY_FETCH` refusal is in effect, and
/// best-effort below the git version that honours it** (see that constant's
/// doc for the floor). Below it, `write-tree` can still resolve a missing
/// promisor blob by fetching it — the one member of this list that reaches the
/// network anyway — and the fetch is credential-less, so it fails as a rejected
/// credential rather than as a network error.
#[must_use]
pub fn credential_rejection_status(stderr: &Redacted, invocation: GitInvocation) -> Option<u16> {
    CREDENTIAL_REJECTIONS
        .iter()
        .find(|(needle, _, scope)| scope.covers(invocation) && stderr.as_str().contains(needle))
        .map(|&(_, status, _)| status)
}

/// Name a rejected credential as the forge status it really was.
///
/// [`ForgeError::Status`] rather than a variant of this transport's own, because
/// what happened *is* a 401 or a 403 the forge answered with — `git` is simply
/// the HTTP client that read it. Reusing the variant makes exit 80 fall out of
/// the classification table that already maps 401/403 to
/// [`crate::cli::ExitCode::AuthError`], with no new arm to keep in step with the
/// published claim table.
///
/// `remote` is the URL the workspace was opened for. It never carries the
/// credential: C-034 injects the pair as an `http.<prefix>.extraHeader` in the
/// child environment, never as userinfo, which is what makes it safe to name in
/// an error at all.
///
/// **Both git paths call this**, which is the point: the fetch's failure handler
/// and the push classifier ask the same question of the same table, so a
/// credential the forge rejects earns exit 80 whichever invocation met the
/// refusal — the claim table's promise does not qualify itself by git subcommand.
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
    /// `Skipped` cells flip to 86), as `== CheckStatus::Passed`, or dropping the
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
    /// retryable race on an older GitLab into a terminal 86 and kill the retry
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
    /// the case that decides whether the 86 promotion is reachable in production
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
            "the combined body must promote, or the 86 arm is dead code in production"
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
    /// a widened matcher shows up as an 86 an operator cannot act on. Each row
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

    /// The promoted 86 names the row that was unreadable, the repository, and
    /// S-017's two-signal remedy.
    ///
    /// There are two different 86s and they carry two different remedies. The
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
                    "this is not the preflight's 86 — its Settings remedy belongs to the readable-false path"
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

    /// A 403 on a push still reaches the documented 77/86 verdict rather than
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
