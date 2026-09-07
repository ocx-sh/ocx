// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The forge-neutral operation set `announce` drives, and the vocabulary types
//! it speaks in.
//!
//! [`Forge`] is the whole surface [`crate::announce::announce`] and
//! `ocx package claim` need, and no forge is named in any of them. A second
//! forge is a second implementation of this trait, never a variant threaded
//! through the orchestration — the orchestration must not learn which forge it
//! is talking to, or every C-cell decision it holds would need re-deciding per
//! forge.
//!
//! The operation count is deliberately **not** stated here. It was stated once,
//! went stale the first time an operation was added, and a number in prose has
//! no way to notice that. Read the trait.
//!
//! `async-trait` is used for the same reason [`crate::oci::index::IndexImpl`]
//! uses it: the trait is consumed as `&dyn Forge` at exactly one call site per
//! run, so `async fn` in trait (not `dyn`-compatible) buys nothing here.

use std::collections::BTreeMap;

use super::{ForgeError, ForkIdentity, PullRequest, RepoCoordinate};

/// How a branch stands relative to a base ref.
///
/// The distinction that matters to announce is [`Ahead`](Self::Ahead) versus
/// [`Diverged`](Self::Diverged), and it is not cosmetic. An `Ahead` branch
/// fast-forwards onto the base, so appending to it always produces a mergeable
/// pull request. A `Diverged` branch does not — and the ordinary way an announce
/// branch becomes `Diverged` is that its pull request was **squash-merged**, which
/// puts its content on the base under a new commit while leaving none of its own
/// commits in the base's history. Appending there re-proposes work that is already
/// merged, and the pull request conflicts on the very file every announce edits
/// (ocx-sh/ocx#228).
///
/// Git alone cannot tell that case apart from "my commits are genuinely unmerged
/// and the base moved on underneath me", so `Diverged` is never a verdict by
/// itself: the caller pairs it with whether an open pull request still exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchComparison {
    /// The branch is the base commit.
    Identical,
    /// The branch holds commits the base does not, and the base holds none the
    /// branch does not — a fast-forward.
    Ahead,
    /// The base has moved on; the branch holds nothing of its own.
    Behind,
    /// Both sides hold commits the other does not.
    Diverged,
}

/// Whether an open pull request can merge into its base as it stands.
///
/// A detector, not a gate: announce consults it on exactly one path — an
/// otherwise-unchanged run whose branch was rebuilt onto the current base and
/// still carries an open pull request. Every other outcome commits with
/// [`RefUpdate::Reset`], which makes the request mergeable by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mergeability {
    /// The forge reports the request merges into its base cleanly.
    Mergeable,
    /// The forge reports the request conflicts with its base.
    Conflicting,
    /// No verdict is available — the forge is still computing one, or the pull
    /// request is not there to answer for. Benign either way: a request that
    /// does not exist cannot conflict, and a forge mid-computation answers on
    /// the next run.
    Unknown,
}

/// Whether a ref update may rewrite history.
///
/// [`FastForward`](Self::FastForward) is the default and the one every ordinary
/// announce uses: it is the compare-and-swap that makes a concurrent announce
/// surface as [`ForgeError::NonFastForward`] instead of being silently
/// overwritten (design register C4). [`Reset`](Self::Reset) is reserved for the
/// one case where a non-fast-forward is the *intent* — repointing a spent
/// announce branch at the upstream base, where refusing to rewrite would preserve
/// exactly the already-merged commits that make the branch unusable.
///
/// Every implementation owes the same guarantee, however its forge spells it: a
/// [`FastForward`](Self::FastForward) commit whose base moved under it must fail
/// with [`ForgeError::NonFastForward`], never succeed by clobbering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefUpdate {
    /// Reject an update that is not a fast-forward.
    FastForward,
    /// Repoint the ref even when the new commit is not a descendant.
    Reset,
}

/// Where a commit's base commit lives.
///
/// The sha alone is not enough. An announce that starts a fresh branch bases it
/// on the **upstream** index's default branch — never on whatever the fork's own
/// default branch happens to hold, which on a long-lived fork is routinely
/// months behind and would silently re-propose stale content. So the base
/// carries its repository as well as its sha, and a forge that must name the
/// source project to reach an object outside the target repository has it.
#[derive(Debug, Clone, Copy)]
pub struct CommitBase<'a> {
    /// The repository the base commit is read from — the upstream index for a
    /// fresh or rebuilt branch, the branch's own repository when accumulating.
    pub repo: &'a RepoCoordinate,
    /// The base commit sha.
    pub sha: &'a str,
    /// The branch in `repo` the base sha was read from.
    ///
    /// Only meaningful when `repo` is **not** the repository being committed to:
    /// the base object then reaches the target only through the fork network, so
    /// an implementation that has to sync a fork before it can parent off that
    /// object needs the branch name to sync (see [`Forge::sync_fork`]).
    pub branch: &'a str,
}

/// The account behind a forge credential or a login.
///
/// `bot` is the **forge's own** assertion — GitLab's `bot` field, GitHub's
/// `type == "Bot"` — never a guess from the login string. A heuristic over
/// names ("does it end in `-bot`") would both miss real service accounts and
/// libel human ones, and the value gates whether a claim may be authored at
/// all, so it carries only what the forge said.
///
/// Deliberately no `Default`: a defaulted identity is `bot: false` for an
/// account nobody looked up, which is the one wrong answer that fails open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForgeIdentity {
    /// The account's login/username as the forge spells it.
    pub login: String,
    /// The forge's immutable numeric id for the account.
    pub id: u64,
    /// The forge's own assertion that this account is a bot.
    pub bot: bool,
}

/// A capability the write preflight can report on.
///
/// The [`std::fmt::Display`] spellings are a **wire vocabulary**: they are
/// rendered into the JSON report a pipeline parses, so they are one-way once
/// shipped. Renaming one is a format break, not a refactor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapabilityName {
    /// The local `git` version against the floor the git transport needs.
    GitVersion,
    /// The credential's push permission on the repository being written.
    PushAccess,
    /// Whether the project lets a CI job token push to its repository.
    JobTokenPush,
    /// Whether the index project's job-token allowlist admits the publishing
    /// project.
    JobTokenAllowlist,
}

impl CapabilityName {
    /// Every capability, in declaration order.
    ///
    /// The single source of that order: [`PushAccess::skipped_all`] seeds its
    /// rows from here, and the report renders them in the order it finds them,
    /// so the declaration order below is the array order a consumer sees. A
    /// second hand-written list would be a second definition of the same
    /// contract, free to drift.
    pub const ALL: [Self; 4] = [
        Self::GitVersion,
        Self::PushAccess,
        Self::JobTokenPush,
        Self::JobTokenAllowlist,
    ];
}

impl std::fmt::Display for CapabilityName {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::GitVersion => "git-version",
            Self::PushAccess => "push-access",
            Self::JobTokenPush => "job-token-push",
            Self::JobTokenAllowlist => "job-token-allowlist",
        })
    }
}

/// How one capability check came out.
///
/// **There is no `Failed`, and that is a decision rather than an omission.** No
/// code path can produce one: a preflight row either passes, cannot be read
/// (`Unknown`), does not apply to this run (`Skipped`), or raises a
/// [`ForgeError`] that ends the run before any report exists. A "failed" check
/// and a rendered report can therefore never coexist. Because these spellings
/// are a wire vocabulary that cannot be withdrawn once shipped, an unreachable
/// variant would be a permanent published value no run can emit and no consumer
/// can branch on. The day a check can fail without failing the run, adding it
/// back is additive.
///
/// The absence is held by the compiler, not by a test: every `match` over this
/// enum is wildcard-free, so introducing a variant is an `E0004` build failure
/// at each of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckStatus {
    /// The capability was read and is present.
    Passed,
    /// The capability could not be read — an older instance, or a field the
    /// credential may not see. Never fails the call.
    Unknown,
    /// The capability does not apply to this run.
    Skipped,
}

impl CheckStatus {
    /// Every status, in declaration order.
    ///
    /// The single source of that order: `check_status_wire_spellings_and_arity`
    /// walks this array to check its length against [`Display`](std::fmt::Display)'s
    /// match arms, so a variant added here without a spelling added there is an
    /// `E0004` build failure (wildcard-free match), and a spelling added there
    /// without extending this array leaves the test counting the old length. A
    /// second hand-written list would be a second definition of the same
    /// contract, free to drift.
    pub const ALL: [Self; 3] = [Self::Passed, Self::Unknown, Self::Skipped];
}

impl std::fmt::Display for CheckStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Passed => "passed",
            Self::Unknown => "unknown",
            Self::Skipped => "skipped",
        })
    }
}

/// One row of the write preflight.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapabilityCheck {
    /// Which capability this row reports on.
    pub name: CapabilityName,
    /// How it came out.
    pub status: CheckStatus,
    /// Human-readable amplification, drawn **only** from a closed set of values
    /// ocx already holds: the parsed git version, the numeric access level, the
    /// name of the field that could not be read, a project path. **Never** a
    /// forge response body — widening that closure means routing the new source
    /// through the redactor first.
    ///
    /// Populated only where the forge exposes a meaningful qualifier — GitLab's
    /// `push-access` row carries `Some("access level {n}")` because the API
    /// returns a numeric access level, GitHub's carries `None` because its
    /// permissions payload is a bare boolean with nothing further to report.
    /// This asymmetry between forges is intended, not a gap: do not invent a
    /// GitHub qualifier to make the two agree.
    pub detail: Option<String>,
}

/// What the write preflight checked, and how each check came out.
///
/// Returned by [`Forge::ensure_push_access`] so a caller can prove the preflight
/// ran rather than trusting a bare success.
///
/// **`checks` is private, and that privacy is the whole contract.** The only way
/// to obtain a `PushAccess` is [`Self::skipped_all`], which seeds one row per
/// [`CapabilityName`]; implementations then fill rows in through
/// [`Self::record`]. There is no `Default`, no `Deserialize`, no
/// `From<Vec<CapabilityCheck>>` and no test-only builder, because each of those
/// is a second way to reach an empty vector. "The report's capability array is
/// non-empty on every run" is therefore unrepresentable-otherwise rather than
/// asserted somewhere downstream, where a test could only check a vector it
/// built itself.
///
/// This type is declared **here**, in the module that owns the trait, and not in
/// `forge.rs`: every forge submodule is a descendant of `forge`, so a private
/// field declared there would be visible to exactly the implementations the rule
/// constrains. Declared here, a sibling module is refused with `E0451`.
///
/// Deliberately **not** `#[must_use]`: the announce orchestration calls
/// `ensure_push_access` for its refusal and discards the value, and the
/// workspace denies warnings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushAccess {
    checks: Vec<CapabilityCheck>,
}

impl PushAccess {
    /// One row per [`CapabilityName`], every one [`CheckStatus::Skipped`].
    ///
    /// The only constructor. Rows arrive in [`CapabilityName::ALL`] order and
    /// keep it — [`Self::record`] replaces a row in place and never appends.
    #[must_use]
    pub fn skipped_all() -> Self {
        Self {
            checks: CapabilityName::ALL
                .into_iter()
                .map(|name| CapabilityCheck {
                    name,
                    status: CheckStatus::Skipped,
                    detail: None,
                })
                .collect(),
        }
    }

    /// Replace `name`'s row with `status` and `detail`.
    ///
    /// In place, so the row order stays [`CapabilityName::ALL`]'s and the vector
    /// can never grow past it.
    ///
    /// **Replace, not upgrade** — two things a caller might read into the name
    /// that this does not do, stated because the first real callers are the
    /// preflight implementations and the difference only bites there:
    ///
    /// - There is no status ordering. Calling this twice for one `name` leaves
    ///   the second `status` and the second `detail`, so a second call can walk
    ///   a row back from [`CheckStatus::Passed`] to [`CheckStatus::Skipped`] and
    ///   drop a `detail` the first call set. Record each row once.
    /// - An unrecorded `name` is a silent no-op rather than an error. Not
    ///   reachable today — [`Self::skipped_all`] is the only constructor and it
    ///   seeds every [`CapabilityName`], and `name` is that enum, so the search
    ///   always hits — but it is the behaviour, not a promise, and it would stop
    ///   being unreachable the moment a second constructor existed. That is one
    ///   more reason there is not one.
    pub fn record(&mut self, name: CapabilityName, status: CheckStatus, detail: Option<String>) {
        for check in &mut self.checks {
            if check.name == name {
                check.status = status;
                check.detail = detail;
                return;
            }
        }
    }

    /// Every row, in [`CapabilityName`] declaration order.
    #[must_use]
    pub fn checks(&self) -> &[CapabilityCheck] {
        &self.checks
    }

    /// The status recorded for `name`.
    ///
    /// The read the `git push` stderr classifier needs: an HTTP 403 is promoted
    /// to a capability refusal **only** when the preflight already reported
    /// `job-token-push` as [`CheckStatus::Unknown`], so the promotion is driven
    /// by what was checked and never by the phrase alone.
    #[must_use]
    pub fn status(&self, name: CapabilityName) -> CheckStatus {
        self.checks
            .iter()
            .find(|check| check.name == name)
            .map_or(CheckStatus::Skipped, |check| check.status)
    }
}

/// The forge operations `announce` and `claim` drive.
///
/// Implementations own their wire format; they do **not** own policy. Every
/// method's contract below is the orchestration's contract, and an
/// implementation that cannot hold it must return an error rather than
/// approximate it.
///
/// Security invariants every implementation owes (design register X5/X6): a
/// no-redirect HTTP client so a cross-host 3xx cannot replay the credential, the
/// credential carried as a header and never in a URL or argv, a fork identity
/// built only from API response bodies and verified against the upstream, and a
/// bounded readiness wait.
///
/// # The write transport changes some of these contracts
///
/// A forge is built for one [`super::WriteTransport`], and the choice is
/// invisible to the caller: the same trait answers either way. It is **not**
/// invisible to an implementor, so the per-transport contract is stated on each
/// signature below rather than only in the design record. In summary:
///
/// - Every **read** stays REST under both transports, with one exception:
///   [`Self::compare_branch`] under `git` is computed from the local clone,
///   because a CI job token has no compare endpoint — which is the whole reason
///   the second transport exists.
/// - [`Self::commit_files`] and [`Self::open_or_update_pull_request`] dispatch
///   on the transport.
/// - [`Self::find_fork`] and [`Self::ensure_fork`] return
///   [`ForgeError::TransportOperationUnsupported`] under `git`.
/// - [`Self::sync_fork`] is a logged no-op under `git`.
#[async_trait::async_trait]
pub trait Forge: Send + Sync {
    /// The account the credential authenticates as, or `None` when the
    /// credential has no account behind it.
    ///
    /// The two negative answers are **different outcomes and must stay
    /// distinguishable**, because the owner ladder reads them differently:
    ///
    /// - `Ok(None)` — the credential is legitimate and simply has no user (a
    ///   GitHub App installation token). The caller falls through to an explicit
    ///   `--owner` or a CI-provided identity.
    /// - `Err(ForgeError::UsersApiUnavailable)` — the credential may not call
    ///   the identity endpoint at all (a GitLab CI job token). A bare `LOGIN`
    ///   cannot be resolved to an id, so the caller must be told to write
    ///   `LOGIN:ID`.
    ///
    /// An implementation that collapses the second into the first costs the
    /// caller that distinction and turns a fixable invocation into a silent
    /// fallback.
    ///
    /// # Errors
    ///
    /// [`ForgeError::UsersApiUnavailable`] when the credential may not call the
    /// endpoint; any other [`ForgeError`] on transport, status or decode
    /// failure.
    async fn authenticated_identity(&self) -> Result<Option<ForgeIdentity>, ForgeError>;

    /// The account behind `login`, or `None` when the forge has no such account.
    ///
    /// # Errors
    ///
    /// [`ForgeError::UsersApiUnavailable`] when the credential may not call the
    /// users API — see [`Self::authenticated_identity`] for why that is not the
    /// same answer as `Ok(None)`; any other [`ForgeError`] on transport, status
    /// or decode failure.
    async fn resolve_user(&self, login: &str) -> Result<Option<ForgeIdentity>, ForgeError>;

    /// The bytes of `path` at `r#ref`, or `None` when the path is absent there.
    ///
    /// REST under both transports.
    ///
    /// # Errors
    ///
    /// Returns a [`ForgeError`] on transport failure or a non-success status
    /// other than "absent".
    async fn get_file_contents(
        &self,
        repo: &RepoCoordinate,
        path: &str,
        r#ref: &str,
    ) -> Result<Option<Vec<u8>>, ForgeError>;

    /// The commit SHA `r#ref` points at, or `None` when the ref does not exist.
    ///
    /// REST under both transports.
    ///
    /// # Errors
    ///
    /// Returns a [`ForgeError`] on transport failure or a non-success status
    /// other than "absent".
    async fn get_ref_sha(&self, repo: &RepoCoordinate, r#ref: &str) -> Result<Option<String>, ForgeError>;

    /// How `head`'s `head_branch` stands relative to `repo`'s `base`.
    ///
    /// **The one read that is not REST under the `git` transport**: it is
    /// computed from the local clone, exactly and not approximately, because a
    /// CI job token has no compare endpoint. It is not called at all when the
    /// branch does not exist — there is no second ref to compare.
    ///
    /// # Errors
    ///
    /// Returns a [`ForgeError`] on transport failure, a non-success status, or a
    /// comparison the implementation cannot classify — an unmodelled result is an
    /// error, never a guess, because a wrong "not ahead" strands a committed
    /// announce with no pull request.
    async fn compare_branch(
        &self,
        repo: &RepoCoordinate,
        base: &str,
        head: &RepoCoordinate,
        head_branch: &str,
    ) -> Result<BranchComparison, ForgeError>;

    /// The open pull/merge request whose head is `head`'s `branch`, or `None`.
    ///
    /// Scoped to **open** requests: the announce branch is per package and
    /// outlives every request opened from it, so "a request exists" is not the
    /// same question as "this branch is still carrying one".
    ///
    /// REST under both transports.
    ///
    /// # Errors
    ///
    /// Returns a [`ForgeError`] on transport failure, a non-success status, or a
    /// malformed response body.
    async fn find_open_pull_request(
        &self,
        index: &RepoCoordinate,
        head: &RepoCoordinate,
        branch: &str,
    ) -> Result<Option<PullRequest>, ForgeError>;

    /// Whether the open pull request `number` on `index` can merge into its
    /// base.
    ///
    /// Read-only, **one request, never a poll**. A forge that has not finished
    /// computing the answer reports [`Mergeability::Unknown`], and so does a
    /// pull request that is not found — the caller treats both as benign and
    /// asks again on the next run, so an implementation must not wait for a
    /// verdict it can report as unknown.
    ///
    /// `number` is the request's project-local number scoped to `index`:
    /// GitHub's `number`, GitLab's `iid`. It is never the forge's internal
    /// database id.
    ///
    /// REST under both transports.
    ///
    /// # Errors
    ///
    /// Returns a [`ForgeError`] on transport failure or a non-success status
    /// other than "absent".
    async fn pull_request_mergeability(&self, index: &RepoCoordinate, number: u64) -> Result<Mergeability, ForgeError>;

    /// Look up an existing fork of `upstream` at `fork`, **without creating
    /// one**. `None` when nothing is there, or when what is there is not a
    /// verified fork of `upstream` (a same-named stranger repository).
    ///
    /// Read-only by contract: the caller resolves the fork's real identity before
    /// deciding whether any write is needed at all, so a pure no-op run never
    /// provokes a fork create (design register C6).
    ///
    /// Under the `git` transport this returns
    /// [`ForgeError::TransportOperationUnsupported`]: the credential that
    /// transport exists for cannot reach the fork API at all, so a fork is
    /// refused up front rather than failing opaquely at the first call.
    ///
    /// # Errors
    ///
    /// Returns a [`ForgeError`] on transport failure, a non-success status other
    /// than "absent", or a verified fork living under an unexpected owner;
    /// [`ForgeError::TransportOperationUnsupported`] under the `git` transport.
    async fn find_fork(
        &self,
        upstream: &RepoCoordinate,
        fork: &RepoCoordinate,
    ) -> Result<Option<ForkIdentity>, ForgeError>;

    /// Ensure a fork of `upstream` exists and is ready, returning its verified
    /// identity. `target_owner` = `None` forks under the token identity; `Some`
    /// forks into that owner and verifies the returned identity against it.
    ///
    /// Under the `git` transport this returns
    /// [`ForgeError::TransportOperationUnsupported`], for the same reason as
    /// [`Self::find_fork`].
    ///
    /// # Errors
    ///
    /// Returns a [`ForgeError`] on transport failure, a non-success status, a
    /// fork identity that fails verification against `upstream` or the expected
    /// owner, or a readiness wait that exceeds its deadline;
    /// [`ForgeError::TransportOperationUnsupported`] under the `git` transport.
    async fn ensure_fork(
        &self,
        upstream: &RepoCoordinate,
        target_owner: Option<&str>,
    ) -> Result<ForkIdentity, ForgeError>;

    /// Bring `fork`'s `branch` up to its upstream, where the forge needs that to
    /// make an upstream base object reachable from the fork.
    ///
    /// Best-effort by contract, and legitimately a no-op: it only moves *where* a
    /// base object lives, so it is never a precondition of the commit that
    /// follows. A forge whose commit API can parent off the upstream project
    /// directly has nothing to do here, and under the `git` transport it is a
    /// logged no-op — a `tracing` line and nothing else.
    async fn sync_fork(&self, fork: &RepoCoordinate, branch: &str);

    /// Verify the credential may push a branch to `repo` before anything is
    /// written there, and report what was checked.
    ///
    /// The fork-free path commits onto the index repository itself, so an
    /// unauthorised credential would otherwise fail partway through the commit
    /// sequence and surface as a bare status code. One permission read collapses
    /// that into a named error before any write is attempted. The returned
    /// [`PushAccess`] lets a caller prove the preflight ran rather than trusting
    /// a bare success.
    ///
    /// **An observed denial still errors; only an unreadable field degrades.**
    /// The two halves are easy to conflate and mean opposite things:
    ///
    /// - A permission field that reads "no push" is a *verdict*. It raises
    ///   [`ForgeError::PushAccessDenied`] and ends the run, as it always has.
    /// - A capability field that could not be read at all — an older instance,
    ///   or a credential that may not see it — is *absence of evidence*. It
    ///   records [`CheckStatus::Unknown`] and the call proceeds.
    /// - A credential that may not call the **visibility endpoint itself** is
    ///   the same absence one level up: the read that would answer "may this
    ///   credential push" is out of reach, so there is no verdict to report.
    ///   It records [`CheckStatus::Unknown`] — but **only where a later write
    ///   renders the verdict**. On a path that performs no such write there is
    ///   nothing left to answer the question, and the unreadable read is a
    ///   denial. On GitLab this is a CI job token, which cannot call
    ///   `GET /projects/:id`: under the `git` transport the push decides and a
    ///   real refusal is promoted from its stderr, while under `api` the
    ///   refusal is raised here.
    ///
    /// Reading only "a check that cannot be read reports `Unknown`" as licence
    /// to swallow the first case would turn a refused push into a silent one;
    /// applying the third without its write would turn it into a bare status
    /// code from the commit that follows, which is what this preflight exists
    /// to collapse.
    ///
    /// Under the `git` transport the same REST probe runs, plus the job-token
    /// capability checks, plus the `git-version` row rendered from the
    /// `GitBinary` the constructor was given — that check ran at the argv
    /// boundary, but its row is emitted here so the capability array has exactly
    /// one assembler.
    ///
    /// # Errors
    ///
    /// Returns [`ForgeError::PushAccessDenied`] when the repository reports no
    /// push permission, or is invisible to the credential on a path where no
    /// later write can render the verdict;
    /// [`ForgeError::WriteCapabilityUnavailable`] when a capability the selected
    /// transport requires reads as *disabled* (as opposed to unreadable); or any
    /// other [`ForgeError`] on transport, status, or decode failure.
    async fn ensure_push_access(&self, repo: &RepoCoordinate) -> Result<PushAccess, ForgeError>;

    /// Commit `files` **atomically** onto `branch` at `base`, returning the
    /// new commit SHA. One commit carries every file (design register C15) — never
    /// a loop over a single-file API, which would leave a half-written index entry
    /// visible on any failure.
    ///
    /// **Dispatches on the transport.** Under `api` it commits and updates the
    /// ref over REST, raising [`ForgeError::NonFastForward`] here. Under `git` it
    /// builds objects and moves a *local* ref and **performs no network write at
    /// all** — so a `commit_files` not followed by
    /// [`Self::open_or_update_pull_request`] publishes nothing, and the
    /// orchestration must not treat it as having done so.
    ///
    /// # Errors
    ///
    /// Returns a [`ForgeError`] on transport failure, a non-success status, or a
    /// malformed response. A base that moved under a
    /// [`RefUpdate::FastForward`] commit MUST surface as
    /// [`ForgeError::NonFastForward`]: the caller answers that by re-reading the
    /// winning head and regenerating against it (design register C4), and any
    /// other classification silently loses the concurrent announce.
    async fn commit_files(
        &self,
        repo: &RepoCoordinate,
        branch: &str,
        base: CommitBase<'_>,
        message: &str,
        files: &BTreeMap<String, Vec<u8>>,
        update: RefUpdate,
    ) -> Result<String, ForgeError>;

    /// Open a pull/merge request from `head`'s `branch` into `index`'s `base`,
    /// or reuse the existing open one — never duplicate.
    ///
    /// `head` is a whole coordinate, not an owner string: GitHub spells a
    /// cross-repository head as `owner:branch` against the upstream, while GitLab
    /// posts to the *source* project and names the target by id. Only the
    /// implementation knows which half of the coordinate it needs.
    ///
    /// **Dispatches on the transport.** Under `api` it opens or reuses the
    /// request over REST. Under `git` it performs the single push carrying both
    /// the ref update and the merge-request push options, then confirms the
    /// request through a bounded REST poll, because the server creates it
    /// asynchronously — [`ForgeError::NonFastForward`] is raised *here* under
    /// that transport, and [`ForgeError::MergeRequestUnconfirmed`] when the
    /// poll's bound is exhausted. **Except with no pending local commit**, where
    /// it first reads the open requests over REST and **writes nothing if one
    /// exists**; if none exists it creates a refresh commit so the ref genuinely
    /// advances, and then pushes as above.
    ///
    /// # Errors
    ///
    /// Returns a [`ForgeError`] on transport failure, a non-success status that
    /// does not mean "one already exists", or a malformed response body; under
    /// `git` additionally [`ForgeError::NonFastForward`],
    /// [`ForgeError::StaleLease`], [`ForgeError::PushRefused`],
    /// [`ForgeError::WriteCapabilityUnavailable`],
    /// [`ForgeError::MergeRequestUnconfirmed`] or [`ForgeError::GitPushFailed`].
    async fn open_or_update_pull_request(
        &self,
        index: &RepoCoordinate,
        head: &RepoCoordinate,
        branch: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<PullRequest, ForgeError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// C-012's wire vocabulary, paired against [`CapabilityName::ALL`].
    ///
    /// The pairing is what makes the arity a guard rather than a number this
    /// test wrote for itself: the spellings are read out of `ALL` positionally,
    /// so a capability added to `ALL` without a spelling here reds on the
    /// length, and one renamed in `Display` reds on its own row. A capability
    /// added to the enum but not to `ALL` never reaches this test at all — the
    /// wildcard-free `Display` match is an `E0004` build failure first.
    ///
    /// Reds on: renaming any spelling in `CapabilityName`'s `Display` impl,
    /// reordering `ALL`, or shortening it.
    #[test]
    fn capability_name_wire_spellings() {
        let expected = [
            (CapabilityName::GitVersion, "git-version"),
            (CapabilityName::PushAccess, "push-access"),
            (CapabilityName::JobTokenPush, "job-token-push"),
            (CapabilityName::JobTokenAllowlist, "job-token-allowlist"),
        ];
        assert_eq!(
            CapabilityName::ALL.len(),
            expected.len(),
            "every capability owes a contracted wire spelling; ALL is {:?}",
            CapabilityName::ALL
        );
        for (index, (name, spelling)) in expected.into_iter().enumerate() {
            assert_eq!(
                CapabilityName::ALL[index],
                name,
                "declaration order is the order a report consumer sees"
            );
            assert_eq!(
                name.to_string(),
                spelling,
                "the spellings are rendered into a parsed JSON report — one-way once shipped"
            );
        }
    }

    /// The reachable half of what `check_status_has_no_failed_variant` promised
    /// (DX-15): three wire spellings plus an arity, paired against
    /// [`CheckStatus::ALL`].
    ///
    /// The absence of a `Failed` variant is deliberately **not** asserted here.
    /// Absence has no runtime representation, and the mutation that would test
    /// it — adding the variant — breaks the build rather than reding this test,
    /// which is not a red. The absence stays held by the compiler instead:
    /// every match over `CheckStatus` is wildcard-free, so a fourth variant is
    /// an `E0004` at each of them.
    ///
    /// Reds on: renaming a spelling in `CheckStatus`'s `Display` impl,
    /// reordering `ALL`, or changing `ALL`'s length.
    #[test]
    fn check_status_wire_spellings_and_arity() {
        let expected = [
            (CheckStatus::Passed, "passed"),
            (CheckStatus::Unknown, "unknown"),
            (CheckStatus::Skipped, "skipped"),
        ];
        assert_eq!(
            CheckStatus::ALL.len(),
            expected.len(),
            "a status without a contracted spelling cannot be rendered; ALL is {:?}",
            CheckStatus::ALL
        );
        for (index, (status, spelling)) in expected.into_iter().enumerate() {
            assert_eq!(CheckStatus::ALL[index], status, "declaration order is the array order");
            assert_eq!(
                status.to_string(),
                spelling,
                "the spellings are a wire vocabulary that cannot be withdrawn once shipped"
            );
        }
    }

    /// C-011/C-069: the only constructor seeds one row per capability, in
    /// [`CapabilityName`] declaration order, and [`PushAccess::record`] replaces
    /// a row in place rather than appending.
    ///
    /// Order is the contract, not membership — the report renders the rows in
    /// the order it finds them — so the sequence is asserted, never a set.
    ///
    /// One test rather than two, and named for both halves: they are the same
    /// invariant read at the two moments it can break. Splitting would duplicate
    /// the `skipped_all()` fixture so the `record` half could re-establish
    /// exactly what the seeding half just asserted, which is the seam, not
    /// separate coverage.
    ///
    /// Reds on: seeding from anything but `CapabilityName::ALL` or in any other
    /// order, seeding a status other than `Skipped`, or making `record` append
    /// instead of replace.
    #[test]
    fn push_access_seeds_every_name_in_declaration_order_and_records_in_place() {
        let access = PushAccess::skipped_all();
        let seeded: Vec<CapabilityName> = access.checks().iter().map(|check| check.name).collect();
        assert_eq!(
            seeded,
            CapabilityName::ALL.to_vec(),
            "the capability array a consumer parses is this sequence, not a set"
        );
        for check in access.checks() {
            assert_eq!(
                check.status,
                CheckStatus::Skipped,
                "{} was not seeded skipped",
                check.name
            );
            assert_eq!(check.detail, None, "{} was seeded with a detail", check.name);
            assert_eq!(
                access.status(check.name),
                check.status,
                "the accessor must answer what the row holds"
            );
        }

        // `record` upgrades in place, so no implementation filling rows in can
        // grow the array or reorder it.
        let mut upgraded = PushAccess::skipped_all();
        upgraded.record(
            CapabilityName::JobTokenPush,
            CheckStatus::Passed,
            Some("access level 30".to_string()),
        );
        assert_eq!(
            upgraded.checks().iter().map(|check| check.name).collect::<Vec<_>>(),
            CapabilityName::ALL.to_vec(),
            "recording a row must not append or reorder"
        );
        assert_eq!(upgraded.status(CapabilityName::JobTokenPush), CheckStatus::Passed);
        assert_eq!(
            upgraded.status(CapabilityName::GitVersion),
            CheckStatus::Skipped,
            "recording one row must not disturb the others"
        );
    }
}
