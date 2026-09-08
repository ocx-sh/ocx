// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The throwaway `git` clone the git write transport works in.
//!
//! One workspace per forge instance, created on the first git-half operation and
//! living until the forge is dropped — its lifetime has to span both
//! [`super::Forge::commit_files`] and
//! [`super::Forge::open_or_update_pull_request`], because the first builds a
//! local commit that only the second publishes.
//!
//! Nothing secret is ever written into the directory: the credential travels in
//! the child environment, never into `.git/config`. That is what makes a
//! directory left behind by a `SIGKILL` merely untidy rather than a leak, and a
//! future change that writes a credential into the clone invalidates the premise
//! and must say so.
//!
//! The methods below are the ADR's recipe in the recipe's own order — fetch,
//! compare, the index-file commit chain, the local ref move, the push, the
//! confirmation poll. Every invocation is built by one argv helper, so the flags
//! that must be on *all* of them ([`GitWorkspace::argv_with`]) cannot be on all
//! but one; the two that are conditional — the credential pair and the
//! `credential.helper` reset — are conditional in exactly one place.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::future::Future;
use std::path::Path;
use std::time::Duration;

use tempfile::TempDir;

use super::error::is_server_fault;
use super::git_command::{CredentialInjection, CredentialScope, LazyFetch, redact, run_git};
use super::git_push_options::{escape_newlines, render_push_options};
use super::git_stderr::{GitInvocation, classify_push_failure, classify_remote_failure};
use super::poll::{PollSchedule, backoff_delays};
use super::{
    BranchComparison, CommitBase, ForgeError, GitBinary, GitPushCredential, PullRequest, PushAccess, RefUpdate,
};

/// The remote-tracking namespace every fetch writes into.
///
/// `o` rather than `origin`: the workspace configures no remote at all — every
/// fetch and the push name a URL — so the namespace is ocx's own label, and the
/// ADR's recipe spells it `o/<ref>`.
const TRACKING_NAMESPACE: &str = "refs/remotes/o";

/// The `<old>` value that makes `update-ref` a create rather than a swap.
///
/// Forty zeros, per C-038. A SHA-256 index repository would need sixty-four, and
/// that is a recorded residual rather than a covered case: such a repository
/// cannot be fetched into a SHA-1 workspace at all, so the mismatch fails loudly
/// one step earlier, at the fetch.
const ZERO_OID: &str = "0000000000000000000000000000000000000000";

/// Where a commit's file contents are written before `hash-object` reads them.
///
/// Inside the work tree, so the path handed to `git hash-object` is relative and
/// inside the repository — the one placement no `git` argues with. Nothing ever
/// adds it to the index: the commit is built from `read-tree` plus
/// `update-index --cacheinfo`, neither of which looks at the work tree.
const STAGING_DIRECTORY: &str = ".ocx-staging";

/// The bounded schedule the merge-request confirmation poll runs on.
///
/// The ADR's `1, 2, 4, 8, 15`, giving up at ~30s of wall clock, expressed as a
/// configuration literal over the **existing** [`backoff_delays`] (DV-7). No
/// second clock is minted in this module, and the delays are assertable without
/// sleeping.
///
/// `request_timeout` is inert here and is deliberately left at the shared
/// default: the probe is the caller's REST read and its own client already
/// carries a timeout, so a different value would read as a bound this module
/// applies and does not.
pub const CONFIRMATION_SCHEDULE: PollSchedule = PollSchedule {
    initial_interval: Duration::from_secs(1),
    max_interval: Duration::from_secs(30),
    deadline: Duration::from_secs(30),
    request_timeout: super::poll::DEFAULT_REQUEST_TIMEOUT,
};

/// The `http.<prefix>` scope a credential injected for `remote` may be used
/// under.
///
/// C-034's rule lives in [`CredentialScope::new`]; this is only the derivation
/// that feeds it — the remote URL with any trailing slash removed. Nothing else
/// is stripped, and in particular a trailing `.git` stays: git matches
/// `http.<url>.*` component-wise, so `…/index` is **not** a prefix of
/// `…/index.git`, and a helpfully-shortened scope would stop applying silently,
/// leaving the push unauthenticated rather than over-scoped.
///
/// Host case and an explicit port pass through untouched. Git normalises both
/// itself when it matches a configuration URL against a request URL, so
/// re-implementing that here would be a second normaliser free to disagree with
/// the one that decides.
///
/// # Errors
///
/// Returns [`ForgeError::GitUnavailable`] when `remote` names no project — see
/// [`CredentialScope::new`], which fails closed rather than injecting the
/// credential across a whole host or group.
pub fn credential_scope(remote: &str) -> Result<CredentialScope, ForgeError> {
    CredentialScope::new(remote.trim_end_matches('/'))
}

/// Wait for the merge request the push asked the server to create.
///
/// A push carrying merge-request push options returns as soon as the ref is
/// written; the server creates the request **asynchronously**, so reading it
/// back once races the server and reports "no request" on a healthy run.
///
/// `probe` performs the REST read — one `Ok(None)` means "not yet", not
/// "never". It is a parameter rather than a method on the workspace because the
/// workspace speaks `git` and nothing else: keeping the REST call on the
/// caller's side is what stops a clone directory from growing an HTTP client.
/// It is a free function rather than a forge method because the *schedule* is
/// the contract being pinned, and it belongs beside the push it confirms.
///
/// The schedule is [`backoff_delays`] over [`CONFIRMATION_SCHEDULE`] — no second
/// clock is minted here, and the delays are asserted without sleeping.
///
/// # Errors
///
/// Returns [`ForgeError::MergeRequestUnconfirmed`] when the schedule is
/// exhausted with no request found — the push already succeeded, so the remedy
/// is a rerun, not a retry of the write. **A forge-side 5xx raised inside this
/// poll reaches the caller as that same variant**, for the reason the variant
/// already states: by the time the poll runs, the ref is written. Reporting a
/// 5xx here as [`ForgeError::Status`] would classify to `Unavailable` (69) and
/// tell a CI wrapper the forge was down and the run never happened, when in
/// fact the branch is published and rerunning is exactly the right recovery —
/// which is `MergeRequestUnconfirmed`'s (75) whole meaning. Scoped to the poll:
/// a 5xx on any read *before* the push keeps 69, because there the run really
/// did not happen. Every other error `probe` returns propagates unchanged, a 429
/// included — it already classifies to the same 75.
///
/// `Fn() -> Fut`, deliberately, and **not** `impl AsyncFn()`. `AsyncFn`'s
/// associated future is higher-ranked over the call lifetime, so a probe that
/// borrows its forge cannot be proved `Send` for *every* lifetime — which is
/// what `async_trait`'s boxed `Send` future demands of the GitLab client that
/// calls this. The compiler reports it as "implementation of `Send` is not
/// general enough" on the whole trait method, with no mention of this bound.
/// A plain `Fn` returning one concrete future has no such obligation.
pub async fn confirm_merge_request<Probe, Fut>(probe: Probe) -> Result<PullRequest, ForgeError>
where
    Probe: Fn() -> Fut,
    Fut: Future<Output = Result<Option<PullRequest>, ForgeError>>,
{
    // The first read happens before the first delay: the common case is a server
    // that has already run its post-receive worker, and a poll that slept first
    // would add a second to every healthy claim. It goes through the same
    // fault-folding attempt as the loop's — a 5xx on the very first read is the
    // one an unfolded version would let escape as a 69.
    if let Some(found) = confirmation_attempt(&probe).await? {
        return Ok(found);
    }
    for delay in backoff_delays(&CONFIRMATION_SCHEDULE) {
        tokio::time::sleep(delay).await;
        if let Some(found) = confirmation_attempt(&probe).await? {
            return Ok(found);
        }
    }
    Err(ForgeError::MergeRequestUnconfirmed {
        deadline_secs: CONFIRMATION_SCHEDULE.deadline.as_secs(),
    })
}

/// One confirmation read, with a forge-side fault folded into "not yet".
///
/// The fold is deliberate swallowing, and the debug line is what keeps it from
/// being silent: the error is not returned, so logging it here is the one place
/// it can be observed at all. Folding rather than returning immediately is also
/// what makes a *transient* incident recoverable inside the deadline — an
/// instance that 5xxes once and then answers still confirms the request, where a
/// version that gave up on the first fault would report an unconfirmed push the
/// server had in fact already created.
///
/// Narrow on purpose. Only a 5xx is folded: a 401 mid-poll means the credential
/// died and the operator must hear that, and a 404 is the probe's own "not
/// found" rather than an error at all.
async fn confirmation_attempt<Probe, Fut>(probe: &Probe) -> Result<Option<PullRequest>, ForgeError>
where
    Probe: Fn() -> Fut,
    Fut: Future<Output = Result<Option<PullRequest>, ForgeError>>,
{
    match probe().await {
        Err(error) if matches!(&error, ForgeError::Status { status, .. } if is_server_fault(*status)) => {
            tracing::debug!(%error, "the merge-request confirmation read faulted; the push already landed, so this is 'not yet'");
            Ok(None)
        }
        other => other,
    }
}

/// What a rejected `git push` needs before it can be named.
///
/// Grouped rather than passed as two more parameters so [`GitWorkspace::push`]
/// stays inside clippy's argument bound, and because the two travel together or
/// not at all: `super::git_stderr::classify_push_failure` reads the preflight for
/// exactly one row and the repository path for exactly one message.
///
/// **Neither is derivable inside the workspace.** The preflight is the forge's,
/// run before the clone exists, and the repository is a forge coordinate while
/// the workspace holds only a URL — so a push that classified from what it had
/// would answer [`ForgeError::PushRefused`] (77) on the exact input C-044
/// requires [`ForgeError::WriteCapabilityUnavailable`] (86) for, silently.
pub struct RefusalContext<'a> {
    /// The write preflight this run already performed.
    pub preflight: &'a PushAccess,
    /// The index project's `namespace/project` path, for the refusal message.
    pub repo: &'a str,
}

/// The credential this workspace injects, and everything derived from it once.
///
/// The derivation happens at [`GitWorkspace::open`] rather than per invocation so
/// the injected header and the redactor's secret list are fed from one value and
/// cannot disagree about what the secret is — C-022's agreement, held by
/// construction.
struct Injected {
    /// The `http.<prefix>` scope the header is placed under.
    scope: CredentialScope,
    /// The pair the header is built from.
    credential: GitPushCredential,
    /// Every live form of the secret, for [`redact`].
    ///
    /// Two, not one: the plaintext secret, and the base64 `user:secret` blob it
    /// exists as on the wire, which no plaintext needle matches. The API
    /// credential is not a third — C-063's ladder makes
    /// [`GitPushCredential::secret`] whichever of the two credentials applies, so
    /// there is no second secret in play on this path.
    secrets: Vec<String>,
}

impl Injected {
    fn new(remote: &str, credential: &GitPushCredential) -> Result<Self, ForgeError> {
        let authorization = credential.basic_authorization();
        let encoded = authorization
            .strip_prefix("Basic ")
            .unwrap_or(authorization.as_str())
            .to_string();
        Ok(Self {
            scope: credential_scope(remote)?,
            credential: credential.clone(),
            secrets: vec![credential.secret.0.clone(), encoded],
        })
    }
}

/// A temporary blobless clone of the index repository.
///
/// The working directory is a temporary directory owned by this value, so it is
/// removed when the workspace is dropped — including on the paths that unwind.
/// No `Drop` body of its own: the temporary-directory guard already does the
/// removal, and a hand-written `Drop` here would be a place for a panic to
/// happen during unwinding, which aborts.
pub struct GitWorkspace {
    /// The `git` the argv-boundary gate resolved and version-checked.
    git: GitBinary,
    /// The clone, and the guard that removes it.
    directory: TempDir,
    /// The remote every fetch and the push name. Never carries a credential.
    remote: String,
    /// `None` leaves git's own credential helpers in charge (C-063 rung three).
    injection: Option<Injected>,
}

// No block-level `expect(dead_code)` any more: deleting `has_unpushed_commit`
// left every method on this impl with a production caller, and an unfulfilled
// `expect` is a hard error under `warnings = "deny"`. Its absence is now the
// check — a method added here without a caller reds the build.
impl GitWorkspace {
    /// Clone `remote` into a fresh temporary directory and fetch what the run
    /// will read.
    ///
    /// The fetch is blobless (`--filter=blob:none`) and never shallow: the
    /// commit chain has to be complete for an exact ancestry comparison, while
    /// the file contents of unrelated packages are never read. `branch` is named
    /// as a second refspec **only** when the branch-existence read found one —
    /// fetching a ref that does not exist fails the whole fetch.
    ///
    /// `credential` `None` leaves git's own credential helpers in charge; `Some`
    /// injects the pair and resets `credential.helper` for exactly the
    /// invocations that carry it.
    ///
    /// **This parameter is the only channel by which a secret enters the
    /// workspace, and one value is sufficient for all three forms C-022 names.**
    /// Worth stating here because the argument is spread across three contracts
    /// and an implementer would otherwise have to reassemble it: C-063's ladder
    /// makes [`GitPushCredential::secret`] whichever of the two credentials
    /// applies, so there is no second secret to pass; the base64
    /// `Authorization` blob is *derived* from `username:secret` at injection
    /// rather than supplied, so it is not an input to **this parameter**; and
    /// C-035 strips every `OCX_*` variable from the child environment, so
    /// nothing arrives out of band. That derived blob is still a secret,
    /// though: [`Injected::secrets`] carries it alongside the plaintext so both
    /// reach [`redact`], or an unmasked `Authorization` header would survive
    /// into [`ForgeError::GitPushFailed`]'s `stderr` — exactly the third of
    /// C-022's three forms, recoverable back to `user:secret`. Widening this
    /// signature to carry a second secret would mean one of those three stopped
    /// holding — say so if it does, rather than adding a parameter.
    ///
    /// # Errors
    ///
    /// Returns [`ForgeError::GitCommandFailed`] when a plumbing step fails, or
    /// [`ForgeError::GitUnavailable`] when the workspace cannot be created at
    /// all — which includes a `remote` naming no project, refused **before** any
    /// directory exists and before any process starts.
    pub async fn open(
        git: &GitBinary,
        remote: &str,
        base: &str,
        branch: Option<&str>,
        credential: Option<&GitPushCredential>,
    ) -> Result<Self, ForgeError> {
        // First, and deliberately: a scope too wide is refused before a
        // directory is created and before `git` is started, so the fail-closed
        // half of C-034 costs nothing and leaves nothing behind.
        let injection = credential
            .map(|credential| Injected::new(remote, credential))
            .transpose()?;

        let directory = tempfile::Builder::new()
            .prefix("ocx-forge-git-")
            .tempdir()
            .map_err(|error| ForgeError::GitUnavailable {
                reason: format!("the git workspace directory could not be created: {error}"),
            })?;
        // Explicitly, not left to the umask (CWE-732) — and Unix only, because
        // Windows has no mode bits and a `set_permissions` there is inert: an
        // assertion written against it would pass whether or not anything
        // happened. The Windows protection is the per-user temporary directory's
        // own ACL, which ocx does not modify.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).map_err(|error| {
                ForgeError::GitUnavailable {
                    reason: format!("the git workspace directory could not be made private: {error}"),
                }
            })?;
        }

        // Built now so every fallible step below runs with the guard in scope: an
        // early return drops `workspace`, which drops the directory.
        let workspace = Self {
            git: git.clone(),
            directory,
            remote: remote.to_string(),
            injection,
        };
        workspace.run_local("init", &[], &["."]).await?;
        workspace.fetch_refs(base, branch).await?;
        Ok(workspace)
    }

    /// Re-fetch `base` and `branch` after a rejected ref update.
    ///
    /// Always names **both** refspecs: a rejection proves the branch exists now,
    /// whatever the branch-existence read reported before the first attempt.
    ///
    /// # Errors
    ///
    /// Returns [`ForgeError::GitCommandFailed`] when the fetch fails.
    pub async fn fetch(&self, base: &str, branch: &str) -> Result<(), ForgeError> {
        self.fetch_refs(base, Some(branch)).await
    }

    /// How `branch` stands relative to `base`, computed from the clone.
    ///
    /// Exact, not approximate: it counts commits on each side rather than
    /// inferring a relation. Never called when the branch does not exist — there
    /// is no second ref to compare.
    ///
    /// **Orientation, because it is the one thing a reader can get backwards.**
    /// `git rev-list --left-right --count <base>...<branch>` prints
    /// `<base-only>\t<branch-only>`, so `0 n` is [`BranchComparison::Ahead`] and
    /// `n 0` is [`BranchComparison::Behind`] — measured against git 2.54.0, and
    /// the orientation C-037 now states, after DX-36 corrected the inverted
    /// example notation that contract shipped with.
    /// Reading that notation literally produces a build that rebuilds a branch
    /// which was merely ahead and fast-forwards onto one that was left behind.
    ///
    /// # Errors
    ///
    /// Returns [`ForgeError::GitCommandFailed`] when the comparison fails or its
    /// output cannot be read.
    pub async fn compare(&self, base: &str, branch: &str) -> Result<BranchComparison, ForgeError> {
        let range = format!("{TRACKING_NAMESPACE}/{base}...{TRACKING_NAMESPACE}/{branch}");
        let counted = self
            .run_local("rev-list", &["--left-right", "--count"], &[&range])
            .await?;
        let mut counts = counted.split_whitespace().map(str::parse::<u64>);
        let (Some(Ok(base_only)), Some(Ok(branch_only))) = (counts.next(), counts.next()) else {
            return Err(self.unreadable("rev-list", &counted));
        };
        Ok(match (base_only, branch_only) {
            (0, 0) => BranchComparison::Identical,
            (0, _) => BranchComparison::Ahead,
            (_, 0) => BranchComparison::Behind,
            _ => BranchComparison::Diverged,
        })
    }

    /// Build one commit carrying every file and move the **local** ref,
    /// returning the new commit sha.
    ///
    /// Writes nothing to the network. The ref move is a local compare-and-swap
    /// against the branch's current value, so a [`RefUpdate::FastForward`] whose
    /// base moved surfaces as [`ForgeError::NonFastForward`] here rather than at
    /// the push.
    ///
    /// The chain is `hash-object` per file, `read-tree` of the parent,
    /// `update-index --cacheinfo` per file, `write-tree`, `commit-tree` — never
    /// `mktree`, which builds one flat tree and would need a hand-rolled
    /// `ls-tree`/`mktree` per level for the three-component path a claim writes.
    ///
    /// The parent is chosen exactly as the REST arm chooses `start_sha`: the
    /// index base when the branch does not exist yet or when a spent branch is
    /// being rebuilt, and the branch's own head when accumulating onto a live
    /// one. Reading `base.sha` unconditionally would drop every tag the branch
    /// already carried.
    ///
    /// **Accumulating, that head must be the base the caller read.** The files
    /// it hands over were regenerated from one commit's root, and laying them on
    /// a different commit's tree deletes whatever that commit added — silently,
    /// because parenting on the remote head makes the push a genuine
    /// fast-forward and no refusal is ever raised ([#436]). A head this run did
    /// not read is therefore [`ForgeError::NonFastForward`], the same answer a
    /// push race already gets, so the caller re-reads it and rebuilds on it.
    ///
    /// The REST arms need no such check, each for its own reason, and neither
    /// is this one — the guarantee below is the git workspace's alone. GitHub
    /// builds every commit on the base tree with `base.sha` as its parent
    /// (`github.rs`, `commit_files_once`) and updates the ref with
    /// `force: false`, so a branch that moved is not a descendant and the ref
    /// update refuses. GitLab does parent on the branch head once the branch
    /// exists — `start_sha`/`start_project` are sent only when it does not, or
    /// under `Reset` — but every file action carries the `last_commit_id` read
    /// at `base.sha`, and a head whose root was written by another commit fails
    /// that per-file compare-and-swap. This is the only place a commit can end
    /// up parented on something the caller never saw *and* be accepted.
    ///
    /// [#436]: https://github.com/ocx-sh/ocx/issues/436
    ///
    /// # Errors
    ///
    /// Returns [`ForgeError::NonFastForward`] when the local ref moved under a
    /// fast-forward-only update, or when the branch head a fast-forward would
    /// parent on is not the base the caller read; [`ForgeError::GitCommandFailed`]
    /// when a plumbing step fails.
    pub async fn commit_files(
        &self,
        branch: &str,
        base: CommitBase<'_>,
        message: &str,
        files: &BTreeMap<String, Vec<u8>>,
        update: RefUpdate,
    ) -> Result<String, ForgeError> {
        // Read **before** anything is built: this is the value the compare-and-swap
        // is made against, so a ref that moves while the objects are being written
        // is caught. Re-reading it at the update instead would compare the ref with
        // itself and pass in every state — a swap that guards nothing.
        let expected = self.resolve(&format!("refs/heads/{branch}")).await?;
        let head = self.resolve(&self.tracking(branch)).await?;
        let parent = match (update, head) {
            // The caller read its root at `base.sha`; a head that is not that
            // commit carries content the root was never derived from. See the
            // doc comment above — this is #436, and it is silent without the arm.
            (RefUpdate::FastForward, Some(head)) if head != base.sha => {
                return Err(ForgeError::NonFastForward {
                    branch: branch.to_string(),
                });
            }
            (RefUpdate::FastForward | RefUpdate::Accumulate, Some(head)) => head,
            (RefUpdate::Reset, _) | (RefUpdate::FastForward | RefUpdate::Accumulate, None) => base.sha.to_string(),
        };

        let staging = self.repository().join(STAGING_DIRECTORY);
        tokio::fs::create_dir_all(&staging)
            .await
            .map_err(|error| self.write_failed(STAGING_DIRECTORY, &error))?;

        let mut staged = Vec::with_capacity(files.len());
        for (position, (path, contents)) in files.iter().enumerate() {
            // The staged name is the position, never the caller's path: the file
            // is read once by `hash-object` and its name reaches no tree, so
            // nothing is gained by reproducing a path the index entry carries
            // anyway — and a caller's path is the one input that could climb out
            // of the staging directory.
            tokio::fs::write(staging.join(position.to_string()), contents)
                .await
                .map_err(|error| self.write_failed(path, &error))?;
            staged.push(format!("{STAGING_DIRECTORY}/{position}"));
        }

        let blobs = self.hash_staged_files(&staged).await?;
        self.run_local("read-tree", &[], &[&parent]).await?;
        if !files.is_empty() {
            // One `update-index` carrying every entry, not one per file. git
            // applies repeated `--cacheinfo` in order (measured against 2.54.0),
            // and each entry is the *value* of a flag rather than a positional —
            // which is why this call has no `--end-of-options` and needs none.
            //
            // git reads `<mode>,<object>,<path>` by taking everything after the
            // second comma as the path, so a comma in the path is carried
            // verbatim and needs no escaping of ocx's own. That is the whole
            // premise this format carries, and it is unchanged by the batching:
            // the `--index-info` form that would have been the other way to batch
            // is tab-delimited and *would* have added a no-tab-in-path premise,
            // which is why it is not used.
            let entries = self.index_entries(files, &blobs)?;
            let mut flags = Vec::with_capacity(entries.len() + 1);
            flags.push("--add");
            flags.extend(entries.iter().map(String::as_str));
            self.run_local("update-index", &flags, &[]).await?;
        }
        // `--missing-ok` because the fetch is blobless (C-036): every entry here
        // either came from the server's own base tree or was just written by
        // `hash-object -w`, so an absent blob is the expected state of this
        // checkout rather than an inconsistency, and the tree names only objects
        // the remote already has or the push carries. Without it `write-tree`
        // verifies each entry exists, and in a partial clone that check *fetches*
        // — a network dial from a local command that holds no credential.
        let tree = self.run_local("write-tree", &["--missing-ok"], &[]).await?;
        let new = self.commit_tree(&tree, &parent, message).await?;
        self.move_ref(branch, &new, expected.as_deref()).await?;
        Ok(new)
    }

    /// Append a commit carrying the same tree and a fresh committer timestamp,
    /// returning its sha.
    ///
    /// A server only processes push options for a push that genuinely advances a
    /// ref, so a run with nothing to say still needs the ref to move before it
    /// can ask for a merge request.
    ///
    /// # Errors
    ///
    /// Returns [`ForgeError::GitCommandFailed`] when a plumbing step fails, or
    /// when the refreshed commit is not new — see the one-second retry below.
    pub async fn refresh_commit(&self, branch: &str, message: &str) -> Result<String, ForgeError> {
        let expected = self.resolve(&format!("refs/heads/{branch}")).await?;
        let head = match expected.clone() {
            Some(head) => head,
            None => self
                .resolve(&self.tracking(branch))
                .await?
                .ok_or_else(|| self.unreadable("rev-parse", &format!("branch {branch} has no head to refresh")))?,
        };
        // `--verify` is load-bearing, not tidiness. A bare `git rev-parse` echoes
        // every argument it does not recognise as a revision, so with the argv
        // builder's `--end-of-options` in front of the revision its stdout is two
        // lines — the separator, then the sha — and the whole blob then reaches
        // `commit-tree` as a tree name. Measured against git 2.54.0;
        // `--verify` suppresses the echo and makes a non-revision an error.
        let tree = self
            .run_local("rev-parse", &["--verify"], &[&format!("{head}^{{tree}}")])
            .await?;

        let mut refreshed = self.commit_tree(&tree, &head, message).await?;
        if refreshed == head {
            // Same tree, same parent, same message, same fixed identity: the
            // committer date is the *only* varying input, and git stamps it at
            // one-second granularity — so a refresh landing inside the same
            // second as the commit it refreshes reproduces that commit's sha
            // exactly, the ref does not move, and the push the caller is about to
            // make carries nothing for the server to attach options to.
            // ponytail: one second and one retry; if this ever needs to be
            // cheaper, `GIT_COMMITTER_DATE` is the lever, and it is an allowlist
            // row in `git_command.rs` rather than a change here.
            tokio::time::sleep(Duration::from_secs(1)).await;
            refreshed = self.commit_tree(&tree, &head, message).await?;
        }
        if refreshed == head {
            return Err(self.unreadable(
                "commit-tree",
                &format!("the refresh of {branch} reproduced {head} and the ref would not advance"),
            ));
        }
        self.move_ref(branch, &refreshed, expected.as_deref()).await?;
        Ok(refreshed)
    }

    /// Push `branch`, carrying the merge-request push options that ask the
    /// server to open the request.
    ///
    /// `lease` is the whole force story: `None` is an ordinary push, and `Some`
    /// carries the sha the branch was read at as a lease, so a rebuild that
    /// rewrites history still refuses to clobber a branch that moved
    /// underneath it. There is no unleased force.
    ///
    /// # Errors
    ///
    /// Returns [`ForgeError::PushOptionRefused`] **before any process starts**
    /// when a rendered option value carries something the wire forbids; then
    /// [`ForgeError::NonFastForward`], [`ForgeError::StaleLease`],
    /// [`ForgeError::PushRefused`] or
    /// [`ForgeError::WriteCapabilityUnavailable`] for a refusal the stderr
    /// classifier recognises, and [`ForgeError::GitPushFailed`] for one it does
    /// not.
    pub async fn push(
        &self,
        branch: &str,
        target: &str,
        title: &str,
        description: &str,
        lease: Option<&str>,
        refusal: RefusalContext<'_>,
    ) -> Result<(), ForgeError> {
        // Escaped, then rendered — and refused — first: nothing has been spawned
        // yet, so a hostile value never reaches a pkt-line and no ref moves.
        //
        // The escape is the description's alone. A claim body is multi-line
        // markdown and GitLab converts the two-character `\n` sequence back into
        // a newline before it stores the description, so the body travels inside
        // the wire alphabet instead of dying at its first LF. `target` is a ref
        // name and `title` is one line: escaping either would turn a newline
        // that must be **refused** into one silently carried.
        let description = escape_newlines(description);
        let options = render_push_options(target, title, &description)?;

        let leased = lease.map(|sha| format!("--force-with-lease={branch}:{sha}"));
        let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
        // Every flag before the positionals, which is what lets `--end-of-options`
        // sit between them. `git push` accepts `-o` ahead of the repository
        // (measured against 2.54.0); leaving the options where they used to be —
        // after the refspec — would put them past the separator and turn each one
        // into a refspec.
        let mut flags = Vec::with_capacity(1 + options.len() * 2);
        if let Some(leased) = &leased {
            flags.push(leased.as_str());
        }
        for option in &options {
            flags.push("-o");
            flags.push(option.as_str());
        }

        let argv = self.network_argv("push", &flags, &[self.remote.as_str(), refspec.as_str()]);
        let injection = self.injection();
        let output = self.spawn(&argv, injection.as_ref(), LazyFetch::Allow).await?;
        if output.status.success() {
            return Ok(());
        }
        // Uncapped on the way into the classifier: the phrases it matches are
        // written behind the server's own `remote:` banner, at the tail, and a
        // cap applied first would turn every recognised refusal into an
        // unclassified exit 1. Capping is the error boundary's.
        let stderr = redact(&String::from_utf8_lossy(&output.stderr), &self.secrets());
        Err(classify_push_failure(
            stderr,
            refusal.preflight,
            branch,
            refusal.repo,
            &self.remote,
        ))
    }

    // ── The recipe's shared parts ────────────────────────────────────────────

    /// The clone's root, which is also its work tree and the cwd of every
    /// invocation.
    fn repository(&self) -> &Path {
        self.directory.path()
    }

    /// The remote-tracking ref `branch` was fetched into.
    fn tracking(&self, branch: &str) -> String {
        format!("{TRACKING_NAMESPACE}/{branch}")
    }

    /// Every live form of the injected secret, for [`redact`].
    fn secrets(&self) -> Vec<&str> {
        self.injection.as_ref().map_or_else(Vec::new, |injected| {
            injected.secrets.iter().map(String::as_str).collect()
        })
    }

    /// The credential placement, borrowed for one invocation.
    fn injection(&self) -> Option<CredentialInjection<'_>> {
        self.injection.as_ref().map(|injected| CredentialInjection {
            url_prefix: &injected.scope,
            credential: &injected.credential,
        })
    }

    /// The argv every invocation shares, plus whatever `extra` the caller adds.
    ///
    /// One builder, so C-068's "every invocation, without exception" is a
    /// property of this function rather than a habit at eleven call sites. The
    /// redirect refusal is on all of them and not only on the two that touch the
    /// network: git's default is `followRedirects=initial`, so the **initial**
    /// request of a push *is* followed, and the push is the one invocation
    /// carrying the credential as an `http.<prefix>.extraHeader`.
    ///
    /// `core.symlinks=false` is defence-in-depth. The recipe materialises no work
    /// tree, so it is inert today; it is kept because it is free and because the
    /// next path that does check out would otherwise inherit a contributor-planted
    /// symlink.
    ///
    /// `http.lowSpeedLimit`/`http.lowSpeedTime` are the transport's deadline: a
    /// transfer moving under 1000 bytes/s for 60 consecutive seconds is aborted,
    /// so a black-holed or trickling remote cannot hang an announce forever.
    /// Deliberately a *stall* bound and not a total one — a genuinely large
    /// blobless fetch may run for minutes and must not be killed for being big.
    /// It rides `argv_with` for the same reason the redirect refusal does, and it
    /// is inert on the local invocations exactly as `core.symlinks` is. This is
    /// **not** the deferred `tokio::time::timeout`, which stays deferred and
    /// whose landing site is `git_command::run_git`.
    ///
    /// **The argv is split into flags and positionals so `--end-of-options` has
    /// somewhere unambiguous to go.** git's separator means "everything after
    /// this is a revision or a path", so it must sit after the last flag and
    /// before the first bare positional — and inferring that boundary from a flat
    /// argv is not possible here: `update-index --cacheinfo <entry>` takes its
    /// value as a separate argument that does not begin with `-`, and
    /// `push … -o <option>` writes flags *after* its positionals. Measured
    /// against git 2.54.0, a separator placed by the naive "first argument
    /// without a leading dash" rule breaks `commit-tree`
    /// (`fatal: must give exactly one tree`), `update-index` and `push`; placed
    /// structurally, all ten of the recipe's subcommands accept it.
    ///
    /// What it buys: a sha or ref that begins with `-` is then a bad object name
    /// rather than an option. Measured — `git read-tree --upload-pack=/bin/false`
    /// is parsed as an *unknown option* (exit 129), and with the separator it is
    /// `fatal: Not a valid object name`. The shape guard on the forge's own
    /// `commit.id` is the fix; this is the belt, and it covers every positional
    /// rather than the one field a reviewer thought of. Available since git 2.24,
    /// below this transport's 2.31 floor, so no capability check is needed.
    ///
    /// Emitted only when there is a positional to protect: `write-tree` and
    /// `update-index` have none, and a separator with nothing after it would be
    /// noise in a recorded argv.
    fn argv_with(extra: &[&str], command: &str, flags: &[&str], positionals: &[&str]) -> Vec<OsString> {
        let mut argv = Vec::with_capacity(9 + extra.len() + flags.len() + positionals.len());
        for flag in [
            "-c",
            "http.followRedirects=false",
            "-c",
            "core.symlinks=false",
            "-c",
            "http.lowSpeedLimit=1000",
            "-c",
            "http.lowSpeedTime=60",
        ] {
            argv.push(OsString::from(flag));
        }
        argv.extend(extra.iter().map(OsString::from));
        argv.push(OsString::from(command));
        argv.extend(flags.iter().map(OsString::from));
        if !positionals.is_empty() {
            argv.push(OsString::from("--end-of-options"));
        }
        argv.extend(positionals.iter().map(OsString::from));
        argv
    }

    /// The argv of a local plumbing invocation: no credential, and therefore no
    /// `credential.helper` reset.
    fn local_argv(command: &str, flags: &[&str], positionals: &[&str]) -> Vec<OsString> {
        Self::argv_with(&[], command, flags, positionals)
    }

    /// The argv of an invocation that talks to the remote.
    ///
    /// `-c credential.helper=` (an empty value resets the list) rides exactly the
    /// invocations that inject an ocx credential and only those — C-034's scope
    /// qualifier is load-bearing rather than a hedge: push-credential precedence
    /// rung three injects nothing and exists precisely so an operator's own
    /// helper can authenticate the push, so applying the reset there would break
    /// the ratified fallback instead of protecting anything ocx supplied.
    fn network_argv(&self, command: &str, flags: &[&str], positionals: &[&str]) -> Vec<OsString> {
        let reset: &[&str] = if self.injection.is_some() {
            &["-c", "credential.helper="]
        } else {
            &[]
        };
        Self::argv_with(reset, command, flags, positionals)
    }

    /// Fetch `base`, and `branch` when there is one, into the tracking namespace.
    ///
    /// Never `--depth`: a shallow clone lies about ahead/behind, and the whole
    /// point of the comparison is that it is exact. `--filter=blob:none` is what
    /// makes a complete commit chain affordable — the file contents of unrelated
    /// packages are never read.
    async fn fetch_refs(&self, base: &str, branch: Option<&str>) -> Result<(), ForgeError> {
        let base_refspec = format!("{base}:{}", self.tracking(base));
        let branch_refspec = branch.map(|branch| format!("{branch}:{}", self.tracking(branch)));
        let mut positionals = vec![self.remote.as_str(), base_refspec.as_str()];
        if let Some(branch_refspec) = &branch_refspec {
            // Only when the branch-existence read found one: `git fetch` fails the
            // *whole* invocation on a refspec source the remote does not have, so
            // an unconditional second refspec would make a first claim — the
            // command's headline use case — impossible.
            positionals.push(branch_refspec.as_str());
        }
        self.run_network("fetch", &["--filter=blob:none"], &positionals).await?;
        Ok(())
    }

    /// Write every staged file into the object store in one invocation, in the
    /// order they were named.
    ///
    /// `git hash-object` accepts many paths and prints one object name per line
    /// in argument order (measured against git 2.54.0), so the whole commit costs
    /// one process instead of one per file. That is the change that matters for
    /// announce, whose mature cascade writes a hundred-odd roots on every publish
    /// — a claim writes one file and gains nothing from it.
    ///
    /// `--no-filters` because `HOME` reaches the child by design (C-033), so an
    /// operator's `core.autocrlf` would otherwise rewrite the bytes of an index
    /// root on their way into the object store.
    ///
    /// ponytail: argv, not `--stdin-paths`. The staged names are ocx-generated
    /// decimal positions, so neither a newline nor a tab can appear in one, and
    /// argv needs no new stdin seam in `git_command`. The ceiling is `ARG_MAX` —
    /// about 2 MB on Linux against roughly 15 bytes per staged name, so tens of
    /// thousands of files; `--stdin-paths` plus `update-index --index-info` is
    /// the upgrade path if a commit ever approaches it, and it costs a
    /// no-tab-in-path premise the `--cacheinfo` form does not carry.
    ///
    /// The pairing back onto the caller's paths — and the arity that pairing
    /// depends on — is [`Self::index_entries`]'s, so it can be asserted without
    /// a `git` that misbehaves.
    ///
    /// # Errors
    ///
    /// Returns [`ForgeError::GitCommandFailed`] when the invocation fails.
    async fn hash_staged_files(&self, staged: &[String]) -> Result<Vec<String>, ForgeError> {
        if staged.is_empty() {
            return Ok(Vec::new());
        }
        let paths: Vec<&str> = staged.iter().map(String::as_str).collect();
        let hashed = self.run_local("hash-object", &["-w", "--no-filters"], &paths).await?;
        Ok(hashed
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect())
    }

    /// Pair each file's path with the object name `hash-object` printed for it,
    /// rendered as the `--cacheinfo` flags one `update-index` carries.
    ///
    /// **The arity check is the load-bearing part, and it is why this is a
    /// function rather than a loop inside the caller.** The pairing is positional
    /// — `files` is a `BTreeMap`, so its key order is the order the staged paths
    /// were written and therefore the order git was given them — so a short list
    /// commits each file's bytes under the *next* file's path. That is silent
    /// corruption of a published index root, and no assertion on the process
    /// count can see it.
    ///
    /// A real `git` cannot produce the short list: it exits non-zero on a path it
    /// cannot hash, so the caller fails first. That makes the arity refusal
    /// unfalsifiable *through* `git` — which is exactly why it lives here, where
    /// a test hands the mismatched list in directly. A guard whose red state is
    /// unreachable is not a guard.
    ///
    /// # Errors
    ///
    /// Returns [`ForgeError::GitCommandFailed`] when `blobs` does not name one
    /// object per file.
    fn index_entries(&self, files: &BTreeMap<String, Vec<u8>>, blobs: &[String]) -> Result<Vec<String>, ForgeError> {
        if blobs.len() != files.len() {
            return Err(self.unreadable(
                "hash-object",
                &format!(
                    "{} object names came back for {} staged files",
                    blobs.len(),
                    files.len()
                ),
            ));
        }
        let mut entries = Vec::with_capacity(files.len() * 2);
        for (path, blob) in files.keys().zip(blobs) {
            entries.push("--cacheinfo".to_string());
            entries.push(format!("100644,{blob},{path}"));
        }
        Ok(entries)
    }

    /// `commit-tree`, the one place a commit object is minted.
    ///
    /// The tree is the positional and the parent rides `-p`, which is the order
    /// `--end-of-options` forces: measured against git 2.54.0,
    /// `commit-tree --end-of-options <tree> -p <parent>` dies with
    /// `fatal: must give exactly one tree`, because everything after the
    /// separator — `-p` included — is read as a tree.
    async fn commit_tree(&self, tree: &str, parent: &str, message: &str) -> Result<String, ForgeError> {
        self.run_local("commit-tree", &["-p", parent, "-m", message], &[tree])
            .await
    }

    /// Move `refs/heads/<branch>` to `new` as a compare-and-swap.
    ///
    /// `expected` is the value the caller **read before it started building**, not
    /// the ref's value now: passing the current value would compare the ref with
    /// itself and succeed in every state. `None` becomes [`ZERO_OID`], the
    /// spelling that makes the update a *create* and refuses to overwrite a ref
    /// that appeared meanwhile.
    ///
    /// Every failure is reported as [`ForgeError::NonFastForward`], and that is a
    /// deliberate narrowing rather than an oversight: the workspace is private and
    /// throwaway, both object names are ones ocx just produced, and no other
    /// process holds its ref lock — so a mismatched `<old>` is the only failure
    /// this call can actually have.
    async fn move_ref(&self, branch: &str, new: &str, expected: Option<&str>) -> Result<(), ForgeError> {
        let reference = format!("refs/heads/{branch}");
        let old = expected.unwrap_or(ZERO_OID);
        let argv = Self::local_argv("update-ref", &[], &[&reference, new, old]);
        let output = self.spawn(&argv, None, LazyFetch::Refuse).await?;
        if output.status.success() {
            return Ok(());
        }
        Err(ForgeError::NonFastForward {
            branch: branch.to_string(),
        })
    }

    /// The sha `revision` names, or `None` when it names nothing.
    ///
    /// `--verify --quiet` is what makes "absent" an ordinary answer rather than an
    /// error: git prints nothing and exits non-zero. A genuinely broken repository
    /// reads as absent here too — an accepted residual, because the next step
    /// then fails loudly against the object it was handed.
    async fn resolve(&self, revision: &str) -> Result<Option<String>, ForgeError> {
        let argv = Self::local_argv("rev-parse", &["--verify", "--quiet"], &[revision]);
        let output = self.spawn(&argv, None, LazyFetch::Refuse).await?;
        if !output.status.success() {
            return Ok(None);
        }
        Ok(Some(String::from_utf8_lossy(&output.stdout).trim().to_string()))
    }

    /// Run a local plumbing step, returning its trimmed stdout.
    async fn run_local(&self, command: &str, flags: &[&str], positionals: &[&str]) -> Result<String, ForgeError> {
        let argv = Self::local_argv(command, flags, positionals);
        self.run(command, &argv, None, LazyFetch::Refuse).await
    }

    /// Run a step that talks to the remote, returning its trimmed stdout.
    async fn run_network(&self, command: &str, flags: &[&str], positionals: &[&str]) -> Result<String, ForgeError> {
        let argv = self.network_argv(command, flags, positionals);
        let injection = self.injection();
        self.run(command, &argv, injection.as_ref(), LazyFetch::Allow).await
    }

    async fn run(
        &self,
        command: &str,
        argv: &[OsString],
        injection: Option<&CredentialInjection<'_>>,
        lazy_fetch: LazyFetch,
    ) -> Result<String, ForgeError> {
        let output = self.spawn(argv, injection, lazy_fetch).await?;
        if !output.status.success() {
            return Err(self.command_failed(command, &output));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// The single seam every invocation in this file goes through.
    ///
    /// `super::git_command::run_git` is the only thing that starts a process on
    /// this path, and this module never holds a child-process builder — which is
    /// what the process firewall's one allowlist row for `git_command.rs` is
    /// worth.
    async fn spawn(
        &self,
        argv: &[OsString],
        injection: Option<&CredentialInjection<'_>>,
        lazy_fetch: LazyFetch,
    ) -> Result<std::process::Output, ForgeError> {
        let borrowed: Vec<&OsStr> = argv.iter().map(OsString::as_os_str).collect();
        run_git(&self.git, self.repository(), injection, lazy_fetch, &borrowed).await
    }

    /// Name a failed non-push invocation.
    ///
    /// A rejected credential is classified **before** the generic path, because
    /// the generic path is deliberately unclassified and exits 1 — and the
    /// published claim table promises exit 80 for "the credential was rejected
    /// (401/403)" in every mode but `--out`. Under this transport the first
    /// network call is the `fetch` at [`Self::open`], so a bad
    /// `OCX_ANNOUNCE_GIT_TOKEN` used to reach the operator as an opaque exit 1
    /// naming a plumbing step.
    ///
    /// The classifier reads the **uncapped** redacted text, for the same reason
    /// the push classifier does: `remote:` banners push the phrase toward the
    /// tail, and a cap applied first would silently destroy the input. The cap
    /// stays on the payload of the error that is actually built.
    ///
    /// The push does not come through here — it classifies through
    /// `git_stderr::classify_push_failure`, which asks the **same** table the
    /// same question before it reads its own, so a rejected credential earns 80
    /// on either path. What differs is one row: a 403 here is the credential
    /// being refused, while a 403 on a push is authenticated-then-forbidden and
    /// keeps its documented 77/86 verdict. That is the whole reason
    /// [`GitInvocation`] is a parameter rather than something inferred.
    fn command_failed(&self, command: &str, output: &std::process::Output) -> ForgeError {
        let stderr = redact(String::from_utf8_lossy(&output.stderr).trim(), &self.secrets());
        if let Some(rejected) = classify_remote_failure(&stderr, &self.remote, GitInvocation::Fetch) {
            return rejected;
        }
        ForgeError::GitCommandFailed {
            command: command.to_string(),
            status: output.status.to_string(),
            stderr: stderr.capped(),
        }
    }

    /// A step that ran but said something this module cannot read.
    fn unreadable(&self, command: &str, output: &str) -> ForgeError {
        ForgeError::GitCommandFailed {
            command: command.to_string(),
            status: "unreadable output".to_string(),
            stderr: redact(output, &self.secrets()).capped(),
        }
    }

    fn write_failed(&self, what: &str, error: &std::io::Error) -> ForgeError {
        ForgeError::GitUnavailable {
            reason: format!("the git workspace could not write {what}: {error}"),
        }
    }
}

/// The working directory, for the tests that must read what the recipe left in
/// it.
///
/// The module header claims nothing secret is ever written into the clone. As a
/// bare struct that claim has **no observation channel** — the temporary
/// directory removes itself on drop and nothing else can name it — so the claim
/// would ship unverifiable. This accessor is that channel and exists for no
/// other reason, which is why it is `#[cfg(test)]`: a production reader would be
/// a second, unreviewed way to reach the clone.
#[cfg(all(test, unix))]
impl GitWorkspace {
    pub fn directory(&self) -> &std::path::Path {
        self.repository()
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::ffi::OsStr;
    #[cfg(unix)]
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::forge::poll::backoff_delays;
    #[cfg(unix)]
    use crate::forge::{ForgeToken, Redacted};

    /// A pull request the confirmation poll can hand back.
    fn request(number: u64) -> PullRequest {
        PullRequest {
            number,
            html_url: format!("https://gitlab.example/acme/index/-/merge_requests/{number}"),
            updated: false,
        }
    }

    // ── C-041 / DV-7: the confirmation poll ──────────────────────────────────

    /// The poll runs on `forge::poll::backoff_delays` over one configuration
    /// literal, and that literal yields the ADR's `1, 2, 4, 8, 15`.
    ///
    /// Two halves, and only together do they mean anything. The first asserts
    /// the schedule — which a stub satisfies, because a `const` needs no
    /// implementation. The second drives the poll to exhaustion and asserts the
    /// **attempt count** the schedule implies (one probe, then one per delay)
    /// and the deadline the error reports, which is what makes the schedule
    /// load-bearing rather than decorative.
    ///
    /// `start_paused` is what keeps this clock-free: tokio auto-advances its
    /// timer when nothing is runnable, so the whole 30-second bound elapses in
    /// no wall time and the test asserts the schedule without sleeping.
    ///
    /// Reds on: a private constant with its own loop (the attempt count stops
    /// following `backoff_delays`); a schedule whose `deadline` is not 30s (the
    /// error's `deadline_secs` moves).
    #[tokio::test(start_paused = true)]
    async fn poll_uses_forge_poll_backoff_delays() {
        assert_eq!(
            backoff_delays(&CONFIRMATION_SCHEDULE),
            vec![
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
                Duration::from_secs(8),
                Duration::from_secs(15),
            ],
            "the confirmation schedule must be the ADR's 1, 2, 4, 8, 15"
        );

        let attempts = std::cell::Cell::new(0_usize);
        // A shared reference, copied into each probe future: `async move` would
        // otherwise move the counter itself out of the enclosing scope, and a
        // `Fn` probe must be callable more than once.
        let counter = &attempts;
        let outcome = confirm_merge_request(|| async move {
            counter.set(counter.get() + 1);
            Ok(None)
        })
        .await;

        assert_eq!(
            attempts.get(),
            backoff_delays(&CONFIRMATION_SCHEDULE).len() + 1,
            "one probe, then one more after each delay in the shared schedule"
        );
        assert!(
            matches!(outcome, Err(ForgeError::MergeRequestUnconfirmed { deadline_secs: 30 })),
            "an exhausted schedule reports the rerun remedy at its own deadline, got {outcome:?}"
        );
    }

    /// A request the server creates late, but inside the bound, is returned.
    ///
    /// S-022's healthy half: the push succeeded and the asynchronous worker had
    /// not finished when the first read landed.
    ///
    /// Reds on: reading once instead of polling (the first `Ok(None)` becomes
    /// the answer).
    #[tokio::test(start_paused = true)]
    async fn confirm_polls_until_the_request_appears() {
        let attempts = std::cell::Cell::new(0_usize);
        let counter = &attempts;
        let found = confirm_merge_request(|| async move {
            counter.set(counter.get() + 1);
            if counter.get() < 4 {
                Ok(None)
            } else {
                Ok(Some(request(7)))
            }
        })
        .await
        .expect("a request that appears inside the bound is returned");

        assert_eq!(found.number, 7);
        assert_eq!(attempts.get(), 4, "the poll stops at the first request it sees");
    }

    /// An error from the probe propagates unchanged rather than being retried
    /// into the deadline or reshaped into an unconfirmed report.
    #[tokio::test(start_paused = true)]
    async fn confirm_propagates_a_probe_error() {
        let outcome = confirm_merge_request(|| async move { Err(ForgeError::UsersApiUnavailable) }).await;
        assert!(
            matches!(outcome, Err(ForgeError::UsersApiUnavailable)),
            "a probe error is the caller's, not a confirmation failure, got {outcome:?}"
        );
    }

    /// A forge status the poll must answer with, and the answer it must give.
    ///
    /// The 5xx rows are the finding: by the time the poll runs the ref is
    /// written, so `Unavailable` (69) — "the forge is down, the run never
    /// happened" — is the one thing a CI wrapper must not be told. `429` and the
    /// non-5xx rows are the boundary either side of it: a rate limit is the
    /// caller's to see (it already carries 75 of its own), and a 401 mid-poll
    /// means the credential died, which the operator must hear.
    const CONFIRMATION_FAULTS: [(u16, bool); 6] = [
        (499, false),
        (500, true),
        (502, true),
        (503, true),
        (599, true),
        (429, false),
    ];

    fn forge_status(status: u16) -> ForgeError {
        ForgeError::Status {
            url: "https://gitlab.example/api/v4/projects/1/merge_requests".to_string(),
            status,
            detail: String::new(),
        }
    }

    /// A forge-side 5xx **inside the confirmation poll** ends as
    /// `MergeRequestUnconfirmed`, and every neighbouring status still propagates.
    ///
    /// The push has already landed when this poll runs, so the two errors say
    /// opposite things to a CI wrapper: `Status{5xx}` classifies to `Unavailable`
    /// (69) and means "the forge is down, nothing happened", while
    /// `MergeRequestUnconfirmed` classifies to `TempFail` (75) and means "the
    /// branch is published, rerun to pick up the request". Answering 69 tells a
    /// pipeline not to rerun the one case where rerunning is the whole recovery.
    ///
    /// The `429` and `499` rows are what stop this being a blanket
    /// swallow-everything: both keep propagating, so a widened predicate reds
    /// here rather than silently eating a rate limit or a client-side status.
    ///
    /// Reds on: deleting the fold (every 5xx row propagates as `Status`);
    /// widening it to `!status.is_success()` or to `is_retryable` (the 429 row
    /// stops propagating); folding on the loop's reads but not the first one
    /// (this drives every read to fault, so the pre-loop read is the one that
    /// escapes).
    #[tokio::test(start_paused = true)]
    async fn a_fault_inside_the_confirmation_poll_is_unconfirmed_not_unavailable() {
        for (status, folded) in CONFIRMATION_FAULTS {
            let outcome = confirm_merge_request(|| async move { Err(forge_status(status)) }).await;
            if folded {
                assert!(
                    matches!(outcome, Err(ForgeError::MergeRequestUnconfirmed { deadline_secs: 30 })),
                    "a {status} after the push must report the rerun remedy, got {outcome:?}"
                );
            } else {
                assert!(
                    matches!(&outcome, Err(ForgeError::Status { status: seen, .. }) if *seen == status),
                    "a {status} is not a forge-side fault and must reach the caller, got {outcome:?}"
                );
            }
        }
    }

    /// A 5xx that clears inside the deadline still confirms the request.
    ///
    /// The half that proves the fold is not "give up quietly": folding a fault
    /// into "not yet" keeps the schedule running, so a single-blip incident on an
    /// instance that then answers returns the merge request the server really
    /// created. A version that returned `MergeRequestUnconfirmed` on the first
    /// fault passes the test above and fails this one.
    ///
    /// Reds on: ending the poll at the first fault instead of continuing it.
    #[tokio::test(start_paused = true)]
    async fn a_transient_fault_still_confirms_inside_the_deadline() {
        let attempts = std::cell::Cell::new(0_usize);
        let counter = &attempts;
        let found = confirm_merge_request(|| async move {
            counter.set(counter.get() + 1);
            if counter.get() < 3 {
                Err(forge_status(503))
            } else {
                Ok(Some(request(11)))
            }
        })
        .await
        .expect("an instance that faults twice and then answers still confirms");

        assert_eq!(found.number, 11);
        assert_eq!(attempts.get(), 3, "the poll kept going through both faults");
    }

    // ── C-034 / S-040: the credential scope ──────────────────────────────────

    /// The `http.<prefix>.extraHeader` scope carries the whole project path, and
    /// a remote that names no project is refused rather than widened.
    ///
    /// The three normalisation shapes C-034 names are asserted as *derivation*
    /// results: a trailing slash is removed, an uppercase host and an explicit
    /// port are passed through untouched because git's own URL matcher folds the
    /// first and understands the second — re-implementing either here would be a
    /// second normaliser free to disagree with the one that decides. The
    /// end-to-end half, where a replayed `git` proves the match, is WP-17's; it
    /// needs an HTTP server this module has no business owning.
    ///
    /// A trailing `.git` deliberately survives: git matches `http.<url>.*`
    /// component-wise, so `…/index` is not a prefix of `…/index.git` and a
    /// shortened scope would stop applying silently.
    ///
    /// Reds on: stripping `.git`; lowercasing the host; dropping the port;
    /// accepting a host-only or group-only remote.
    #[test]
    fn extra_header_prefix_carries_the_project_path() {
        for (remote, expected) in [
            (
                "https://gitlab.example/acme/index.git",
                "https://gitlab.example/acme/index.git",
            ),
            (
                "https://gitlab.example/acme/index/",
                "https://gitlab.example/acme/index",
            ),
            ("https://GitLab.Example/acme/index", "https://GitLab.Example/acme/index"),
            (
                "https://gitlab.example:8443/acme/index",
                "https://gitlab.example:8443/acme/index",
            ),
            (
                "https://gitlab.example/acme/platform/tooling/index",
                "https://gitlab.example/acme/platform/tooling/index",
            ),
        ] {
            let scope = credential_scope(remote).expect("a remote naming a project is accepted");
            assert_eq!(scope.to_string(), expected, "scope derived from {remote}");
        }

        for widened in [
            "https://gitlab.example",
            "https://gitlab.example/",
            "https://gitlab.example/acme",
            "https://gitlab.example/acme/",
        ] {
            assert!(
                credential_scope(widened).is_err(),
                "{widened} names no project and must fail closed rather than scope the credential wider"
            );
        }
    }

    // ── The real-`git` fixture ───────────────────────────────────────────────

    /// The fixture drives a real `git` against a real repository over `file://`,
    /// through a shim that records every invocation.
    ///
    /// `#[cfg(unix)]` because the shim is a `#!/bin/sh` script and the mode bits
    /// it needs do not exist on Windows. The Windows half of this file is one
    /// `#[cfg(unix)]` branch (the 0700 mode), and it is pinned by review rather
    /// than by a test that cannot run — the shape `quality-core.md` calls a green
    /// that never ran.
    #[cfg(unix)]
    mod fixture {
        use std::os::unix::fs::PermissionsExt as _;

        use super::*;
        use crate::forge::git_command::{LazyFetch, probe_git_binary_at, run_git};

        /// The field separator the shim writes between record fields.
        const SEPARATOR: char = '\u{1f}';

        /// The durable base directory every fixture tree is created under.
        ///
        /// Deliberately **not** `TMPDIR`. This host sweeps `/tmp` on a timer,
        /// and a sweep landing mid-run deletes the recording shim that every
        /// invocation `exec`s — producing up to twenty failures whose membership
        /// differs from run to run and none of which is about the code under
        /// test. A suite whose result depends on a reaper's clock is not a check.
        ///
        /// The repository's own gitignored `.tmp/`, created on demand. Scoped to
        /// **this directory alone** rather than exported as a repo-local
        /// `TMPDIR`: that would move every other suite's temporary tree under a
        /// path with an `ocx.toml` above it and red
        /// `config::loader::tests::project_path_walk_without_git_or_ceiling_returns_none`
        /// and `project::hook::tests::load_returns_no_project_when_cwd_walk_misses`,
        /// which walk upward expecting to find none.
        ///
        /// ponytail: the `TempDir` still removes itself on drop, so nothing
        /// accumulates except after a SIGKILL — the same residue `/tmp` was
        /// leaving, in a directory git already ignores.
        /// This host's own `git`, resolved against the **process** `PATH`.
        ///
        /// Deliberately not `probe_git_binary`, which resolves against
        /// `crate::env::var("PATH")` — a value `crate::test::env` lets any test
        /// in this binary override *globally*. `git_command`'s
        /// `probe_git_binary_resolves_git_on_the_child_path` points that
        /// override at a temporary directory holding a `--version`-only shim,
        /// and its locals drop in reverse declaration order, so the directory is
        /// removed **before** the override is released. A fixture probing inside
        /// that window resolves a `git` that is already gone.
        ///
        /// Measured: `forge::git_workspace` plus that one sibling reds 7 of 24
        /// rows on one run and 10 on the next, a different subset each time,
        /// with `/bin/sh: /tmp/.tmpXXXXXX/git: No such file or directory`;
        /// either selection alone is green. That reads exactly like a temporary
        /// directory being reaped, which is how it was first diagnosed — the
        /// reaper is innocent, the shared override is not.
        fn host_git() -> PathBuf {
            which::which("git").expect("this suite needs a real git on PATH")
        }

        fn scratch_root() -> PathBuf {
            // `CARGO_MANIFEST_DIR` is `crates/ocx_lib`, so two levels up is the
            // workspace root — the same hop `launch.rs`'s tests already make.
            let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.tmp/forge-git-workspace");
            std::fs::create_dir_all(&root).expect("the durable scratch root");
            root
        }

        /// The environment names the shim reports, in order.
        const RECORDED_ENV: &[&str] = &[
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_KEY_0",
            "GIT_CONFIG_VALUE_0",
            "GIT_AUTHOR_NAME",
            "GIT_AUTHOR_EMAIL",
            "GIT_COMMITTER_NAME",
            "GIT_COMMITTER_EMAIL",
            "GIT_NO_LAZY_FETCH",
        ];

        /// One recorded `git` invocation.
        #[derive(Debug)]
        pub struct Invocation {
            /// argv without argv[0].
            pub args: Vec<String>,
            /// The recorded slice of the child environment.
            pub env: BTreeMap<String, String>,
        }

        impl Invocation {
            /// The first argument that is not a global `-c <name>=<value>` pair —
            /// the subcommand.
            pub fn subcommand(&self) -> &str {
                let mut rest = self.args.iter();
                while let Some(argument) = rest.next() {
                    if argument == "-c" {
                        rest.next();
                        continue;
                    }
                    return argument;
                }
                ""
            }

            pub fn carries(&self, needle: &str) -> bool {
                self.args.iter().any(|argument| argument == needle)
            }
        }

        pub struct Fixture {
            /// Makes every seeded commit a genuine change: `git commit` fails with
            /// an empty stderr when the index matches HEAD, and a fixture that
            /// re-wrote identical bytes would die there rather than in the
            /// assertion it was built for.
            nonce: std::cell::Cell<usize>,
            root: tempfile::TempDir,
            /// The host's own `git`, used to build and seed the remote.
            real: GitBinary,
            /// The recording wrapper the workspace under test is handed.
            pub shim: GitBinary,
            log: PathBuf,
        }

        impl Fixture {
            pub async fn new() -> Self {
                let real = probe_git_binary_at(&host_git())
                    .await
                    .expect("this suite needs a real git on PATH");
                let root = tempfile::Builder::new()
                    .prefix("ocx-wp13-")
                    .tempdir_in(scratch_root())
                    .expect("a scratch directory");
                let log = root.path().join("git.log");
                std::fs::write(&log, b"").expect("the invocation log");

                let shim_path = root.path().join("git-shim");
                let script = format!(
                    "#!/bin/sh\n{{\nprintf '%s\\037' {names}\nfor a in \"$@\"; do printf '%s\\037' \"$a\"; done\nprintf '\\n'\n}} >> '{log}'\nexec '{real}' \"$@\"\n",
                    names = RECORDED_ENV
                        .iter()
                        .map(|name| format!("\"{name}=${{{name}-}}\""))
                        .collect::<Vec<_>>()
                        .join(" "),
                    log = log.display(),
                    real = real.path.display(),
                );
                std::fs::write(&shim_path, script).expect("the recording shim");
                std::fs::set_permissions(&shim_path, std::fs::Permissions::from_mode(0o755))
                    .expect("the shim must be executable");

                let fixture = Self {
                    nonce: std::cell::Cell::new(0),
                    shim: GitBinary {
                        path: shim_path,
                        version: real.version,
                    },
                    real,
                    root,
                    log,
                };
                fixture.seed().await;
                fixture
            }

            fn path(&self, name: &str) -> PathBuf {
                self.root.path().join(name)
            }

            /// The remote as the workspace addresses it.
            ///
            /// `file://` and not a bare path: git refuses `--filter` against a
            /// remote whose name begins with `/` ("promisor remote name cannot
            /// begin with '/'"), so a plain path would make the blobless fetch
            /// C-036 requires unexercisable.
            pub fn remote(&self) -> String {
                format!("file://{}", self.path("remote").display())
            }

            /// Run the host's own `git`, asserting success. Never the shim: the
            /// log must hold the workspace's invocations and nothing else.
            pub async fn git(&self, workdir: &Path, args: &[&str]) -> String {
                let argv: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
                let output = run_git(&self.real, workdir, None, LazyFetch::Allow, &argv)
                    .await
                    .unwrap_or_else(|error| panic!("git {args:?} could not run: {error}"));
                assert!(
                    output.status.success(),
                    "git {args:?} failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                String::from_utf8_lossy(&output.stdout).trim().to_string()
            }

            /// Run the host's own `git`, reporting success rather than asserting
            /// it — for the one assertion that needs to see a `git` refusal.
            ///
            /// `LazyFetch::Refuse` because this is the *observer*: a probe that
            /// resolves a missing object by fetching it reports every object as
            /// present, which is the one answer that would make an absence
            /// assertion green in every state.
            pub async fn try_git(&self, workdir: &Path, args: &[&str]) -> bool {
                let argv: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
                run_git(&self.real, workdir, None, LazyFetch::Refuse, &argv)
                    .await
                    .is_ok_and(|output| output.status.success())
            }

            async fn seed(&self) {
                let root = self.root.path();
                self.git(root, &["init", "--bare", "--initial-branch=main", "remote"])
                    .await;
                let remote = self.path("remote");
                self.git(&remote, &["config", "uploadpack.allowFilter", "true"]).await;
                self.git(&remote, &["config", "receive.advertisePushOptions", "true"])
                    .await;

                self.git(root, &["init", "--initial-branch=main", "seed"]).await;
                let seed = self.path("seed");
                std::fs::write(seed.join("base.json"), b"{\"seed\":true}\n").expect("the seed file");
                self.git(&seed, &["add", "-A"]).await;
                self.git(&seed, &["-c", "commit.gpgsign=false", "commit", "-m", "base"])
                    .await;
                self.push_seed("main").await;
            }

            async fn push_seed(&self, branch: &str) {
                let seed = self.path("seed");
                let remote = self.path("remote");
                self.git(
                    &seed,
                    &[
                        "push",
                        "--force",
                        &remote.display().to_string(),
                        &format!("{branch}:{branch}"),
                    ],
                )
                .await;
            }

            /// Point `branch` at `start` in the seed repository, add `extra`
            /// commits on it, and publish it to the remote.
            pub async fn branch_at(&self, branch: &str, start: &str, extra: usize) -> String {
                let seed = self.path("seed");
                self.git(&seed, &["checkout", "-q", "-B", branch, start]).await;
                for _ in 0..extra {
                    self.nonce.set(self.nonce.get() + 1);
                    let nonce = self.nonce.get();
                    let name = format!("{branch}-{nonce}.json");
                    std::fs::write(seed.join(&name), format!("{{\"n\":{nonce}}}\n")).expect("a seed commit file");
                    self.git(&seed, &["add", "-A"]).await;
                    self.git(
                        &seed,
                        &[
                            "-c",
                            "commit.gpgsign=false",
                            "commit",
                            "-m",
                            &format!("{branch} {nonce}"),
                        ],
                    )
                    .await;
                }
                self.push_seed(branch).await;
                self.git(&seed, &["rev-parse", "HEAD"]).await
            }

            /// The remote's current value for `branch`.
            pub async fn remote_sha(&self, branch: &str) -> String {
                let remote = self.path("remote");
                self.git(&remote, &["rev-parse", &format!("refs/heads/{branch}")]).await
            }

            /// Every invocation the workspace has made so far, in order.
            pub fn invocations(&self) -> Vec<Invocation> {
                let recorded = std::fs::read_to_string(&self.log).expect("the invocation log");
                recorded
                    .lines()
                    .filter(|line| !line.is_empty())
                    .map(|line| {
                        let mut fields: Vec<&str> = line.split(SEPARATOR).collect();
                        // `printf '%s\037'` terminates every field, so the split
                        // leaves one trailing empty piece.
                        fields.pop();
                        let env = RECORDED_ENV
                            .iter()
                            .zip(fields.iter())
                            .map(|(name, field)| {
                                let value = field.strip_prefix(&format!("{name}=")).unwrap_or_default();
                                ((*name).to_string(), value.to_string())
                            })
                            .collect();
                        Invocation {
                            args: fields[RECORDED_ENV.len()..]
                                .iter()
                                .map(|field| (*field).to_string())
                                .collect(),
                            env,
                        }
                    })
                    .collect()
            }

            /// The invocations whose subcommand is `name`.
            pub fn calls(&self, name: &str) -> Vec<Invocation> {
                self.invocations()
                    .into_iter()
                    .filter(|invocation| invocation.subcommand() == name)
                    .collect()
            }
        }

        /// A credential whose secret is unmistakable in any text that carries it.
        pub fn credential() -> GitPushCredential {
            GitPushCredential {
                username: GitPushCredential::DEFAULT_USERNAME.to_string(),
                secret: ForgeToken::new("glpat-WP13-SECRET-VALUE".to_string()),
            }
        }

        /// One file at the three-component path a claim writes.
        pub fn claim_files() -> BTreeMap<String, Vec<u8>> {
            let mut files = BTreeMap::new();
            files.insert(
                "p/acme/widget.json".to_string(),
                b"{\"name\":\"ocx.sh/acme/widget\"}\n".to_vec(),
            );
            files
        }

        /// A preflight that reports nothing — the state every push in this module
        /// is made under, since the preflight is the forge's.
        pub fn preflight() -> PushAccess {
            PushAccess::skipped_all()
        }

        pub fn refusal(preflight: &PushAccess) -> RefusalContext<'_> {
            RefusalContext {
                preflight,
                repo: "acme/index",
            }
        }

        /// Every file under `root`, as bytes, for the "no secret anywhere"
        /// assertion.
        pub fn all_files(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
            let mut found = Vec::new();
            let mut pending = vec![root.to_path_buf()];
            while let Some(directory) = pending.pop() {
                let Ok(entries) = std::fs::read_dir(&directory) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        pending.push(path);
                    } else if let Ok(contents) = std::fs::read(&path) {
                        found.push((path, contents));
                    }
                }
            }
            found
        }
    }

    #[cfg(unix)]
    use fixture::{Fixture, all_files, claim_files, credential, preflight, refusal};

    // ── C-036: the fetch ─────────────────────────────────────────────────────

    /// A first claim has no branch, so the fetch names exactly one refspec.
    ///
    /// The unconditional two-refspec form is not merely wasteful: `git fetch`
    /// fails the **whole** invocation on a refspec source the remote does not
    /// have, so it would make the `Absent` state — the normal state for a first
    /// claim — unreachable.
    ///
    /// Reds on: naming the branch refspec unconditionally (the fetch dies with
    /// "couldn't find remote ref").
    #[cfg(unix)]
    #[tokio::test]
    async fn fetch_names_one_refspec_when_absent() {
        let fixture = Fixture::new().await;
        let workspace = GitWorkspace::open(&fixture.shim, &fixture.remote(), "main", None, None)
            .await
            .expect("a first claim opens against a branch that does not exist");

        let fetches = fixture.calls("fetch");
        assert_eq!(fetches.len(), 1, "opening fetches once");
        let refspecs: Vec<&String> = fetches[0]
            .args
            .iter()
            .filter(|a| a.contains(":refs/remotes/o/"))
            .collect();
        assert_eq!(
            refspecs,
            vec![&"main:refs/remotes/o/main".to_string()],
            "an absent branch is not named: {:?}",
            fetches[0].args
        );
        drop(workspace);
    }

    /// The fetch is blobless and never shallow.
    ///
    /// `--depth` is the defect this pins: a shallow clone lies about ahead/behind,
    /// so the four-way comparison would answer from a truncated history. The
    /// blobless filter is the other half — it is what makes the clone cheap
    /// enough to be worth taking at all.
    ///
    /// Reds on: dropping `--filter=blob:none`; adding any `--depth` form.
    #[cfg(unix)]
    #[tokio::test]
    async fn fetch_never_carries_depth() {
        let fixture = Fixture::new().await;
        let _workspace = GitWorkspace::open(&fixture.shim, &fixture.remote(), "main", None, None)
            .await
            .expect("open");

        for fetch in fixture.calls("fetch") {
            assert!(
                fetch.carries("--filter=blob:none"),
                "the fetch must be blobless: {:?}",
                fetch.args
            );
            assert!(
                !fetch.args.iter().any(|argument| argument.starts_with("--depth")),
                "no fetch may be shallow: {:?}",
                fetch.args
            );
        }
        assert!(
            !fixture
                .invocations()
                .iter()
                .any(|call| call.args.iter().any(|argument| argument.starts_with("--depth"))),
            "no invocation at all may be shallow"
        );
    }

    /// A retry re-fetches **both** refs, whatever the branch-existence read
    /// reported before the first attempt.
    ///
    /// A rejection proves the branch exists now, so the conditional second
    /// refspec of the first fetch is wrong here: without the branch the retry
    /// would re-derive its parent from a stale `o/<branch>` and never converge.
    ///
    /// Reds on: reusing the opening fetch's conditional refspec list.
    #[cfg(unix)]
    #[tokio::test]
    async fn retry_refetches_both_refs() {
        let fixture = Fixture::new().await;
        fixture.branch_at("claim", "main", 1).await;
        let workspace = GitWorkspace::open(&fixture.shim, &fixture.remote(), "main", None, None)
            .await
            .expect("open");

        workspace.fetch("main", "claim").await.expect("the retry fetch");

        let fetches = fixture.calls("fetch");
        let retry = fetches.last().expect("a retry fetch");
        let refspecs: Vec<&String> = retry.args.iter().filter(|a| a.contains(":refs/remotes/o/")).collect();
        assert_eq!(
            refspecs,
            vec![
                &"main:refs/remotes/o/main".to_string(),
                &"claim:refs/remotes/o/claim".to_string()
            ],
            "a retry always names both refspecs: {:?}",
            retry.args
        );
    }

    // ── C-068 / C-033: what every invocation carries ─────────────────────────

    /// Every git invocation carries `-c http.followRedirects=false`, without
    /// exception.
    ///
    /// Git's default is `followRedirects=initial`, so the **initial** request of
    /// a push is followed — and the push is the one invocation carrying the
    /// credential as an `http.<prefix>.extraHeader`. A flag applied only to the
    /// network calls would therefore still hand the header to a redirect target
    /// on the very call that matters.
    ///
    /// The assertion walks a run that exercises every builder in the file — init,
    /// fetch, rev-parse, rev-list, hash-object, read-tree, update-index,
    /// write-tree, commit-tree, update-ref and push — so "every" is a statement
    /// about the recipe and not about the two calls a shorter test happened to
    /// make.
    ///
    /// Reds on: removing the flag from the shared argv builder (every invocation
    /// fails at once).
    #[cfg(unix)]
    #[tokio::test]
    async fn every_git_invocation_carries_no_redirects() {
        let fixture = Fixture::new().await;
        fixture.branch_at("claim", "main", 1).await;
        let base = fixture.remote_sha("main").await;
        let credential = credential();
        let workspace = GitWorkspace::open(
            &fixture.shim,
            &fixture.remote(),
            "main",
            Some("claim"),
            Some(&credential),
        )
        .await
        .expect("open");

        workspace.compare("main", "claim").await.expect("compare");
        let repo = fixture.remote();
        let _ = repo;
        workspace
            .commit_files(
                "claim",
                CommitBase {
                    repo: &"acme/index"
                        .parse::<crate::forge::RepoCoordinate>()
                        .expect("a coordinate"),
                    sha: &base,
                    branch: "main",
                },
                "claim acme/widget",
                &claim_files(),
                RefUpdate::Accumulate,
            )
            .await
            .expect("the commit chain");
        let access = preflight();
        workspace
            .push(
                "claim",
                "main",
                "claim acme/widget",
                "owners: alice:1",
                None,
                refusal(&access),
            )
            .await
            .expect("the push");

        let invocations = fixture.invocations();
        assert!(
            invocations.len() >= 10,
            "the run must exercise the whole recipe, saw {}",
            invocations.len()
        );
        for invocation in &invocations {
            let pair = invocation
                .args
                .windows(2)
                .any(|window| window[0] == "-c" && window[1] == "http.followRedirects=false");
            assert!(
                pair,
                "every invocation carries the redirect refusal, {} did not: {:?}",
                invocation.subcommand(),
                invocation.args
            );
        }
    }

    /// No invocation recurses submodules, and every one disables symlinks.
    ///
    /// `--recurse-submodules` closes the one Clone2Leak vector host-scoping alone
    /// does not; `core.symlinks=false` is defence-in-depth for a path that
    /// materialises no work tree today and must not inherit a planted symlink the
    /// day one does.
    #[cfg(unix)]
    #[tokio::test]
    async fn no_invocation_recurses_submodules_and_every_one_disables_symlinks() {
        let fixture = Fixture::new().await;
        let _workspace = GitWorkspace::open(&fixture.shim, &fixture.remote(), "main", None, None)
            .await
            .expect("open");

        for invocation in fixture.invocations() {
            assert!(
                !invocation
                    .args
                    .iter()
                    .any(|argument| argument.contains("recurse-submodules")),
                "no invocation may recurse submodules: {:?}",
                invocation.args
            );
            assert!(
                invocation
                    .args
                    .windows(2)
                    .any(|window| window[0] == "-c" && window[1] == "core.symlinks=false"),
                "every invocation disables symlinks: {:?}",
                invocation.args
            );
        }
    }

    // ── C-037: the four-way comparison ───────────────────────────────────────

    /// `rev-list --left-right --count o/<base>...o/<branch>` maps to all four
    /// states.
    ///
    /// The orientation is the whole test. `--left-right` prints
    /// `<base-only>\t<branch-only>` for `base...branch`, so **`0 n` is `Ahead`
    /// and `n 0` is `Behind`** — measured against git 2.54.0. C-037 states this
    /// orientation today; it shipped with the pairing inverted and DX-36
    /// corrected it. Reading that original `n/0 → Ahead` literally produces a
    /// build that rebuilds a branch which was merely ahead and fast-forwards
    /// onto one that was left behind — which is why the orientation is asserted
    /// here rather than assumed.
    ///
    /// Reds on: swapping the two counts; comparing the refs in the other order.
    #[cfg(unix)]
    #[tokio::test]
    async fn compare_maps_four_way() {
        let fixture = Fixture::new().await;
        // `behind` and `diverged` are cut from the base as it stands now; the base
        // then moves, which is what leaves them behind it.
        let original = fixture.remote_sha("main").await;
        fixture.branch_at("behind", &original, 0).await;
        fixture.branch_at("diverged", &original, 1).await;
        fixture.branch_at("main", &original, 1).await;
        // `same` and `ahead` are cut from the base as it stands *after* the move.
        let moved = fixture.remote_sha("main").await;
        fixture.branch_at("same", &moved, 0).await;
        fixture.branch_at("ahead", &moved, 2).await;

        for (branch, expected) in [
            ("same", BranchComparison::Identical),
            ("ahead", BranchComparison::Ahead),
            ("behind", BranchComparison::Behind),
            ("diverged", BranchComparison::Diverged),
        ] {
            let fixture_remote = fixture.remote();
            let workspace = GitWorkspace::open(&fixture.shim, &fixture_remote, "main", Some(branch), None)
                .await
                .expect("open");
            assert_eq!(
                workspace.compare("main", branch).await.expect("compare"),
                expected,
                "branch {branch}"
            );
        }
    }

    // ── C-038: the commit chain ──────────────────────────────────────────────

    /// The commit is an index-file chain and it writes a three-component path.
    ///
    /// `git mktree` builds one flat tree, so `p/<namespace>/<package>.json` would
    /// need a hand-rolled `ls-tree`/`mktree` per level. The assertion is on the
    /// resulting tree — the nested path really exists with the bytes handed in —
    /// and on the argv, so a build that produced the right tree by some other
    /// route still reds.
    ///
    /// Reds on: reaching for `mktree`; committing the path flat.
    #[cfg(unix)]
    #[tokio::test]
    async fn commit_chain_writes_nested_path() {
        let fixture = Fixture::new().await;
        let base = fixture.remote_sha("main").await;
        let workspace = GitWorkspace::open(&fixture.shim, &fixture.remote(), "main", None, None)
            .await
            .expect("open");

        let coordinate = "acme/index"
            .parse::<crate::forge::RepoCoordinate>()
            .expect("a coordinate");
        let sha = workspace
            .commit_files(
                "claim",
                CommitBase {
                    repo: &coordinate,
                    sha: &base,
                    branch: "main",
                },
                "claim acme/widget",
                &claim_files(),
                RefUpdate::Accumulate,
            )
            .await
            .expect("the commit chain");

        let listed = fixture
            .git(workspace.directory(), &["ls-tree", "-r", "--name-only", &sha])
            .await;
        assert!(
            listed.lines().any(|line| line == "p/acme/widget.json"),
            "the nested path must exist in the tree, got {listed:?}"
        );
        let blob = fixture
            .git(
                workspace.directory(),
                &["cat-file", "-p", &format!("{sha}:p/acme/widget.json")],
            )
            .await;
        assert_eq!(blob, "{\"name\":\"ocx.sh/acme/widget\"}");

        let subcommands: Vec<String> = fixture
            .invocations()
            .iter()
            .map(|invocation| invocation.subcommand().to_string())
            .collect();
        for step in [
            "hash-object",
            "read-tree",
            "update-index",
            "write-tree",
            "commit-tree",
            "update-ref",
        ] {
            assert!(
                subcommands.contains(&step.to_string()),
                "the chain must run {step}: {subcommands:?}"
            );
        }
        assert!(
            !subcommands.contains(&"mktree".to_string()),
            "never mktree: {subcommands:?}"
        );
    }

    /// The commit chain survives a base-tree blob the blobless fetch never
    /// pulled — the defect behind [#428].
    ///
    /// `update-index` invalidates the cache-tree of every directory on the way
    /// to the root, so `write-tree` rebuilds those trees and re-verifies every
    /// entry under them, including the ones that came from `read-tree <base>`.
    /// In a `--filter=blob:none` checkout those blobs are promisor objects the
    /// clone does not hold, and git resolves a missing object by *fetching* it —
    /// from a local plumbing command that carries no credential. Against a real
    /// forge that dial is answered 401 and the classifier reports a rejected
    /// credential, naming a token that is fine.
    ///
    /// The premise is asserted before the behaviour: a base blob that is
    /// actually present would make the whole row green with the fix reverted.
    ///
    /// Reds on: dropping `--missing-ok` (the chain then dies at `write-tree`
    /// with `could not fetch <sha> from promisor remote`); or allowing the lazy
    /// fetch on the local lane, which silently resolves the object and makes the
    /// scenario unobservable — which is exactly why the suite missed it.
    ///
    /// [#428]: https://github.com/ocx-sh/ocx/issues/428
    #[cfg(unix)]
    #[tokio::test]
    async fn write_tree_tolerates_a_base_blob_the_blobless_fetch_never_pulled() {
        let fixture = Fixture::new().await;
        let base = fixture.remote_sha("main").await;
        let workspace = GitWorkspace::open(&fixture.shim, &fixture.remote(), "main", None, None)
            .await
            .expect("open");

        // The premise. `rev-parse` names the blob out of the *tree*, which the
        // filtered fetch did carry; `cat-file -e` is what needs the object
        // itself, and `try_git` refuses the lazy fetch so its answer is the
        // clone's real contents rather than the remote's.
        let seeded = fixture
            .git(workspace.directory(), &["rev-parse", &format!("{base}:base.json")])
            .await;
        assert!(
            !fixture
                .try_git(workspace.directory(), &["cat-file", "-e", &seeded])
                .await,
            "the blobless fetch must leave {seeded} absent, or this row proves nothing"
        );

        let coordinate = "acme/index"
            .parse::<crate::forge::RepoCoordinate>()
            .expect("a coordinate");
        workspace
            .commit_files(
                "claim",
                CommitBase {
                    repo: &coordinate,
                    sha: &base,
                    branch: "main",
                },
                "claim acme/widget",
                &claim_files(),
                RefUpdate::Accumulate,
            )
            .await
            .expect("the commit chain must not need a blob the base tree already has");

        let write_tree = fixture
            .invocations()
            .into_iter()
            .find(|invocation| invocation.subcommand() == "write-tree")
            .expect("the chain runs write-tree");
        assert!(
            write_tree.carries("--missing-ok"),
            "write-tree must tolerate the promisor objects: {:?}",
            write_tree.args
        );
    }

    /// `GIT_NO_LAZY_FETCH` rides every local invocation and no network one.
    ///
    /// The local half is the guard [#428] asks for: a plumbing command that
    /// resolves a missing object over the network turns a local failure into a
    /// transport one, and `git_stderr.rs` then reports an auth error for a
    /// credential that was never offered. With the fetch refused, the same state
    /// fails as `invalid object` in the command that needed it.
    ///
    /// The network half is why this cannot be a [`table::SET`] row: `fetch` and
    /// `push` generate packs against the promisor remote and legitimately depend
    /// on the behaviour it removes.
    ///
    /// Reds on: moving the name into `SET` (the two network invocations then
    /// carry it); deriving the lane from `injection.is_some()` (precedence rung
    /// three fetches with no injection and would be mislabelled local).
    ///
    /// [#428]: https://github.com/ocx-sh/ocx/issues/428
    #[cfg(unix)]
    #[tokio::test]
    async fn no_lazy_fetch_rides_the_local_invocations_only() {
        let fixture = Fixture::new().await;
        let base = fixture.remote_sha("main").await;
        let credential = credential();
        let workspace = GitWorkspace::open(&fixture.shim, &fixture.remote(), "main", None, Some(&credential))
            .await
            .expect("open");
        let coordinate = "acme/index"
            .parse::<crate::forge::RepoCoordinate>()
            .expect("a coordinate");
        workspace
            .commit_files(
                "claim",
                CommitBase {
                    repo: &coordinate,
                    sha: &base,
                    branch: "main",
                },
                "claim acme/widget",
                &claim_files(),
                RefUpdate::Accumulate,
            )
            .await
            .expect("the commit chain");

        let invocations = fixture.invocations();
        let mut locals = 0;
        let mut networks = 0;
        for invocation in &invocations {
            let refused = invocation.env.get("GIT_NO_LAZY_FETCH").map(String::as_str) == Some("1");
            if matches!(invocation.subcommand(), "fetch" | "push") {
                networks += 1;
                assert!(
                    !refused,
                    "{} generates a pack against the promisor remote: {:?}",
                    invocation.subcommand(),
                    invocation.args
                );
            } else {
                locals += 1;
                assert!(
                    refused,
                    "{} is local and must not dial out: {:?}",
                    invocation.subcommand(),
                    invocation.args
                );
            }
        }
        // Without these the walk is green over an empty list, which is the state
        // a recording shim that stopped reporting the name would also be in.
        assert!(
            networks > 0,
            "the run must contain a network invocation: {invocations:?}"
        );
        assert!(locals > 0, "the run must contain a local invocation: {invocations:?}");
    }

    /// A branch that does not exist yet is created by an `update-ref` whose
    /// `<old>` is forty zeros.
    ///
    /// That is what makes the local ref move a *create* rather than a swap: a
    /// non-zero `<old>` on an absent ref fails, and an omitted `<old>` would
    /// overwrite whatever was there.
    ///
    /// Reds on: omitting `<old>`; passing the branch's (absent) value.
    #[cfg(unix)]
    #[tokio::test]
    async fn update_ref_uses_zero_old_when_absent() {
        let fixture = Fixture::new().await;
        let base = fixture.remote_sha("main").await;
        let workspace = GitWorkspace::open(&fixture.shim, &fixture.remote(), "main", None, None)
            .await
            .expect("open");
        let coordinate = "acme/index"
            .parse::<crate::forge::RepoCoordinate>()
            .expect("a coordinate");
        workspace
            .commit_files(
                "claim",
                CommitBase {
                    repo: &coordinate,
                    sha: &base,
                    branch: "main",
                },
                "claim acme/widget",
                &claim_files(),
                RefUpdate::Accumulate,
            )
            .await
            .expect("the commit chain");

        let updates = fixture.calls("update-ref");
        let update = updates.last().expect("an update-ref");
        assert_eq!(
            update.args.last().map(String::as_str),
            Some("0000000000000000000000000000000000000000"),
            "an absent branch is created against the zero oid: {:?}",
            update.args
        );
        assert!(
            update.carries("refs/heads/claim"),
            "the local ref is the one moved: {:?}",
            update.args
        );
    }

    /// The local ref move is a real compare-and-swap: `<old>` is the value the
    /// commit was built on, never omitted, and `git` refuses a mismatch.
    ///
    /// Two halves, because neither alone is the contract. The first reads the
    /// recorded argv: the second commit swaps against the **first commit's** sha,
    /// so `<old>` is a value that was actually read rather than a placeholder, and
    /// it is present — an omitted `<old>` would overwrite whatever the ref held.
    /// The second half proves the mechanism the first relies on is not decoration:
    /// `update-ref` with a wrong `<old>` is refused by `git` itself.
    ///
    /// **Not the concurrency guard.** Under the git transport D-T4 puts the
    /// rejection of a concurrent writer at the push, where the server compares the
    /// refs — `push_to_a_moved_branch_classifies_non_fast_forward` is that test.
    /// What the local swap buys is the create case and the build window inside one
    /// call.
    ///
    /// Reds on: dropping `<old>` from the `update-ref` argv (both this test and
    /// `update_ref_uses_zero_old_when_absent` fail, on the present and absent
    /// halves respectively).
    ///
    /// **One mutation deliberately does not red it, and is recorded rather than
    /// papered over.** Reading the ref again at the update instead of before the
    /// build leaves the argv byte-identical here, because nothing moves the ref
    /// between the two reads in a single-threaded test — so *when* `<old>` is read
    /// is a design decision this suite cannot observe. Observing it would need two
    /// writers interleaved inside one `commit_files` call, which no deterministic
    /// test can arrange against a real subprocess chain, and which the git
    /// transport never performs. What the assertion above does hold is the half
    /// that is reachable: the value is present, and it is the branch's own
    /// previous commit rather than a placeholder.
    #[cfg(unix)]
    #[tokio::test]
    async fn update_ref_swaps_against_the_value_the_commit_was_built_on() {
        let fixture = Fixture::new().await;
        let base = fixture.remote_sha("main").await;
        let workspace = GitWorkspace::open(&fixture.shim, &fixture.remote(), "main", None, None)
            .await
            .expect("open");
        let coordinate = "acme/index"
            .parse::<crate::forge::RepoCoordinate>()
            .expect("a coordinate");
        let commit_base = CommitBase {
            repo: &coordinate,
            sha: &base,
            branch: "main",
        };

        let first = workspace
            .commit_files("claim", commit_base, "first", &claim_files(), RefUpdate::Accumulate)
            .await
            .expect("the first commit creates the branch");
        let second = workspace
            .commit_files("claim", commit_base, "second", &claim_files(), RefUpdate::Accumulate)
            .await
            .expect("the second commit accumulates");

        let updates = fixture.calls("update-ref");
        let last = updates.last().expect("an update-ref");
        let positional: Vec<&String> = last.args.iter().skip_while(|a| *a != "update-ref").collect();
        assert_eq!(
            positional.iter().map(|a| a.as_str()).collect::<Vec<_>>(),
            vec![
                "update-ref",
                "--end-of-options",
                "refs/heads/claim",
                second.as_str(),
                first.as_str()
            ],
            "the swap names the value the commit was built on, behind the separator: {:?}",
            last.args
        );

        assert!(
            !fixture
                .try_git(
                    workspace.directory(),
                    &[
                        "update-ref",
                        "refs/heads/claim",
                        &base,
                        "0000000000000000000000000000000000000000"
                    ],
                )
                .await,
            "git must refuse a mismatched <old>, or the swap above guards nothing"
        );
    }

    // ── C-045: commit identity ───────────────────────────────────────────────

    /// The commit identity is the fixed `ocx <noreply@ocx.sh>`, whatever git
    /// would otherwise read from configuration.
    ///
    /// The discriminator is a **repository-local** `user.name`/`user.email`,
    /// which outranks `~/.gitconfig`: if the assertion still sees `ocx`, the
    /// identity came from `GIT_AUTHOR_*`/`GIT_COMMITTER_*` and not from any
    /// configuration file. A test that only asserted the value without planting a
    /// competing one would pass on a developer's machine whose global identity
    /// happened to be irrelevant, which is the green that cannot be told from
    /// never having run.
    ///
    /// Reds on: removing the four identity rows from `git_command`'s `SET` table
    /// (the commit is then authored `not-ocx <someone@example.invalid>`).
    #[cfg(unix)]
    #[tokio::test]
    async fn commit_identity_is_fixed() {
        let fixture = Fixture::new().await;
        let base = fixture.remote_sha("main").await;
        let workspace = GitWorkspace::open(&fixture.shim, &fixture.remote(), "main", None, None)
            .await
            .expect("open");

        fixture
            .git(workspace.directory(), &["config", "user.name", "not-ocx"])
            .await;
        fixture
            .git(
                workspace.directory(),
                &["config", "user.email", "someone@example.invalid"],
            )
            .await;

        let coordinate = "acme/index"
            .parse::<crate::forge::RepoCoordinate>()
            .expect("a coordinate");
        let sha = workspace
            .commit_files(
                "claim",
                CommitBase {
                    repo: &coordinate,
                    sha: &base,
                    branch: "main",
                },
                "claim acme/widget",
                &claim_files(),
                RefUpdate::Accumulate,
            )
            .await
            .expect("the commit chain");

        let commit = fixture.git(workspace.directory(), &["cat-file", "commit", &sha]).await;
        let author = commit
            .lines()
            .find(|line| line.starts_with("author "))
            .unwrap_or_default();
        let committer = commit
            .lines()
            .find(|line| line.starts_with("committer "))
            .unwrap_or_default();
        assert!(
            author.starts_with("author ocx <noreply@ocx.sh>"),
            "the author is ocx's fixed identity, got {author:?}"
        );
        assert!(
            committer.starts_with("committer ocx <noreply@ocx.sh>"),
            "the committer is ocx's fixed identity, got {committer:?}"
        );
    }

    /// #436: a `FastForward` whose branch head is not the base the caller read
    /// is refused, and nothing is written.
    ///
    /// The shape a production announce hit: the run read its root from the index
    /// base because the forge's branch listing answered "no branch", the git
    /// fetch then found one, and the commit was parented on that head. The push
    /// was a genuine fast-forward, so no CAS anywhere refused it, and a released
    /// version tag left the index with both runs reporting success.
    ///
    /// Two clauses, and the second is the one that matters. "It returned an
    /// error" would pass for a build that refused *after* writing the commit and
    /// moving the local ref — the ref move is what the push would then carry —
    /// so the absence of a `commit-tree` is what says the refusal came first.
    ///
    /// `Accumulate` is the control: the identical fixture, differing only in the
    /// caller's declared relationship to its base, must still commit. Without it
    /// this row passes against a build that refuses every fast-forward onto an
    /// existing head, which would strand every `Ahead` claim.
    #[cfg(unix)]
    #[tokio::test]
    async fn fast_forward_refuses_a_head_the_caller_did_not_read() {
        let fixture = Fixture::new().await;
        fixture.branch_at("claim", "main", 1).await;
        let base = fixture.remote_sha("main").await;
        let head = fixture.remote_sha("claim").await;
        assert_ne!(base, head, "the fixture must put the branch ahead of the base");
        let workspace = GitWorkspace::open(&fixture.shim, &fixture.remote(), "main", Some("claim"), None)
            .await
            .expect("open");
        let coordinate = "acme/index"
            .parse::<crate::forge::RepoCoordinate>()
            .expect("a coordinate");
        let commit_base = CommitBase {
            repo: &coordinate,
            sha: &base,
            branch: "main",
        };

        let refused = workspace
            .commit_files(
                "claim",
                commit_base,
                "derived from main",
                &claim_files(),
                RefUpdate::FastForward,
            )
            .await
            .expect_err("a payload derived from the base may not be laid on another head");
        assert!(
            matches!(refused, ForgeError::NonFastForward { ref branch } if branch == "claim"),
            "the caller retries a race, so the refusal must be spelled as one; got {refused:?}"
        );
        assert!(
            fixture.calls("commit-tree").is_empty(),
            "the refusal precedes the write, or a commit the caller never learned about \
             is already sitting on the local ref: {:?}",
            fixture.calls("commit-tree")
        );

        workspace
            .commit_files(
                "claim",
                commit_base,
                "authored whole",
                &claim_files(),
                RefUpdate::Accumulate,
            )
            .await
            .expect("a payload independent of the base still accumulates onto the head");
        let commits = fixture.calls("commit-tree");
        assert_eq!(commits.len(), 1, "exactly the control commit: {commits:?}");
        let parents: Vec<&String> = commits[0]
            .args
            .iter()
            .skip_while(|argument| *argument != "-p")
            .skip(1)
            .take(1)
            .collect();
        assert_eq!(
            parents.first().map(|sha| sha.as_str()),
            Some(head.as_str()),
            "and it parents on the head, which is the behaviour `FastForward` refuses"
        );
    }

    // ── C-040: the lease ─────────────────────────────────────────────────────

    /// `--force-with-lease` rides the `Reset` rebuild and nothing else, and a
    /// naive `--force` never appears anywhere.
    ///
    /// The lease is the compare-and-swap that keeps a rewrite from clobbering a
    /// branch that moved: without it the rebuild is exactly the silent overwrite
    /// the whole `RefUpdate` distinction exists to prevent.
    ///
    /// Reds on: passing the lease on an ordinary push; reaching for `--force`.
    #[cfg(unix)]
    #[tokio::test]
    async fn force_with_lease_only_on_reset() {
        let fixture = Fixture::new().await;
        fixture.branch_at("claim", "main", 1).await;
        let base = fixture.remote_sha("main").await;
        let workspace = GitWorkspace::open(&fixture.shim, &fixture.remote(), "main", Some("claim"), None)
            .await
            .expect("open");
        let coordinate = "acme/index"
            .parse::<crate::forge::RepoCoordinate>()
            .expect("a coordinate");
        let commit_base = CommitBase {
            repo: &coordinate,
            sha: &base,
            branch: "main",
        };
        let access = preflight();

        // The ordinary accumulate: no lease anywhere.
        workspace
            .commit_files(
                "claim",
                commit_base,
                "accumulate",
                &claim_files(),
                RefUpdate::Accumulate,
            )
            .await
            .expect("the accumulating commit");
        workspace
            .push("claim", "main", "title", "body", None, refusal(&access))
            .await
            .expect("the ordinary push");
        let plain = fixture.calls("push");
        assert!(
            plain.iter().all(|push| {
                !push
                    .args
                    .iter()
                    .any(|argument| argument.starts_with("--force-with-lease"))
            }),
            "an ordinary push carries no lease: {plain:?}"
        );

        // The rebuild: leased against the sha the branch was read at — which,
        // after the accumulate above, is that push's own commit.
        let head = fixture.remote_sha("claim").await;
        workspace
            .commit_files("claim", commit_base, "rebuild", &claim_files(), RefUpdate::Reset)
            .await
            .expect("the rebuild commit");
        workspace
            .push("claim", "main", "title", "body", Some(&head), refusal(&access))
            .await
            .expect("the leased push");

        let leased = fixture.calls("push");
        let last = leased.last().expect("the leased push");
        assert!(
            last.carries(&format!("--force-with-lease=claim:{head}")),
            "the rebuild leases against the sha it read: {:?}",
            last.args
        );
        assert!(
            !fixture.invocations().iter().any(|call| call
                .args
                .iter()
                .any(|argument| argument == "--force" || argument == "-f")),
            "a naive force never appears"
        );
    }

    // ── C-042: the no-pending-commit path ────────────────────────────────────

    /// A refresh commit genuinely moves the ref, carrying the same tree.
    ///
    /// A server only processes merge-request push options for a push that
    /// advances a ref, so an unchanged run with no open request has to produce a
    /// new commit or its push is a no-op the server never sees.
    ///
    /// Reds on: returning the head unchanged; committing a different tree.
    #[cfg(unix)]
    #[tokio::test]
    async fn refresh_commit_advances_the_ref() {
        let fixture = Fixture::new().await;
        fixture.branch_at("claim", "main", 1).await;
        let head = fixture.remote_sha("claim").await;
        let workspace = GitWorkspace::open(&fixture.shim, &fixture.remote(), "main", Some("claim"), None)
            .await
            .expect("open");

        let refreshed = workspace
            .refresh_commit("claim", "refresh acme/widget")
            .await
            .expect("the refresh commit");

        assert_ne!(refreshed, head, "the refresh must produce a new commit");
        let before = fixture
            .git(workspace.directory(), &["rev-parse", &format!("{head}^{{tree}}")])
            .await;
        let after = fixture
            .git(workspace.directory(), &["rev-parse", &format!("{refreshed}^{{tree}}")])
            .await;
        assert_eq!(before, after, "the refresh carries the same tree");
        let parent = fixture
            .git(workspace.directory(), &["rev-parse", &format!("{refreshed}^")])
            .await;
        assert_eq!(parent, head, "the refresh is parented on the branch head");
        let moved = fixture
            .git(workspace.directory(), &["rev-parse", "refs/heads/claim"])
            .await;
        assert_eq!(moved, refreshed, "the local ref really advanced");
    }

    // ── S-023 / C-043: concurrency, and the convergence proof ────────────────

    /// A branch that moved between the read and the push is rejected, and the
    /// rejection is named rather than swallowed.
    #[cfg(unix)]
    #[tokio::test]
    async fn push_to_a_moved_branch_classifies_non_fast_forward() {
        let fixture = Fixture::new().await;
        fixture.branch_at("claim", "main", 1).await;
        let base = fixture.remote_sha("main").await;
        let workspace = GitWorkspace::open(&fixture.shim, &fixture.remote(), "main", Some("claim"), None)
            .await
            .expect("open");
        let coordinate = "acme/index"
            .parse::<crate::forge::RepoCoordinate>()
            .expect("a coordinate");
        workspace
            .commit_files(
                "claim",
                CommitBase {
                    repo: &coordinate,
                    sha: &base,
                    branch: "main",
                },
                "claim acme/widget",
                &claim_files(),
                RefUpdate::Accumulate,
            )
            .await
            .expect("the commit chain");

        // A second writer wins the race.
        fixture.branch_at("claim", "claim", 1).await;

        let access = preflight();
        let outcome = workspace
            .push("claim", "main", "title", "body", None, refusal(&access))
            .await;
        assert!(
            matches!(outcome, Err(ForgeError::NonFastForward { .. })),
            "a moved branch is a non-fast-forward, got {outcome:?}; the workspace ran {:?}",
            fixture.invocations()
        );
    }

    /// The retry **converges**: after the re-fetch the regenerated commit parents
    /// on the winning head and the second push succeeds.
    ///
    /// A fixture that rejects forever proves only that the retry runs. The remote
    /// here moves exactly once, so a build whose retry re-derives its parent from
    /// the stale `o/<branch>` reds on the second push rather than passing.
    ///
    /// Reds on: not re-fetching before the second commit; parenting the retry on
    /// the base instead of the winning head.
    #[cfg(unix)]
    #[tokio::test]
    async fn retry_converges_after_a_moved_branch() {
        let fixture = Fixture::new().await;
        fixture.branch_at("claim", "main", 1).await;
        let base = fixture.remote_sha("main").await;
        let coordinate = "acme/index"
            .parse::<crate::forge::RepoCoordinate>()
            .expect("a coordinate");
        let commit_base = CommitBase {
            repo: &coordinate,
            sha: &base,
            branch: "main",
        };
        let workspace = GitWorkspace::open(&fixture.shim, &fixture.remote(), "main", Some("claim"), None)
            .await
            .expect("open");
        workspace
            .commit_files(
                "claim",
                commit_base,
                "claim acme/widget",
                &claim_files(),
                RefUpdate::Accumulate,
            )
            .await
            .expect("the first commit");

        fixture.branch_at("claim", "claim", 1).await;
        let winner = fixture.remote_sha("claim").await;

        let access = preflight();
        assert!(
            workspace
                .push("claim", "main", "title", "body", None, refusal(&access))
                .await
                .is_err(),
            "the first push loses the race"
        );

        // The retry, exactly as the orchestration performs it.
        workspace.fetch("main", "claim").await.expect("the retry fetch");
        let regenerated = workspace
            .commit_files(
                "claim",
                commit_base,
                "claim acme/widget",
                &claim_files(),
                RefUpdate::Accumulate,
            )
            .await
            .expect("the regenerated commit");
        workspace
            .push("claim", "main", "title", "body", None, refusal(&access))
            .await
            .expect("the second push converges");

        let parent = fixture
            .git(workspace.directory(), &["rev-parse", &format!("{regenerated}^")])
            .await;
        assert_eq!(parent, winner, "the regenerated commit parents on the winning head");
        assert_eq!(
            fixture.remote_sha("claim").await,
            regenerated,
            "the remote carries the regenerated commit"
        );
    }

    // ── S-030: no secret anywhere ────────────────────────────────────────────

    /// No secret reaches argv, the remote URL, `.git/config`, the reflog or the
    /// error — on a failing path, which is the one that builds a message out of
    /// git's own bytes.
    ///
    /// Both live forms are checked: the plaintext secret and the base64
    /// `user:secret` blob it exists as on the wire, which no plaintext needle
    /// matches. The workspace tree is walked whole rather than spot-checked, so
    /// `.git/config`, `git remote -v`'s source and `.git/logs` are covered by one
    /// assertion that cannot go stale as git changes where it writes.
    ///
    /// Reds on: putting the credential in the remote URL; writing it into
    /// `.git/config`; formatting raw stderr into the error.
    #[cfg(unix)]
    #[tokio::test]
    async fn no_secret_reaches_argv_config_or_the_error() {
        let fixture = Fixture::new().await;
        fixture.branch_at("claim", "main", 1).await;
        let base = fixture.remote_sha("main").await;
        let credential = credential();
        let secret = "glpat-WP13-SECRET-VALUE";
        let encoded = credential
            .basic_authorization()
            .strip_prefix("Basic ")
            .unwrap_or_default()
            .to_string();
        let workspace = GitWorkspace::open(
            &fixture.shim,
            &fixture.remote(),
            "main",
            Some("claim"),
            Some(&credential),
        )
        .await
        .expect("open");
        let coordinate = "acme/index"
            .parse::<crate::forge::RepoCoordinate>()
            .expect("a coordinate");
        workspace
            .commit_files(
                "claim",
                CommitBase {
                    repo: &coordinate,
                    sha: &base,
                    branch: "main",
                },
                "claim acme/widget",
                &claim_files(),
                RefUpdate::Accumulate,
            )
            .await
            .expect("the commit chain");
        fixture.branch_at("claim", "claim", 1).await;

        let access = preflight();
        let failure = workspace
            .push("claim", "main", "title", "body", None, refusal(&access))
            .await
            .expect_err("the push loses the race");
        let rendered = format!("{failure} {failure:?}");

        for form in [secret, encoded.as_str()] {
            assert!(
                !rendered.contains(form),
                "the error must not carry the secret: {rendered}"
            );
            for invocation in fixture.invocations() {
                assert!(
                    !invocation.args.iter().any(|argument| argument.contains(form)),
                    "no secret in argv: {:?}",
                    invocation.args
                );
            }
            for (path, contents) in all_files(workspace.directory()) {
                let text = String::from_utf8_lossy(&contents);
                assert!(
                    !text.contains(form),
                    "no secret anywhere under the clone, found one in {}",
                    path.display()
                );
            }
        }
    }

    /// The credential is injected on the network invocations and on no others,
    /// and a run without one configures no `extraHeader` at all.
    #[cfg(unix)]
    #[tokio::test]
    async fn injection_rides_the_network_invocations_only() {
        let fixture = Fixture::new().await;
        let bare = Fixture::new().await;

        let credential = credential();
        let _authenticated = GitWorkspace::open(&fixture.shim, &fixture.remote(), "main", None, Some(&credential))
            .await
            .expect("open with a credential");
        let _anonymous = GitWorkspace::open(&bare.shim, &bare.remote(), "main", None, None)
            .await
            .expect("open without one");

        for invocation in fixture.invocations() {
            let injected = invocation
                .env
                .get("GIT_CONFIG_KEY_0")
                .map(String::as_str)
                .unwrap_or_default();
            let resets_helper = invocation
                .args
                .windows(2)
                .any(|window| window[0] == "-c" && window[1] == "credential.helper=");
            if invocation.subcommand() == "fetch" || invocation.subcommand() == "push" {
                assert_eq!(
                    injected,
                    format!("http.{}.extraHeader", fixture.remote()),
                    "the network invocation carries the scoped header"
                );
                assert!(
                    resets_helper,
                    "a credential-injecting invocation resets the helper list"
                );
            } else {
                assert!(injected.is_empty(), "{} needs no credential", invocation.subcommand());
                assert!(
                    !resets_helper,
                    "{} injects nothing, so it must not reset the operator's helpers",
                    invocation.subcommand()
                );
            }
        }

        for invocation in bare.invocations() {
            assert!(
                invocation
                    .env
                    .get("GIT_CONFIG_KEY_0")
                    .map(String::as_str)
                    .unwrap_or_default()
                    .is_empty(),
                "a run with no credential configures no extraHeader: {:?}",
                invocation.args
            );
            assert!(
                !invocation
                    .args
                    .windows(2)
                    .any(|window| window[0] == "-c" && window[1] == "credential.helper="),
                "step three leaves the operator's own helpers in charge: {:?}",
                invocation.args
            );
        }
    }

    // ── C-033: the workspace directory ───────────────────────────────────────

    /// The clone is private on Unix and is removed when the workspace is
    /// dropped.
    ///
    /// Mode `0700` is asserted only under `#[cfg(unix)]` on purpose: Windows has
    /// no mode bits, so the same assertion there would pass whether or not
    /// anything happened.
    #[cfg(unix)]
    #[tokio::test]
    async fn workspace_directory_is_private_and_removed() {
        use std::os::unix::fs::PermissionsExt as _;

        let fixture = Fixture::new().await;
        let workspace = GitWorkspace::open(&fixture.shim, &fixture.remote(), "main", None, None)
            .await
            .expect("open");
        let directory = workspace.directory().to_path_buf();

        let mode = std::fs::metadata(&directory)
            .expect("the clone exists")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700, "the clone is private to its owner, got {mode:o}");

        drop(workspace);
        assert!(
            !directory.exists(),
            "the guard removes the clone: {} survived",
            directory.display()
        );
    }

    /// A failure inside `open` leaves nothing behind — the guard runs on the
    /// paths that unwind, not only on the happy one.
    ///
    /// The refusal is also a scope refusal, so it doubles as the fail-closed half
    /// of C-034: a remote naming no project never reaches a process at all.
    #[cfg(unix)]
    #[tokio::test]
    async fn open_removes_the_directory_when_it_fails() {
        let fixture = Fixture::new().await;

        // A remote whose branch does not exist: the fetch fails after the
        // directory was created.
        let outcome = GitWorkspace::open(&fixture.shim, &fixture.remote(), "main", Some("absent"), None).await;
        assert!(outcome.is_err(), "a fetch of an absent ref fails the whole open");

        let credential = credential();
        let widened =
            GitWorkspace::open(&fixture.shim, "https://gitlab.example", "main", None, Some(&credential)).await;
        assert!(widened.is_err(), "a remote naming no project is refused");
        assert!(
            fixture.invocations().iter().all(|invocation| !invocation
                .args
                .iter()
                .any(|argument| argument.contains("gitlab.example"))),
            "the scope refusal happens before anything is spawned"
        );
    }

    /// A push option the wire forbids is refused before any process starts.
    ///
    /// The refusal itself is the renderer's (C-039, WP-11); what this pins is
    /// that the workspace asks *before* it spawns, so a rejected value never
    /// reaches a pkt-line and no ref moves.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_refused_push_option_spawns_no_process() {
        let fixture = Fixture::new().await;
        let workspace = GitWorkspace::open(&fixture.shim, &fixture.remote(), "main", None, None)
            .await
            .expect("open");
        let before = fixture.invocations().len();

        let access = preflight();
        let outcome = workspace
            .push("claim", "main", "", "body", None, refusal(&access))
            .await;
        assert!(
            matches!(outcome, Err(ForgeError::PushOptionRefused { .. })),
            "an empty title is refused by the renderer, got {outcome:?}"
        );
        assert_eq!(fixture.invocations().len(), before, "a refused option spawns nothing");
    }

    /// A lease that does not match the branch's real value is refused, and the
    /// refusal is named.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_stale_lease_is_refused() {
        let fixture = Fixture::new().await;
        fixture.branch_at("claim", "main", 1).await;
        let base = fixture.remote_sha("main").await;
        let workspace = GitWorkspace::open(&fixture.shim, &fixture.remote(), "main", Some("claim"), None)
            .await
            .expect("open");
        let coordinate = "acme/index"
            .parse::<crate::forge::RepoCoordinate>()
            .expect("a coordinate");
        workspace
            .commit_files(
                "claim",
                CommitBase {
                    repo: &coordinate,
                    sha: &base,
                    branch: "main",
                },
                "rebuild",
                &claim_files(),
                RefUpdate::Reset,
            )
            .await
            .expect("the rebuild commit");

        let access = preflight();
        let outcome = workspace
            .push("claim", "main", "title", "body", Some(&base), refusal(&access))
            .await;
        assert!(
            outcome.is_err(),
            "a lease naming the wrong sha must not overwrite the branch, got {outcome:?}"
        );
    }

    // ── The published exit code for a rejected credential ────────────────────

    /// A rejected credential on a non-push invocation is named as the forge
    /// status it was, not as an opaque plumbing failure.
    ///
    /// This is the wiring half; the phrase table and its recordings are
    /// `git_stderr.rs`'s. Driven through `command_failed` because that is the
    /// single function every non-push failure in this file routes through, and
    /// because no `file://` fixture can produce a 401 — an acceptance row against
    /// an auth-enforcing HTTP fixture is the end-to-end proof and lives in
    /// `test/tests/test_transport_git.py`.
    ///
    /// The negative row is the discriminating one: an ordinary plumbing failure
    /// must still build `GitCommandFailed`, which is unclassified and exits 1, or
    /// every unreadable object would start reporting an authentication problem.
    ///
    /// Reds on: deleting the classifier call from `command_failed` (both auth
    /// rows become `GitCommandFailed`); classifying unconditionally (the plumbing
    /// row becomes a `Status`); capping before classifying (the 403 row, whose
    /// phrase sits past 300 characters of server banner, falls through).
    #[cfg(unix)]
    #[tokio::test]
    async fn a_rejected_credential_is_classified_before_the_generic_path() {
        use std::os::unix::process::ExitStatusExt as _;

        let fixture = Fixture::new().await;
        let workspace = GitWorkspace::open(&fixture.shim, &fixture.remote(), "main", None, None)
            .await
            .expect("open");

        let failed = |stderr: &str| std::process::Output {
            // 128 is what `git` exits with on a `fatal:`; the raw form is
            // `code << 8` because the low byte carries the signal.
            status: std::process::ExitStatus::from_raw(128 << 8),
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        };
        // A banner long enough that the phrase sits past `Redacted::CAP`, which
        // is the ordering a real server produces and the one a cap-then-classify
        // implementation destroys.
        let banner = "remote: Enumerating objects: 1, done.\n".repeat(12);

        let cases = [
            (
                format!("{banner}fatal: could not read Username for 'x': terminal prompts disabled"),
                401_u16,
            ),
            (format!("{banner}fatal: Authentication failed for 'x'"), 401),
            (
                format!("{banner}fatal: unable to access 'x': The requested URL returned error: 403"),
                403,
            ),
        ];
        for (stderr, expected) in cases {
            assert!(
                stderr.len() > Redacted::CAP,
                "the fixture body must exceed the cap, or this row does not test the ordering"
            );
            match workspace.command_failed("fetch", &failed(&stderr)) {
                ForgeError::Status { url, status, .. } => {
                    assert_eq!(status, expected, "{stderr:?} carried the wrong status");
                    assert_eq!(
                        url,
                        fixture.remote(),
                        "the error names the remote, never a plumbing step"
                    );
                }
                other => panic!("a rejected credential must not reach the generic path; got {other:?}"),
            }
        }

        match workspace.command_failed("read-tree", &failed("fatal: not a valid object name")) {
            ForgeError::GitCommandFailed { command, .. } => assert_eq!(command, "read-tree"),
            other => panic!("an ordinary plumbing failure stays unclassified; got {other:?}"),
        }
    }

    // ── The argv boundary: --end-of-options and the transfer deadline ────────

    /// Drive every builder in the file once — init, fetch, rev-parse, rev-list,
    /// hash-object, read-tree, update-index, write-tree, commit-tree, update-ref
    /// and push — and hand back what the shim recorded.
    ///
    /// Shared by the two argv assertions below so neither can quietly test a
    /// shorter run than it claims.
    #[cfg(unix)]
    async fn recipe_invocations(fixture: &Fixture) -> Vec<fixture::Invocation> {
        fixture.branch_at("claim", "main", 1).await;
        let base = fixture.remote_sha("main").await;
        let credential = credential();
        let workspace = GitWorkspace::open(
            &fixture.shim,
            &fixture.remote(),
            "main",
            Some("claim"),
            Some(&credential),
        )
        .await
        .expect("open");

        workspace.compare("main", "claim").await.expect("compare");
        workspace
            .commit_files(
                "claim",
                CommitBase {
                    repo: &"acme/index"
                        .parse::<crate::forge::RepoCoordinate>()
                        .expect("a coordinate"),
                    sha: &base,
                    branch: "main",
                },
                "claim acme/widget",
                &claim_files(),
                RefUpdate::Accumulate,
            )
            .await
            .expect("the commit chain");
        let access = preflight();
        workspace
            .push(
                "claim",
                "main",
                "claim acme/widget",
                "owners: alice:1",
                None,
                refusal(&access),
            )
            .await
            .expect("the push");

        let invocations = fixture.invocations();
        assert!(
            invocations.len() >= 10,
            "the run must exercise the whole recipe, saw {}",
            invocations.len()
        );
        invocations
    }

    /// Every invocation that has a positional puts `--end-of-options`
    /// immediately before the first one, and every invocation still succeeds.
    ///
    /// The separator is what stops a sha or a ref that begins with `-` being read
    /// as an option — measured against git 2.54.0,
    /// `git read-tree --upload-pack=/bin/false` without it is parsed as an
    /// *unknown option* (exit 129) rather than as a bad revision, and
    /// `--upload-pack` names a program to run.
    ///
    /// **The placement is the assertion, not the presence.** git reads everything
    /// after the separator as a revision or a path, so a separator in the wrong
    /// place does not fail loudly — it silently reinterprets the flags that
    /// follow it. Measured: `commit-tree --end-of-options <tree> -p <parent>`
    /// dies with `fatal: must give exactly one tree`, and `push … --end-of-options
    /// <remote> <refspec> -o <option>` turns each push option into a refspec. So
    /// this asserts *immediately before the first positional* and, because the
    /// whole recipe actually runs to a successful push above, that every
    /// subcommand still parses.
    ///
    /// Reds on: dropping the separator from the argv builder; emitting it before
    /// the subcommand; emitting it before the flags; emitting it at the end.
    #[cfg(unix)]
    #[tokio::test]
    async fn every_positional_argument_sits_behind_end_of_options() {
        let fixture = Fixture::new().await;
        let invocations = recipe_invocations(&fixture).await;

        // Which invocations carry a positional at all, so the "at least one"
        // assertion below cannot be satisfied by a build that emits nothing.
        let mut separated = 0_usize;
        for invocation in &invocations {
            let Some(position) = invocation
                .args
                .iter()
                .position(|argument| argument == "--end-of-options")
            else {
                // `write-tree` and `update-index` have no positional; a separator
                // with nothing after it would be noise.
                assert!(
                    matches!(invocation.subcommand(), "write-tree" | "update-index"),
                    "{} has positionals and no separator: {:?}",
                    invocation.subcommand(),
                    invocation.args
                );
                continue;
            };
            separated += 1;
            assert!(
                position + 1 < invocation.args.len(),
                "{} ends with the separator, which protects nothing: {:?}",
                invocation.subcommand(),
                invocation.args
            );
            assert!(
                invocation.args[..position]
                    .iter()
                    .any(|argument| argument == invocation.subcommand()),
                "the separator must follow the subcommand, not precede it: {:?}",
                invocation.args
            );
            assert!(
                invocation.args[position + 1..]
                    .iter()
                    .all(|argument| !argument.starts_with('-')),
                "{} left a flag past the separator, where git reads it as a revision: {:?}",
                invocation.subcommand(),
                invocation.args
            );
        }
        assert!(
            separated >= 8,
            "most of the recipe carries positionals; only {separated} invocations were separated"
        );
    }

    /// Every invocation carries the stalled-transfer deadline.
    ///
    /// A remote that accepts the connection and then trickles has no other bound
    /// on this path: `git` has no total timeout of its own, and the deferred
    /// `tokio::time::timeout` is deliberately still deferred. 1000 bytes/s for 60
    /// consecutive seconds aborts the transfer; a slow-but-progressing large
    /// fetch is untouched, which is why it is a stall bound and not a total one.
    ///
    /// Asserted on **every** invocation rather than on the two that touch the
    /// network, for the same reason the redirect refusal is: it rides the one
    /// shared argv builder, so "all but one" is not a state this file can reach.
    ///
    /// Reds on: removing either `-c` pair from the builder; setting them on the
    /// network builder alone (the local invocations then fail).
    #[cfg(unix)]
    #[tokio::test]
    async fn every_git_invocation_carries_the_stalled_transfer_deadline() {
        let fixture = Fixture::new().await;
        let invocations = recipe_invocations(&fixture).await;

        for invocation in &invocations {
            for setting in ["http.lowSpeedLimit=1000", "http.lowSpeedTime=60"] {
                assert!(
                    invocation
                        .args
                        .windows(2)
                        .any(|window| window[0] == "-c" && window[1] == setting),
                    "every invocation carries {setting}, {} did not: {:?}",
                    invocation.subcommand(),
                    invocation.args
                );
            }
        }
    }

    // ── One process per commit, not one per file ─────────────────────────────

    /// Three files cost one `hash-object` and one `update-index`, and each file's
    /// bytes land under **its own** path.
    ///
    /// The batching is for announce, whose mature cascade writes a hundred-odd
    /// roots on every publish; a claim writes one file and gains nothing. The
    /// process count is the point of the change, but the pairing assertion is the
    /// point of the test: batching pairs git's output lines with the caller's
    /// path list positionally, so a reordered or short read commits each file's
    /// bytes under the next file's name — a corruption no count can see, and one
    /// that a single-file fixture cannot reach at all.
    ///
    /// The contents are deliberately distinguishable per path. Identical bodies
    /// would hash to one object and the pairing would hold under any permutation.
    ///
    /// Reds on: going back to one process per file (the counts); pairing the
    /// object names with the paths in the wrong order (the contents); dropping a
    /// file (`ls-tree`).
    #[cfg(unix)]
    #[tokio::test]
    async fn a_multi_file_commit_costs_one_hash_object_and_one_update_index() {
        let fixture = Fixture::new().await;
        let base = fixture.remote_sha("main").await;
        let workspace = GitWorkspace::open(&fixture.shim, &fixture.remote(), "main", None, None)
            .await
            .expect("open");

        let paths = ["p/acme/alpha.json", "p/acme/beta.json", "p/zulu/gamma.json"];
        let mut files = BTreeMap::new();
        for path in paths {
            files.insert(path.to_string(), format!("{{\"name\":\"{path}\"}}\n").into_bytes());
        }

        let coordinate = "acme/index"
            .parse::<crate::forge::RepoCoordinate>()
            .expect("a coordinate");
        let sha = workspace
            .commit_files(
                "claim",
                CommitBase {
                    repo: &coordinate,
                    sha: &base,
                    branch: "main",
                },
                "claim three",
                &files,
                RefUpdate::Accumulate,
            )
            .await
            .expect("the commit chain");

        assert_eq!(
            fixture.calls("hash-object").len(),
            1,
            "three files must cost one hash-object, not three"
        );
        assert_eq!(
            fixture.calls("update-index").len(),
            1,
            "three files must cost one update-index, not three"
        );

        let listed = fixture
            .git(workspace.directory(), &["ls-tree", "-r", "--name-only", &sha])
            .await;
        for path in paths {
            assert!(
                listed.lines().any(|line| line == path),
                "{path} is missing from the tree: {listed:?}"
            );
            let blob = fixture
                .git(workspace.directory(), &["cat-file", "-p", &format!("{sha}:{path}")])
                .await;
            assert_eq!(
                blob,
                format!("{{\"name\":\"{path}\"}}"),
                "{path} carries another file's bytes, so the object names were paired out of order"
            );
        }
    }

    /// The batched pairing is positional, and a list that does not pair one
    /// object per file is refused rather than truncated.
    ///
    /// Asserted here rather than through a live `git` **because a live `git`
    /// cannot produce the failing input**: it exits non-zero on a path it cannot
    /// hash, so the caller fails one step earlier and the arity arm never runs.
    /// Driven from the commit path, that mutation stays green — measured — which
    /// is the definition of a guard whose red state is unreachable. Handing the
    /// mismatched list in directly is what gives it one.
    ///
    /// The happy row pins the order too: `files` is a `BTreeMap`, so its key
    /// order is the order the staged files were written and therefore the order
    /// git was given them. If that ever stops being true, each file's bytes are
    /// committed under the next file's path.
    ///
    /// Reds on: deleting the arity check (both mismatched rows); pairing in
    /// reverse or by any other order (the happy row); emitting one flag per
    /// entry instead of the `--cacheinfo` pair.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_index_pairing_refuses_a_list_that_is_not_one_object_per_file() {
        let fixture = Fixture::new().await;
        let workspace = GitWorkspace::open(&fixture.shim, &fixture.remote(), "main", None, None)
            .await
            .expect("open");

        let mut files = BTreeMap::new();
        for path in ["p/acme/alpha.json", "p/acme/beta.json", "p/zulu/gamma.json"] {
            files.insert(path.to_string(), b"{}\n".to_vec());
        }
        let blob = |tag: char| std::iter::repeat_n(tag, 40).collect::<String>();

        let entries = workspace
            .index_entries(&files, &[blob('a'), blob('b'), blob('c')])
            .expect("one object per file is the shape git returns");
        assert_eq!(
            entries,
            vec![
                "--cacheinfo".to_string(),
                format!("100644,{},p/acme/alpha.json", blob('a')),
                "--cacheinfo".to_string(),
                format!("100644,{},p/acme/beta.json", blob('b')),
                "--cacheinfo".to_string(),
                format!("100644,{},p/zulu/gamma.json", blob('c')),
            ],
            "each object name must pair with the path git was given at that position"
        );

        for (blobs, case) in [
            (vec![blob('a'), blob('b')], "one object name short"),
            (
                vec![blob('a'), blob('b'), blob('c'), blob('d')],
                "one object name too many",
            ),
            (Vec::new(), "no object names at all"),
        ] {
            match workspace.index_entries(&files, &blobs) {
                Err(ForgeError::GitCommandFailed { command, .. }) => {
                    assert_eq!(
                        command, "hash-object",
                        "{case}: the refusal names the step that misread"
                    );
                }
                other => panic!("{case}: a mismatched list must be refused, not paired; got {other:?}"),
            }
        }
    }

    // ── The escape is the description's alone ────────────────────────────────

    /// `escape_newlines` reaches the description and neither the title nor the
    /// target — asserted through `push`, where the escape is actually applied.
    ///
    /// The distinction is the whole contract. A claim body is multi-line markdown
    /// and GitLab converts the two-character `\n` back before storing it, so the
    /// description **must** be escaped or it dies at its first LF. `target` is a
    /// ref name and `title` is one line: escaping either would turn a newline
    /// that must be **refused** into one silently carried into a merge request a
    /// human reviews.
    ///
    /// **Driven through `push` on purpose.** The refusal itself is
    /// `render_push_options`', and a test that calls the renderer directly stays
    /// green when the escape is widened — the renderer never sees the escape.
    /// Only `push` composes the two, so only here is the mutation reachable.
    ///
    /// The accepting row is not decoration: it is what stops the whole thing
    /// being satisfied by a `push` that refuses everything, and it pins the
    /// escaped body on the wire byte for byte.
    ///
    /// Reds on: applying `escape_newlines` to `title` or to `target` (that row
    /// stops being refused and a process is spawned); deleting it from
    /// `description` (the accepting row is refused).
    #[cfg(unix)]
    #[tokio::test]
    async fn push_escapes_the_description_and_refuses_a_newline_in_title_or_target() {
        let fixture = Fixture::new().await;
        let base = fixture.remote_sha("main").await;
        let workspace = GitWorkspace::open(&fixture.shim, &fixture.remote(), "main", None, None)
            .await
            .expect("open");
        // The accepting row below must reach the wire, so there has to be a
        // branch to push: a refspec naming nothing fails before any option is
        // read, and the test would then pass without observing the escape.
        workspace
            .commit_files(
                "claim",
                CommitBase {
                    repo: &"acme/index"
                        .parse::<crate::forge::RepoCoordinate>()
                        .expect("a coordinate"),
                    sha: &base,
                    branch: "main",
                },
                "claim acme/widget",
                &claim_files(),
                RefUpdate::Accumulate,
            )
            .await
            .expect("the commit chain");
        let access = preflight();

        for (branch, target, title, key) in [
            ("claim", "main", "claim acme/widget\nsecond line", "merge_request.title"),
            ("claim", "main\nmore", "claim acme/widget", "merge_request.target"),
        ] {
            let before = fixture.invocations().len();
            let outcome = workspace
                .push(branch, target, title, "body", None, refusal(&access))
                .await;
            match outcome {
                Err(ForgeError::PushOptionRefused { key: refused, .. }) => {
                    assert_eq!(
                        refused, key,
                        "the refusal must name the option that carried the newline"
                    );
                }
                other => panic!("a newline in {key} must be refused, not escaped; got {other:?}"),
            }
            assert_eq!(
                fixture.invocations().len(),
                before,
                "a refused option must spawn nothing"
            );
        }

        // The permissive half, through the same entry point: a multi-line body
        // reaches the wire as the two-character sequence GitLab converts back.
        let body = "Package claim for `ocx.sh/acme/widget`.\n\n- name: ocx.sh/acme/widget\n";
        workspace
            .push("claim", "main", "claim acme/widget", body, None, refusal(&access))
            .await
            .expect("a multi-line body is what the escape exists for");

        let pushed = fixture.calls("push");
        let pushed = pushed.last().expect("the push was recorded");
        let expected = format!("merge_request.description={}", escape_newlines(body));
        assert!(
            pushed.carries(&expected),
            "the escaped body must reach the wire verbatim: {:?}",
            pushed.args
        );
        assert!(
            pushed.carries("merge_request.title=claim acme/widget"),
            "the title reaches the wire unescaped: {:?}",
            pushed.args
        );
    }
}
