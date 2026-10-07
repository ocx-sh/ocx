// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The forge-neutral operation set `announce` and `claim` drive, and its vocabulary types.

use std::collections::BTreeMap;

use super::{ForgeError, ForkIdentity, PullRequest, RepoCoordinate};

/// How a branch stands relative to a base ref.
///
/// `Diverged` usually means a squash-merged pull request, which git cannot tell from
/// unmerged work on a moved base, so callers pair it with whether a request is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchComparison {
    /// The branch is the base commit.
    Identical,
    /// A fast-forward: the branch holds commits the base lacks, never the reverse.
    Ahead,
    /// The base has moved on; the branch holds nothing of its own.
    Behind,
    /// Both sides hold commits the other does not.
    Diverged,
}

/// Whether an open pull request can merge into its base as it stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mergeability {
    /// The forge reports the request merges into its base cleanly.
    Mergeable,
    /// The forge reports the request conflicts with its base.
    Conflicting,
    /// No verdict yet, or no such request; benign, the next run asks again.
    Unknown,
}

/// Whether a ref update may rewrite history.
///
/// Every implementation owes this: a `FastForward` commit whose base moved fails with
/// [`ForgeError::NonFastForward`], never clobbers, or a concurrent announce is lost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefUpdate {
    /// Reject a non-fast-forward and parent only on `base.sha`: a payload derived from it,
    /// laid on another head, silently deletes what that head added, so any other branch
    /// head is [`ForgeError::NonFastForward`].
    FastForward,
    /// Reject a non-fast-forward but tolerate a branch head the caller did not read, for
    /// a payload independent of `base.sha` (a claim root). Only the git workspace parents
    /// differently; the REST arms treat it as [`FastForward`](Self::FastForward).
    Accumulate,
    /// Repoint the ref even when the new commit is not a descendant.
    Reset,
}

/// What one path in a [`Forge::commit_files`] payload asks for.
///
/// Every driver owes this: a [`Delete`](Self::Delete) of a path absent at the parent is a
/// no-op, never an error, since the orphan set may name an object never committed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileChange {
    /// Write these bytes at the path, creating or replacing.
    Put(Vec<u8>),
    /// Remove the path if it is there.
    Delete,
}

/// Where a commit's base commit lives: a fresh branch bases on the upstream index,
/// never the fork's default branch, which may be months stale.
#[derive(Debug, Clone, Copy)]
pub struct CommitBase<'a> {
    /// The upstream index for a fresh or rebuilt branch, the branch's own repository
    /// when accumulating.
    pub repo: &'a RepoCoordinate,
    /// The base commit sha.
    pub sha: &'a str,
    /// The branch in `repo` the base was read from: the git transport fetches it, and a
    /// cross-repo commit [`Forge::sync_fork`]s it before parenting off it.
    pub branch: &'a str,
}

/// The account behind a forge credential or a login.
///
/// `bot` gates claim authoring, so it is only the forge's own assertion, never a guess
/// from the login. No `Default`: `bot: false` for an unlooked-up account fails open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForgeIdentity {
    /// The account's login/username as the forge spells it.
    pub login: String,
    /// The forge's immutable numeric id for the account.
    pub id: u64,
    /// The forge's own assertion that this account is a bot.
    pub bot: bool,
}

/// A capability the write preflight reports on.
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
    /// Every capability, in the report's row order ([`PushAccess::skipped_all`] seeds from it).
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
/// No `Failed`: a failed check raises a [`ForgeError`] before any report exists, and a
/// wire spelling no run can emit could never be withdrawn once shipped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckStatus {
    /// The capability was read and is present.
    Passed,
    /// Unreadable (an older instance, or a field the credential may not see); never fails the call.
    Unknown,
    /// The capability does not apply to this run.
    Skipped,
}

impl CheckStatus {
    /// Every status; `check_status_wire_spellings_and_arity` checks it against `Display`.
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
    /// Amplification drawn only from values ocx already holds (git version, access level,
    /// field name, project path), never a raw forge response body.
    pub detail: Option<String>,
}

/// What the write preflight checked, so a caller can prove it ran.
///
/// [`Self::skipped_all`] is the only constructor: any other (`Default`, `Deserialize`,
/// `From<Vec<_>>`) could build the empty capability array the report must never carry.
// Not `#[must_use]`: the orchestration discards it, and the workspace denies warnings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushAccess {
    // Declared here, not in `forge.rs`, where a private field is visible to every forge impl.
    checks: Vec<CapabilityCheck>,
}

impl PushAccess {
    /// One [`CheckStatus::Skipped`] row per [`CapabilityName`], in [`CapabilityName::ALL`] order.
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

    /// Replace `name`'s row in place. The last call wins, even over `Passed`, so record
    /// each row once.
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
/// A contract an implementation cannot hold is an error, never an approximation; every
/// implementation owes the security invariants in [`crate::forge`]. Reads are REST under
/// both [`super::WriteTransport`]s except [`Self::compare_branch`].
#[async_trait::async_trait]
pub trait Forge: Send + Sync {
    /// The account the credential authenticates as, or `None` when it has no account.
    ///
    /// Keep `Ok(None)` (an App token: the caller falls back to `--owner`) apart from
    /// [`ForgeError::UsersApiUnavailable`] (a job token: the caller must ask for
    /// `LOGIN:ID`), or a fixable invocation becomes a silent fallback.
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
    /// users API; any other [`ForgeError`] on transport, status or decode failure.
    async fn resolve_user(&self, login: &str) -> Result<Option<ForgeIdentity>, ForgeError>;

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
    /// Computed from the local clone under `git`, where a job token has no compare endpoint.
    ///
    /// # Errors
    ///
    /// Returns a [`ForgeError`] on transport failure, a non-success status, or a
    /// comparison it cannot classify: a guessed "not ahead" strands a committed
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
    /// Open requests only: the per-package branch outlives every request opened from it.
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

    /// Whether the open pull request `number` on `index` can merge into its base.
    ///
    /// One request, never a poll: a pending verdict or a missing request is
    /// [`Mergeability::Unknown`]. `number` is project-local (GitHub `number`, GitLab `iid`).
    ///
    /// # Errors
    ///
    /// Returns a [`ForgeError`] on transport failure or a non-success status
    /// other than "absent".
    async fn pull_request_mergeability(&self, index: &RepoCoordinate, number: u64) -> Result<Mergeability, ForgeError>;

    /// Look up an existing fork of `upstream` at `fork`, **without creating one**: `None`
    /// when nothing is there or it is not a verified fork of `upstream` (a same-named
    /// stranger repository).
    ///
    /// Read-only, so a no-op run never provokes a fork create.
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
    /// identity, under `target_owner` or else the token identity.
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

    /// Bring `fork`'s `branch` up to upstream so an upstream base object is reachable
    /// from the fork. Best-effort, never a precondition of the next commit; a no-op
    /// under `git` or where the commit API parents off upstream directly.
    async fn sync_fork(&self, fork: &RepoCoordinate, branch: &str);

    /// Verify the credential may push to `repo` before any write, and report what was checked.
    ///
    /// An observed denial errors and only an unreadable field records
    /// [`CheckStatus::Unknown`], or a refused push goes silent. An invisible repository is
    /// `Unknown` only under `git`, where the push renders the verdict; under `api` it is a
    /// denial here, or the REST commit fails with a bare status code.
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

    /// Commit `files`, `Delete`s included, as one commit onto `branch` at `base`, returning
    /// the new SHA; a per-file loop would leave a half-written entry visible on failure.
    ///
    /// Under `git` it moves only a local ref, so without a following
    /// [`Self::open_or_update_pull_request`] nothing is published.
    ///
    /// # Errors
    ///
    /// Returns a [`ForgeError`] on transport failure, a non-success status, or a
    /// malformed response. A base that moved under a [`RefUpdate::FastForward`] commit
    /// must be [`ForgeError::NonFastForward`], the one error the caller regenerates on,
    /// or the concurrent announce is lost.
    async fn commit_files(
        &self,
        repo: &RepoCoordinate,
        branch: &str,
        base: CommitBase<'_>,
        message: &str,
        files: &BTreeMap<String, FileChange>,
        update: RefUpdate,
    ) -> Result<String, ForgeError>;

    /// Open a pull/merge request from `head`'s `branch` into `index`'s `base`, or reuse the
    /// open one, never duplicate.
    ///
    /// Under `git` it pushes, then polls for the request the server creates; with no
    /// pending local commit and no open request it makes a refresh commit so the push moves a ref.
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

    /// The capabilities' message spellings, paired against [`CapabilityName::ALL`]; report rows are snake_case.
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
