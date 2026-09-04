// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The forge-neutral operation set `announce` drives, and the vocabulary types
//! it speaks in.
//!
//! [`Forge`] is the whole surface [`crate::announce::announce`] needs: ten
//! operations, no forge named in any of them. A second forge is a second
//! implementation of this trait, never a variant threaded through the
//! orchestration — the orchestration must not learn which forge it is talking
//! to, or every C-cell decision it holds would need re-deciding per forge.
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

/// The forge operations `announce` drives.
///
/// Implementations own their wire format; they do **not** own policy. Every
/// method's contract below is the announce contract, and an implementation that
/// cannot hold it must return an error rather than approximate it.
///
/// Security invariants every implementation owes (design register X5/X6): a
/// no-redirect HTTP client so a cross-host 3xx cannot replay the credential, the
/// credential carried as a header and never in a URL or argv, a fork identity
/// built only from API response bodies and verified against the upstream, and a
/// bounded readiness wait.
#[async_trait::async_trait]
pub trait Forge: Send + Sync {
    /// The bytes of `path` at `r#ref`, or `None` when the path is absent there.
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
    /// # Errors
    ///
    /// Returns a [`ForgeError`] on transport failure or a non-success status
    /// other than "absent".
    async fn get_ref_sha(&self, repo: &RepoCoordinate, r#ref: &str) -> Result<Option<String>, ForgeError>;

    /// How `head`'s `head_branch` stands relative to `repo`'s `base`.
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
    /// # Errors
    ///
    /// Returns a [`ForgeError`] on transport failure, a non-success status other
    /// than "absent", or a verified fork living under an unexpected owner.
    async fn find_fork(
        &self,
        upstream: &RepoCoordinate,
        fork: &RepoCoordinate,
    ) -> Result<Option<ForkIdentity>, ForgeError>;

    /// Ensure a fork of `upstream` exists and is ready, returning its verified
    /// identity. `target_owner` = `None` forks under the token identity; `Some`
    /// forks into that owner and verifies the returned identity against it.
    ///
    /// # Errors
    ///
    /// Returns a [`ForgeError`] on transport failure, a non-success status, a
    /// fork identity that fails verification against `upstream` or the expected
    /// owner, or a readiness wait that exceeds its deadline.
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
    /// directly has nothing to do here.
    async fn sync_fork(&self, fork: &RepoCoordinate, branch: &str);

    /// Verify the credential may push a branch to `repo`, before anything is
    /// written there.
    ///
    /// The fork-free announce path commits onto the index repository itself, so
    /// an unauthorised credential would otherwise fail partway through the commit
    /// sequence and surface as a bare status code. One permission read collapses
    /// that into a named error before any write is attempted.
    ///
    /// # Errors
    ///
    /// Returns [`ForgeError::PushAccessDenied`] when the repository is invisible
    /// to the credential or reports no push permission, or any other
    /// [`ForgeError`] on transport, status, or decode failure.
    async fn ensure_push_access(&self, repo: &RepoCoordinate) -> Result<(), ForgeError>;

    /// Commit `files` **atomically** onto `branch` at `base`, returning the
    /// new commit SHA. One commit carries every file (design register C15) — never
    /// a loop over a single-file API, which would leave a half-written index entry
    /// visible on any failure.
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
    /// # Errors
    ///
    /// Returns a [`ForgeError`] on transport failure, a non-success status that
    /// does not mean "one already exists", or a malformed response body.
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
