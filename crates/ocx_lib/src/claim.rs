// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx package claim` orchestration (ADR `adr_index_claim_command.md`).
//!
//! A publisher claims a package once, before its first `ocx package announce`:
//! claim renders the package's index root from structured flags, opens a pull
//! request against `ocx-sh/index`, and leaves the merge to that repository's
//! human governance gate. Announce refuses an unclaimed package at exit 79;
//! claim refuses an already-claimed one at exit 65 (C-050). The two codes are the
//! pair a release wrapper branches on (S-037).
//!
//! Forge-neutral and transport-blind **by construction**: `commit_files` and
//! `open_or_update_pull_request` dispatch on the transport *inside* the forge,
//! and [`ClaimRequest`] carries no transport field, so the orchestration below
//! behaves identically under `api` and `git`. The per-transport half of C-051 is
//! an acceptance test, not a unit one.
//!
//! Claim never calls `pull_request_mergeability` (C-053): every divergent case
//! resets onto the current index base, so the request is mergeable by
//! construction. Announce, which appends, does call it — copying announce's
//! `Diverged` arm here would reintroduce a read this design removed.

pub mod error;
pub mod owners;
pub mod request;
pub mod root;

pub use error::ClaimError;
pub use owners::{OwnerResolution, ResolvedOwner, resolve_owners};
pub use request::{
    ClaimOutcome, ClaimRequest, ClaimStatus, ClaimTarget, OwnerIdentitySource, OwnerSpec, Upstream, request_body,
    request_title, upstream_repository_url_is_publishable,
};
pub use root::{parse_repository, render_root, root_name};

use std::collections::BTreeMap;
use std::path::Path;

use crate::forge::{BranchComparison, CommitBase, Forge, ForgeError, PushAccess, RefUpdate, RepoCoordinate};
use crate::log;

/// The base branch of the index repository — the ref the C-050 refusal reads and
/// the ref every claim commit is parented on.
pub const INDEX_BASE_REF: &str = "main";

/// The claim branch for `package` (C-054).
///
/// `indexbot-claim-<namespace>-<package>`, deliberately distinct from announce's
/// `indexbot-announce-…` so indexbot's G-04 `new-package` classification is never
/// confused with a refresh. Derived **once** per run and carried on
/// [`ClaimOutcome::branch`], so the reported branch and the ref actually written
/// cannot diverge.
///
/// The `/`→`-` substitution collides `a-b/c` with `a/b-c`. That is pre-existing
/// and identical on announce; fixing it here would diverge the two namings C-054
/// exists to keep parallel.
#[must_use]
pub fn claim_branch(package: &str) -> String {
    format!("indexbot-claim-{}", package.replace('/', "-"))
}

/// The root's path inside the index repository.
#[must_use]
pub fn root_path(package: &str) -> String {
    format!("p/{package}.json")
}

/// Claim one package.
///
/// `forge` is `Some` in every mode: `--out` still reads the committed root for the
/// C-050 refusal and still resolves owners. S-011's `push-access: skipped` under
/// `--out` is satisfied by **not calling `ensure_push_access`** on that path — the
/// rows then come from `PushAccess::skipped_all()` — never by a forge answering
/// `skipped`, which on GitHub would mean an unreadable `permissions` and exit 80.
///
/// # Errors
///
/// Returns a [`ClaimError`] for a missing forge, a malformed `--repository`, an
/// already-claimed package, any owner-ladder refusal, a missing base ref, a
/// race lost twice (`NonFastForward` under `api`, `StaleLease` under `git`), an
/// `--out` write failure, or any forge failure.
pub async fn claim(forge: Option<&dyn Forge>, request: ClaimRequest) -> Result<ClaimOutcome, ClaimError> {
    let forge = forge.ok_or(ClaimError::ForgeRequired)?;
    let package = request.package.repository().to_string();
    let root_path = root_path(&package);
    // Derived ONCE and carried onto the outcome, so the branch the report names
    // and the ref actually written cannot diverge.
    let branch = claim_branch(&package);
    let repository = parse_repository(&request.repository)?;

    // C-050 — the refusal reads the index BASE ref, never the claim branch: a
    // re-run of an unmerged claim must report `unchanged`, not 65. It runs
    // before anything is written, `--out` included.
    if forge
        .get_file_contents(&request.index_repo, &root_path, INDEX_BASE_REF)
        .await?
        .is_some()
    {
        return Err(ClaimError::PackageAlreadyClaimed {
            package,
            path: root_path,
            base_ref: INDEX_BASE_REF.to_string(),
        });
    }

    let OwnerResolution {
        owners,
        source,
        author,
        author_identity_source,
    } = resolve_owners(forge, &request.owners).await?;
    let name = root_name(&request.package);
    // C-072 — who this claims for, on stderr, BEFORE any write. The word is
    // read from `source` here and again from `ClaimOutcome::owner_identity_source`
    // by the report and the request body, so the three cannot disagree.
    log::info!(
        "claiming {name} for {} (owner identity source: {source})",
        owners
            .iter()
            .map(|owner| format!("{}:{}", owner.login, owner.id))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let root_bytes = render_root(&name, &repository, &owners, request.upstream.as_ref());

    // `--out` writes the tree and stops: no branch exists to be unchanged
    // against, so the run is ALWAYS `updated`, and `ensure_push_access` is not
    // called at all — S-011's `push-access: skipped` comes from
    // `PushAccess::skipped_all()`, never from a forge answering `skipped`.
    if let ClaimTarget::Out(directory) = &request.target {
        let written_paths = write_out(directory, &root_path, &root_bytes).await?;
        return Ok(ClaimOutcome {
            package,
            name,
            status: ClaimStatus::Updated,
            owners,
            owner_identity_source: source,
            author,
            author_identity_source,
            branch,
            pull_request: None,
            fork: None,
            written_paths,
            push_access: PushAccess::skipped_all(),
        });
    }

    // Where the claim branch lives right now: a fork that already exists, the
    // index repository itself on the fork-free path. Read-only (`find_fork`),
    // so an unchanged run provokes no fork create.
    let fork_target = match &request.target {
        ClaimTarget::Fork(target) => Some(target),
        ClaimTarget::Direct | ClaimTarget::Out(_) => None,
    };
    let existing_fork = match fork_target {
        Some(target) => forge.find_fork(&request.index_repo, target).await?,
        None => None,
    };
    let branch_repo: Option<RepoCoordinate> = match &request.target {
        ClaimTarget::Direct => Some(request.index_repo.clone()),
        ClaimTarget::Fork(_) => existing_fork.as_ref().map(|fork| fork.coordinate(&request.index_repo)),
        ClaimTarget::Out(_) => None,
    };

    // C-051's state machine. `compare_branch` runs if and only if the branch
    // exists — an absent branch has no second ref to compare, so the call is a
    // contract and not an optimisation.
    let branch_head = match &branch_repo {
        Some(repo) => forge.get_ref_sha(repo, &head_ref(&branch)).await?,
        None => None,
    };
    let live_branch = branch_repo.as_ref().filter(|_| branch_head.is_some());
    let comparison = match live_branch {
        Some(repo) => Some(
            forge
                .compare_branch(&request.index_repo, INDEX_BASE_REF, repo, &branch)
                .await?,
        ),
        None => None,
    };
    let open_request = match live_branch {
        Some(repo) => forge.find_open_pull_request(&request.index_repo, repo, &branch).await?,
        None => None,
    };
    // `Behind` and `Diverged` are rebuilt on the current index base, so the ref
    // is repointed with a lease; every other state fast-forwards, which is a
    // real compare-and-swap. `pull_request_mergeability` is never consulted
    // (C-053): the rebuild makes the request mergeable by construction.
    let update = match comparison {
        Some(BranchComparison::Behind | BranchComparison::Diverged) => RefUpdate::Reset,
        _ => RefUpdate::FastForward,
    };

    // The one state that may write nothing: a branch strictly ahead of the base
    // already carrying byte-identical content. The comparison is on BYTES —
    // `serde_json::Value`'s object is an `IndexMap` and `IndexMap::eq` is
    // order-independent, so a value-level compare reports `unchanged` for a
    // root whose fields a third party reordered.
    if let Some(repo) = live_branch
        && comparison == Some(BranchComparison::Ahead)
        && forge.get_file_contents(repo, &root_path, &branch).await?.as_deref() == Some(root_bytes.as_slice())
    {
        // Content already right AND a request already open ⇒ no write of any
        // kind. Without one, the content is stranded, so the request is
        // ensured — and nothing else is.
        let pull_request = match open_request {
            Some(pull_request) => pull_request,
            None => {
                forge
                    .open_or_update_pull_request(
                        &request.index_repo,
                        repo,
                        &branch,
                        INDEX_BASE_REF,
                        &request_title(&name),
                        &request_body(&name, &repository, &branch, &owners, source),
                    )
                    .await?
            }
        };
        return Ok(ClaimOutcome {
            package,
            name,
            status: ClaimStatus::Unchanged,
            owners,
            owner_identity_source: source,
            author,
            author_identity_source,
            branch,
            pull_request: Some(pull_request),
            fork: existing_fork,
            written_paths: Vec::new(),
            push_access: PushAccess::skipped_all(),
        });
    }

    // The write path. Push permission is verified before the first byte on the
    // fork-free path only: a fork is the credential's own repository.
    let (commit_repo, fork, push_access) = match fork_target {
        Some(target) => {
            let fork = match existing_fork {
                Some(fork) => fork,
                None => forge.ensure_fork(&request.index_repo, Some(&target.namespace)).await?,
            };
            let coordinate = fork.coordinate(&request.index_repo);
            // The commit parents off a SHA read from the upstream but is
            // written to the fork, so land that object in the fork's own
            // history first.
            forge.sync_fork(&coordinate, INDEX_BASE_REF).await;
            (coordinate, Some(fork), PushAccess::skipped_all())
        }
        None => {
            let push_access = forge.ensure_push_access(&request.index_repo).await?;
            (request.index_repo.clone(), None, push_access)
        }
    };

    let files = BTreeMap::from([(root_path.clone(), root_bytes)]);
    let message = request_title(&name);
    let body = request_body(&name, &repository, &branch, &owners, source);
    // Every claim commit is parented on the index base — never on the branch.
    // A claim root accumulates nothing, so there is no branch content to build
    // on, and basing on the base is what keeps `Behind`/`Diverged` mergeable.
    let base_sha = read_base_sha(forge, &request.index_repo).await?;
    // The commit and the pull request are **one unit of work** for the purposes
    // of the race, because which of the two loses it depends on the transport
    // (the same reasoning announce's C-056 retry is built on). Under `api` the
    // commit's compare-and-swap is rejected. Under `git`, `commit_files`
    // performs no network write at all — objects and a local ref only — and the
    // push happens inside `open_or_update_pull_request`, so the rejection
    // arrives there. Retrying around the commit alone would therefore give up
    // under exactly the transport that needs the retry most.
    let first_attempt = match forge
        .commit_files(
            &commit_repo,
            &branch,
            CommitBase {
                repo: &request.index_repo,
                sha: &base_sha,
                branch: INDEX_BASE_REF,
            },
            &message,
            &files,
            update,
        )
        .await
    {
        Ok(_) => {
            forge
                .open_or_update_pull_request(
                    &request.index_repo,
                    &commit_repo,
                    &branch,
                    INDEX_BASE_REF,
                    &message,
                    &body,
                )
                .await
        }
        Err(error) => Err(error),
    };
    let pull_request = match first_attempt {
        Ok(pull_request) => pull_request,
        // The two spellings of "the base moved under us", one per transport.
        // `api` refuses the fast-forward; `git` loses the `--force-with-lease`
        // and reports `StaleLease`. Matching only the first would retry under
        // `api` and give up under `git` for the identical race — and since both
        // classify to exit 75, no assertion on the outcome could tell the two
        // behaviours apart.
        Err(ForgeError::NonFastForward { .. } | ForgeError::StaleLease { .. }) => {
            // C-051: re-fetch, re-read the winning head and regenerate EXACTLY
            // once — never a loop, or a second rejection is swallowed. A claim
            // root is rendered wholly from the request, so "regenerate" adds
            // nothing to merge; what the re-read buys is the refusal below.
            if let Some(repo) = &branch_repo
                && forge.get_ref_sha(repo, &head_ref(&branch)).await?.is_some()
                && forge.get_file_contents(repo, &root_path, &branch).await?.is_none()
            {
                // The writer that won the race left no root at its head, so
                // there is nothing to regenerate against. Committing the
                // freshly-rendered root anyway would clobber them silently.
                return Err(ClaimError::MissingHeadRoot {
                    branch,
                    path: root_path,
                });
            }
            let base_sha = read_base_sha(forge, &request.index_repo).await?;
            forge
                .commit_files(
                    &commit_repo,
                    &branch,
                    CommitBase {
                        repo: &request.index_repo,
                        sha: &base_sha,
                        branch: INDEX_BASE_REF,
                    },
                    &message,
                    &files,
                    update,
                )
                .await?;
            // One shot, by construction: a second rejection propagates rather
            // than starting a third pass — still rejected is exit 75, and the
            // caller reruns the command.
            forge
                .open_or_update_pull_request(
                    &request.index_repo,
                    &commit_repo,
                    &branch,
                    INDEX_BASE_REF,
                    &message,
                    &body,
                )
                .await?
        }
        // Every other failure is settled, not raced. Widening this to `Err(_)`
        // once two calls feed one `match` is the natural mistake and the costly
        // one: `MergeRequestUnconfirmed` means the push already landed, so
        // retrying pushes a second time.
        Err(other) => return Err(ClaimError::Forge(other)),
    };

    Ok(ClaimOutcome {
        package,
        name,
        status: ClaimStatus::Updated,
        owners,
        owner_identity_source: source,
        author,
        author_identity_source,
        branch,
        pull_request: Some(pull_request),
        fork,
        written_paths: Vec::new(),
        push_access,
    })
}

/// The git ref path for a branch.
///
/// [`Forge::get_ref_sha`] speaks ref paths (`heads/<branch>`), not bare branch
/// names: GitHub builds `/git/ref/{ref}` from it verbatim and answers 404 for a
/// bare name, while GitLab strips the prefix.
fn head_ref(branch: &str) -> String {
    format!("heads/{branch}")
}

/// The commit parent every claim uses.
///
/// # Errors
///
/// [`ClaimError::MissingBaseRef`] — never `unwrap_or_default`, which parents the
/// commit on an empty string.
async fn read_base_sha(forge: &dyn Forge, index_repo: &RepoCoordinate) -> Result<String, ClaimError> {
    forge
        .get_ref_sha(index_repo, &head_ref(INDEX_BASE_REF))
        .await?
        .ok_or_else(|| ClaimError::MissingBaseRef {
            repo: index_repo.full_path(),
            base_ref: INDEX_BASE_REF.to_string(),
        })
}

/// Write the rendered root under the `--out` directory.
///
/// # Errors
///
/// [`ClaimError::OutputWrite`] — its own variant rather than a bare
/// `io::Error`, because `cli::classify` special-cases only `PermissionDenied`
/// and everything else would land on exit 1.
async fn write_out(directory: &Path, relative: &str, bytes: &[u8]) -> Result<Vec<String>, ClaimError> {
    let path = directory.join(relative);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|source| ClaimError::OutputWrite {
                path: parent.display().to_string(),
                source,
            })?;
    }
    tokio::fs::write(&path, bytes)
        .await
        .map_err(|source| ClaimError::OutputWrite {
            path: path.display().to_string(),
            source,
        })?;
    Ok(vec![relative.to_string()])
}

/// The claim orchestration's tests, plus the [`FakeForge`] double they and the
/// owner-ladder tests share.
///
/// `pub(crate)` rather than private, and deliberately: no `Forge` double existed
/// anywhere in this crate before WP-9, and both this module and `claim::owners`
/// need one. A second copy in `owners.rs` would be the same fixture maintained
/// twice, drifting the moment the trait grows a method.
#[cfg(test)]
pub(crate) mod tests {
    use std::collections::{BTreeMap, HashMap};
    use std::sync::Mutex;

    use super::*;
    use crate::forge::{
        BranchComparison, CommitBase, ForgeError, ForgeIdentity, ForkIdentity, Mergeability, PullRequest, PushAccess,
        RefUpdate, RepoCoordinate,
    };
    use crate::oci;

    pub(crate) const PINNED_INSTANT: &str = "2026-01-02T03:04:05Z";

    /// One recorded [`Forge`] call, in the order the orchestration made it.
    ///
    /// Recording — rather than only scripting answers — is what makes the
    /// *absent* calls assertable: C-051's `Absent` row and C-053 are both
    /// contracts about a call that must **not** happen, and no return value can
    /// express that.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(crate) enum Call {
        AuthenticatedIdentity,
        ResolveUser(String),
        GetFileContents {
            path: String,
            reference: String,
        },
        GetRefSha(String),
        CompareBranch {
            base: String,
            head_branch: String,
        },
        FindOpenPullRequest(String),
        Mergeability(u64),
        EnsurePushAccess,
        CommitFiles {
            branch: String,
            base_sha: String,
            base_branch: String,
            update: RefUpdate,
            files: BTreeMap<String, Vec<u8>>,
        },
        OpenOrUpdatePullRequest {
            branch: String,
            title: String,
            body: String,
        },
    }

    /// A scripted, recording [`Forge`].
    #[derive(Default)]
    pub(crate) struct FakeForge {
        /// `None` → the users API is unreachable
        /// ([`ForgeError::UsersApiUnavailable`]); `Some(map)` → reachable, keyed
        /// by **lower-cased** login so the case-insensitive lookup is
        /// exercisable.
        pub(crate) users: Option<HashMap<String, ForgeIdentity>>,
        /// `authenticated_identity`'s answer. `None` is the App-installation-token
        /// shape (`Ok(None)`), which is not the same as an unreachable API.
        pub(crate) token_identity: Option<ForgeIdentity>,
        /// File contents keyed by `(reference, path)`.
        pub(crate) files: HashMap<(String, String), Vec<u8>>,
        /// `get_ref_sha` answers keyed by ref name. A branch absent here does not
        /// exist.
        pub(crate) refs: HashMap<String, String>,
        /// `compare_branch`'s answer when it is (wrongly or rightly) called.
        pub(crate) comparison: Option<BranchComparison>,
        /// The open request `find_open_pull_request` reports.
        pub(crate) open_request: Option<PullRequest>,
        /// How many `commit_files` calls raise `NonFastForward` before one
        /// succeeds.
        pub(crate) non_fast_forward_rejections: Mutex<u32>,
        /// How many `open_or_update_pull_request` calls raise `StaleLease`
        /// before one succeeds — the **git** transport's shape of the same
        /// race, where `commit_files` writes nothing over the network and the
        /// push lives inside the request call.
        pub(crate) stale_lease_rejections: Mutex<u32>,
        /// Panic instead of answering `pull_request_mergeability` (C-053).
        pub(crate) forbid_mergeability: bool,
        /// Every call, in order.
        pub(crate) calls: Mutex<Vec<Call>>,
    }

    impl FakeForge {
        pub(crate) fn record(&self, call: Call) {
            self.calls.lock().expect("the call log is not poisoned").push(call);
        }

        pub(crate) fn calls(&self) -> Vec<Call> {
            self.calls.lock().expect("the call log is not poisoned").clone()
        }

        pub(crate) fn commit_calls(&self) -> usize {
            self.calls()
                .iter()
                .filter(|call| matches!(call, Call::CommitFiles { .. }))
                .count()
        }

        pub(crate) fn pull_request_calls(&self) -> usize {
            self.calls()
                .iter()
                .filter(|call| matches!(call, Call::OpenOrUpdatePullRequest { .. }))
                .count()
        }

        pub(crate) fn called_compare_branch(&self) -> bool {
            self.calls()
                .iter()
                .any(|call| matches!(call, Call::CompareBranch { .. }))
        }
    }

    #[async_trait::async_trait]
    impl Forge for FakeForge {
        async fn authenticated_identity(&self) -> Result<Option<ForgeIdentity>, ForgeError> {
            self.record(Call::AuthenticatedIdentity);
            match &self.users {
                Some(_) => Ok(self.token_identity.clone()),
                None => Err(ForgeError::UsersApiUnavailable),
            }
        }

        async fn resolve_user(&self, login: &str) -> Result<Option<ForgeIdentity>, ForgeError> {
            self.record(Call::ResolveUser(login.to_string()));
            match &self.users {
                Some(users) => Ok(users.get(&login.to_ascii_lowercase()).cloned()),
                None => Err(ForgeError::UsersApiUnavailable),
            }
        }

        async fn get_file_contents(
            &self,
            _repo: &RepoCoordinate,
            path: &str,
            reference: &str,
        ) -> Result<Option<Vec<u8>>, ForgeError> {
            self.record(Call::GetFileContents {
                path: path.to_string(),
                reference: reference.to_string(),
            });
            Ok(self.files.get(&(reference.to_string(), path.to_string())).cloned())
        }

        async fn get_ref_sha(&self, _repo: &RepoCoordinate, reference: &str) -> Result<Option<String>, ForgeError> {
            self.record(Call::GetRefSha(reference.to_string()));
            // The trait speaks git ref paths, and `refs` is keyed by bare
            // branch name. GitHub builds `/git/ref/{ref}` verbatim and answers
            // 404 for a bare name, so an orchestration that passes one would
            // resolve nothing in production while a tolerant fake stayed green.
            let name = reference
                .strip_prefix("heads/")
                .expect("`Forge::get_ref_sha` takes a ref path such as `heads/<branch>`, never a bare branch name");
            Ok(self.refs.get(name).cloned())
        }

        async fn compare_branch(
            &self,
            _repo: &RepoCoordinate,
            base: &str,
            _head: &RepoCoordinate,
            head_branch: &str,
        ) -> Result<BranchComparison, ForgeError> {
            self.record(Call::CompareBranch {
                base: base.to_string(),
                head_branch: head_branch.to_string(),
            });
            Ok(self.comparison.unwrap_or(BranchComparison::Identical))
        }

        async fn find_open_pull_request(
            &self,
            _index: &RepoCoordinate,
            _head: &RepoCoordinate,
            branch: &str,
        ) -> Result<Option<PullRequest>, ForgeError> {
            self.record(Call::FindOpenPullRequest(branch.to_string()));
            Ok(self.open_request.clone())
        }

        async fn pull_request_mergeability(
            &self,
            _index: &RepoCoordinate,
            number: u64,
        ) -> Result<Mergeability, ForgeError> {
            self.record(Call::Mergeability(number));
            assert!(
                !self.forbid_mergeability,
                "C-053: claim must never call pull_request_mergeability — every divergent case \
                 resets onto the current base, so the request is mergeable by construction"
            );
            Ok(Mergeability::Mergeable)
        }

        async fn find_fork(
            &self,
            _upstream: &RepoCoordinate,
            _fork: &RepoCoordinate,
        ) -> Result<Option<ForkIdentity>, ForgeError> {
            Ok(None)
        }

        async fn ensure_fork(
            &self,
            _upstream: &RepoCoordinate,
            _target_owner: Option<&str>,
        ) -> Result<ForkIdentity, ForgeError> {
            Err(ForgeError::TransportOperationUnsupported {
                operation: "fork".to_string(),
                transport: crate::forge::WriteTransport::Api,
            })
        }

        async fn sync_fork(&self, _fork: &RepoCoordinate, _branch: &str) {}

        async fn ensure_push_access(&self, _repo: &RepoCoordinate) -> Result<PushAccess, ForgeError> {
            self.record(Call::EnsurePushAccess);
            Ok(PushAccess::skipped_all())
        }

        async fn commit_files(
            &self,
            _repo: &RepoCoordinate,
            branch: &str,
            base: CommitBase<'_>,
            _message: &str,
            files: &BTreeMap<String, Vec<u8>>,
            update: RefUpdate,
        ) -> Result<String, ForgeError> {
            self.record(Call::CommitFiles {
                branch: branch.to_string(),
                base_sha: base.sha.to_string(),
                base_branch: base.branch.to_string(),
                update,
                files: files.clone(),
            });
            let mut remaining = self
                .non_fast_forward_rejections
                .lock()
                .expect("the rejection counter is not poisoned");
            if *remaining > 0 {
                *remaining -= 1;
                return Err(ForgeError::NonFastForward {
                    branch: branch.to_string(),
                });
            }
            Ok("commitsha".to_string())
        }

        async fn open_or_update_pull_request(
            &self,
            _index: &RepoCoordinate,
            _head: &RepoCoordinate,
            branch: &str,
            _base: &str,
            title: &str,
            body: &str,
        ) -> Result<PullRequest, ForgeError> {
            self.record(Call::OpenOrUpdatePullRequest {
                branch: branch.to_string(),
                title: title.to_string(),
                body: body.to_string(),
            });
            let mut remaining = self
                .stale_lease_rejections
                .lock()
                .expect("the rejection counter is not poisoned");
            if *remaining > 0 {
                *remaining -= 1;
                return Err(ForgeError::StaleLease {
                    branch: branch.to_string(),
                });
            }
            Ok(PullRequest {
                number: 42,
                html_url: "https://example.test/pull/42".to_string(),
                updated: self.open_request.is_some(),
            })
        }
    }

    pub(crate) fn identity(login: &str, id: u64, bot: bool) -> ForgeIdentity {
        ForgeIdentity {
            login: login.to_string(),
            id,
            bot,
        }
    }

    pub(crate) const PACKAGE: &str = "acme/widget";
    pub(crate) const ROOT_PATH: &str = "p/acme/widget.json";
    pub(crate) const CLAIM_BRANCH: &str = "indexbot-claim-acme-widget";
    pub(crate) const BASE_SHA: &str = "basesha";

    pub(crate) fn index_repo() -> RepoCoordinate {
        RepoCoordinate {
            host: None,
            namespace: "ocx-sh".to_string(),
            project: "index".to_string(),
        }
    }

    fn request(target: ClaimTarget) -> ClaimRequest {
        ClaimRequest {
            package: oci::Identifier::new_registry(PACKAGE, "ocx.sh"),
            repository: "oci://ghcr.io/acme/widget".to_string(),
            owners: vec![OwnerSpec::Resolved {
                login: "alice".to_string(),
                id: 1234,
            }],
            upstream: None,
            target,
            index_repo: index_repo(),
        }
    }

    /// A forge whose users API is reachable, whose index base exists, and whose
    /// claim branch does not — the ordinary first-claim state.
    fn first_claim_forge() -> FakeForge {
        let mut users = HashMap::new();
        users.insert("alice".to_string(), identity("alice", 1234, false));
        FakeForge {
            users: Some(users),
            token_identity: Some(identity("alice", 1234, false)),
            refs: HashMap::from([(INDEX_BASE_REF.to_string(), BASE_SHA.to_string())]),
            forbid_mergeability: true,
            ..FakeForge::default()
        }
    }

    /// Pins the shared clock and blanks the owner ladder's CI environment.
    ///
    /// **Not re-entrant** — `crate::test::env::lock()` is a plain
    /// `std::sync::Mutex`, so a second live guard on the same thread deadlocks.
    /// One per test.
    struct ClockSeam {
        _lock: crate::test::env::EnvLock,
    }

    impl ClockSeam {
        fn pinned() -> Self {
            let lock = crate::test::env::lock();
            // SAFETY: `EnvLock` serialises every env-touching test against this
            // write, and `Drop` clears it unconditionally. `EnvLock::set` cannot
            // carry the pin: `oci::index::current_timestamp` reads
            // `std::env::var` directly, so the override map never reaches it.
            unsafe { std::env::set_var("__OCX_TESTING_ANNOUNCE_CLOCK", PINNED_INSTANT) };
            // All four, never the pair a test exercises: `crate::env::var`
            // falls through to `std::env` for a key with no override, and
            // GitHub Actions exports `GITHUB_ACTOR`/`GITHUB_ACTOR_ID` on the
            // runner this repository's gate uses.
            for key in owners::CI_IDENTITY_VARS {
                lock.remove(key);
            }
            Self { _lock: lock }
        }
    }

    impl Drop for ClockSeam {
        fn drop(&mut self) {
            // SAFETY: see `ClockSeam::pinned`. A struct's own `Drop` runs before
            // its fields', so the pin is gone before the lock releases — without
            // this the pin outlives the test and every later reader of the
            // shared clock in this process sees a fixed instant.
            unsafe { std::env::remove_var("__OCX_TESTING_ANNOUNCE_CLOCK") };
        }
    }

    fn pinned_clock() -> ClockSeam {
        ClockSeam::pinned()
    }

    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a current-thread runtime builds")
            .block_on(future)
    }

    // ── C-054: the branch name ───────────────────────────────────────────────

    /// C-054 — the claim branch is `indexbot-claim-<ns>-<pkg>`, asserted against
    /// the literal and against announce's live form.
    ///
    /// Idempotence would not catch a rename: a run that used
    /// `indexbot-announce-…` throughout is internally consistent. The inequality
    /// against the announce spelling is what makes a future rename of *either*
    /// side red, which is the whole point of C-054 — indexbot's G-04
    /// `new-package` classification keys on the difference.
    ///
    /// Reds on: the `indexbot-announce-` prefix, or dropping the `/`→`-`
    /// substitution.
    #[test]
    fn claim_branch_name_is_distinct_from_announce() {
        assert_eq!(claim_branch(PACKAGE), CLAIM_BRANCH);
        assert_eq!(root_path(PACKAGE), ROOT_PATH);

        let announce_branch = format!("indexbot-announce-{}", PACKAGE.replace('/', "-"));
        assert_ne!(
            claim_branch(PACKAGE),
            announce_branch,
            "the two branch namespaces must not converge"
        );
        assert!(
            !claim_branch(PACKAGE).contains('/'),
            "a branch name carrying a slash would nest refs under the namespace"
        );
    }

    /// The branch name is derived **once** and the same value reaches both the
    /// ref that is written and [`ClaimOutcome::branch`].
    ///
    /// Reds on: deriving it twice with one spelling changed — the report then
    /// names a branch that does not exist, and a script acting on the report
    /// addresses nothing.
    #[test]
    fn branch_name_is_derived_once_and_reported() {
        let _clock = pinned_clock();
        let forge = first_claim_forge();

        let outcome = block_on(claim(Some(&forge), request(ClaimTarget::Direct))).expect("the first claim succeeds");

        assert_eq!(outcome.branch, CLAIM_BRANCH);
        let committed_branches: Vec<String> = forge
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                Call::CommitFiles { branch, .. } => Some(branch),
                _ => None,
            })
            .collect();
        assert_eq!(
            committed_branches,
            vec![outcome.branch.clone()],
            "the reported branch is the ref that was actually written"
        );
    }

    // ── C-050: an already-claimed package ────────────────────────────────────

    /// C-050 — a root already committed on the **index base ref** is refused at
    /// exit 65, naming `ocx package announce`, in every mode.
    ///
    /// Three things no existing test asserts: that an already-*committed* root is
    /// what produces the 65 (rather than merely that some error yields it), that
    /// the **base ref** and not the claim branch is what is read, and that the
    /// refusal survives `--out`.
    ///
    /// Reds on: reading the branch instead of the base (an idempotent re-run of an
    /// unmerged claim then exits 65 instead of reporting `unchanged`), gating the
    /// check on `target != Out`, or dropping the remedy from the message.
    #[test]
    fn package_already_claimed_is_refused_at_data_error() {
        use crate::cli::{ClassifyExitCode, ExitCode};

        let output = tempfile::TempDir::new().expect("a temp dir is created");
        for target in [ClaimTarget::Direct, ClaimTarget::Out(output.path().to_path_buf())] {
            let _clock = pinned_clock();
            let mut forge = first_claim_forge();
            forge
                .files
                .insert((INDEX_BASE_REF.to_string(), ROOT_PATH.to_string()), b"{}\n".to_vec());

            let error = block_on(claim(Some(&forge), request(target))).expect_err("a committed root refuses the claim");
            assert!(
                matches!(&error, ClaimError::PackageAlreadyClaimed { path, base_ref, .. }
                    if path == ROOT_PATH && base_ref == INDEX_BASE_REF),
                "the refusal names the committed path on the base ref: {error:?}"
            );
            assert_eq!(error.classify(), Some(ExitCode::DataError));
            assert!(
                error.to_string().contains("ocx package announce"),
                "the message points the operator at the command that does work here: {error}"
            );
            assert_eq!(
                std::fs::read_dir(output.path())
                    .expect("the out dir is readable")
                    .count(),
                0,
                "under --out the refusal happens before anything is written"
            );
        }
    }

    /// The mirror image: a root sitting on the **claim branch** — an unmerged,
    /// re-run claim — is not an already-claimed package.
    ///
    /// This is the positive control for the assertion above. Without it, an
    /// implementation that reads neither ref passes the refusal test.
    ///
    /// Reds on: reading the branch ref for the C-050 check.
    #[test]
    fn an_unmerged_claim_on_the_branch_is_not_already_claimed() {
        let _clock = pinned_clock();
        let mut forge = first_claim_forge();
        forge.refs.insert(CLAIM_BRANCH.to_string(), "branchsha".to_string());
        forge.comparison = Some(BranchComparison::Ahead);
        forge
            .files
            .insert((CLAIM_BRANCH.to_string(), ROOT_PATH.to_string()), b"{}\n".to_vec());

        let outcome = block_on(claim(Some(&forge), request(ClaimTarget::Direct)))
            .expect("an unmerged claim on the branch is a re-run, not a refusal");
        assert_eq!(outcome.package, PACKAGE);
    }

    // ── C-051 / C-053: the branch state machine ──────────────────────────────

    /// C-051 — every branch state, over `{open request present, absent}`.
    ///
    /// The axis is **not** `× both transports`. The orchestration is
    /// transport-blind by construction: `commit_files` and
    /// `open_or_update_pull_request` both dispatch on the transport *inside* the
    /// forge, and [`ClaimRequest`] carries no transport field — so a fake `Forge`
    /// behaves identically under any transport label and the second axis
    /// multiplies rows without adding a reachable red. The per-transport half is
    /// an acceptance test over the git fixture.
    ///
    /// Reds on: calling `compare_branch` on an absent branch (a contract, not an
    /// optimisation — there is no second ref to compare); `RefUpdate::Reset` on
    /// `Absent`/`Identical`/`Ahead` (weakening a real compare-and-swap into a
    /// lease); `RefUpdate::FastForward` on `Behind`/`Diverged` (a behind branch
    /// can then never merge); parenting a commit anywhere but the index base;
    /// writing at all on the byte-identical-with-open-request row; or skipping the
    /// request-ensure on the byte-identical-without-one row.
    #[test]
    fn branch_state_machine_table() {
        let open = PullRequest {
            number: 42,
            html_url: "https://example.test/pull/42".to_string(),
            updated: true,
        };

        // (branch exists, comparison, root on branch is byte-identical, open
        //  request, expected update, expects a commit, expects a request ensure)
        type StateRow = (bool, Option<BranchComparison>, bool, bool, RefUpdate, bool, bool);
        let rows: [StateRow; 8] = [
            (false, None, false, false, RefUpdate::FastForward, true, true),
            (
                true,
                Some(BranchComparison::Identical),
                false,
                false,
                RefUpdate::FastForward,
                true,
                true,
            ),
            (
                true,
                Some(BranchComparison::Ahead),
                true,
                true,
                RefUpdate::FastForward,
                false,
                false,
            ),
            (
                true,
                Some(BranchComparison::Ahead),
                true,
                false,
                RefUpdate::FastForward,
                false,
                true,
            ),
            (
                true,
                Some(BranchComparison::Ahead),
                false,
                false,
                RefUpdate::FastForward,
                true,
                true,
            ),
            (
                true,
                Some(BranchComparison::Behind),
                false,
                false,
                RefUpdate::Reset,
                true,
                true,
            ),
            (
                true,
                Some(BranchComparison::Diverged),
                false,
                false,
                RefUpdate::Reset,
                true,
                true,
            ),
            (
                true,
                Some(BranchComparison::Diverged),
                false,
                true,
                RefUpdate::Reset,
                true,
                true,
            ),
        ];

        for (index, (branch_exists, comparison, identical, has_request, update, expects_commit, expects_ensure)) in
            rows.into_iter().enumerate()
        {
            let _clock = pinned_clock();
            let mut forge = first_claim_forge();
            forge.comparison = comparison;
            forge.open_request = has_request.then(|| open.clone());
            if branch_exists {
                forge.refs.insert(CLAIM_BRANCH.to_string(), "branchsha".to_string());
            }
            if identical {
                // The bytes the renderer will produce this run, placed on the
                // branch head so the comparison finds them unchanged.
                let rendered = render_root(
                    &root_name(&oci::Identifier::new_registry(PACKAGE, "ocx.sh")),
                    "oci://ghcr.io/acme/widget",
                    &[ResolvedOwner {
                        login: "alice".to_string(),
                        id: 1234,
                    }],
                    None,
                );
                forge
                    .files
                    .insert((CLAIM_BRANCH.to_string(), ROOT_PATH.to_string()), rendered);
            }

            let outcome = block_on(claim(Some(&forge), request(ClaimTarget::Direct))).unwrap_or_else(|error| {
                panic!("row {index} must succeed: {error:?}");
            });
            let calls = forge.calls();

            assert_eq!(
                forge.called_compare_branch(),
                branch_exists,
                "row {index}: compare_branch runs if and only if the branch exists"
            );

            let commits: Vec<&Call> = calls
                .iter()
                .filter(|call| matches!(call, Call::CommitFiles { .. }))
                .collect();
            assert_eq!(
                commits.len(),
                usize::from(expects_commit),
                "row {index}: whether a commit happens at all is the contract"
            );
            if let Some(Call::CommitFiles {
                base_sha,
                base_branch,
                update: actual,
                ..
            }) = commits.first()
            {
                assert_eq!(
                    base_sha, BASE_SHA,
                    "row {index}: the commit is parented on the index base"
                );
                assert_eq!(
                    base_branch, INDEX_BASE_REF,
                    "row {index}: on the base ref, not the branch"
                );
                assert_eq!(*actual, update, "row {index}: the ref-update mode");
            }

            assert_eq!(
                calls
                    .iter()
                    .filter(|call| matches!(call, Call::OpenOrUpdatePullRequest { .. }))
                    .count(),
                usize::from(expects_ensure),
                "row {index}: an unchanged branch already carrying a request writes nothing at all; \
                 the same branch without one still has its request ensured"
            );

            if !expects_commit {
                assert_eq!(outcome.status, ClaimStatus::Unchanged, "row {index}");
            }
        }
    }

    /// C-053 — `pull_request_mergeability` is never called, driven over the state
    /// where announce *does* call it.
    ///
    /// As a bare assertion this would be vacuous: a run that only exercises
    /// `Absent` proves nothing, because nothing would call it there either. The
    /// discriminating state is `Diverged` **with an open pull request** — exactly
    /// announce's `PullRequestUnmergeable` tripwire — so the table is driven
    /// through the panicking fake including that row.
    ///
    /// Reds on: copying announce's mergeability check onto the `Diverged` arm.
    #[test]
    fn claim_never_calls_mergeability() {
        for comparison in [
            BranchComparison::Identical,
            BranchComparison::Ahead,
            BranchComparison::Behind,
            BranchComparison::Diverged,
        ] {
            for has_request in [false, true] {
                let _clock = pinned_clock();
                let mut forge = first_claim_forge();
                forge.forbid_mergeability = true;
                forge.comparison = Some(comparison);
                forge.refs.insert(CLAIM_BRANCH.to_string(), "branchsha".to_string());
                forge.open_request = has_request.then(|| PullRequest {
                    number: 42,
                    html_url: "https://example.test/pull/42".to_string(),
                    updated: true,
                });

                block_on(claim(Some(&forge), request(ClaimTarget::Direct)))
                    .unwrap_or_else(|error| panic!("{comparison:?}/{has_request} must succeed: {error:?}"));

                assert!(
                    !forge.calls().iter().any(|call| matches!(call, Call::Mergeability(_))),
                    "C-053: no mergeability read on {comparison:?} with open_request={has_request}"
                );
            }
        }
    }

    /// The unchanged short-circuit compares **serialized bytes**, not parsed
    /// values.
    ///
    /// `serde_json::Value`'s object is an `IndexMap` under `preserve_order`, and
    /// `IndexMap::eq` is `other.get(key)` — order-independent. A value-level
    /// comparison therefore reports `unchanged` for a branch root whose fields a
    /// third party reordered, and leaves the malformed ordering in place forever.
    ///
    /// Reds on: `serde_json::from_slice::<Value>(head) == rendered_value`.
    #[test]
    fn byte_identical_comparison_is_on_bytes_not_parsed_values() {
        let _clock = pinned_clock();
        let rendered = render_root(
            &root_name(&oci::Identifier::new_registry(PACKAGE, "ocx.sh")),
            "oci://ghcr.io/acme/widget",
            &[ResolvedOwner {
                login: "alice".to_string(),
                id: 1234,
            }],
            None,
        );
        let text = String::from_utf8(rendered).expect("the root is UTF-8");
        // Same fields, same values, `owners` and `status` transposed.
        let (head, index) = (
            text.replace(
                "  \"owners\": [\n    {\n      \"login\": \"alice\",\n      \"id\": 1234\n    }\n  ],\n  \"status\": \"active\",\n",
                "  \"status\": \"active\",\n  \"owners\": [\n    {\n      \"login\": \"alice\",\n      \"id\": 1234\n    }\n  ],\n",
            ),
            0,
        );
        assert_ne!(head, text, "row {index}: the reorder actually changed the bytes");

        let mut forge = first_claim_forge();
        forge.refs.insert(CLAIM_BRANCH.to_string(), "branchsha".to_string());
        forge.comparison = Some(BranchComparison::Ahead);
        forge.open_request = Some(PullRequest {
            number: 42,
            html_url: "https://example.test/pull/42".to_string(),
            updated: true,
        });
        forge
            .files
            .insert((CLAIM_BRANCH.to_string(), ROOT_PATH.to_string()), head.into_bytes());

        let outcome = block_on(claim(Some(&forge), request(ClaimTarget::Direct))).expect("the claim succeeds");
        assert_eq!(
            outcome.status,
            ClaimStatus::Updated,
            "a reordered head root is not byte-identical, so the claim rewrites it"
        );
        assert_eq!(forge.commit_calls(), 1, "and it commits rather than short-circuiting");
    }

    /// C-051 — on `NonFastForward` the claim re-fetches, re-reads the winning
    /// head and regenerates **exactly once**.
    ///
    /// Both `NonFastForward` and `StaleLease` map to exit 75, so no exit-code
    /// assertion can tell "retried once, then gave up" from "looped". Only the
    /// commit call count can.
    ///
    /// Reds on: a `loop` (the second rejection is swallowed and the count exceeds
    /// two), or no retry at all (the count is one and the first rejection
    /// surfaces).
    #[test]
    fn non_fast_forward_retry_regenerates_exactly_once() {
        let _clock = pinned_clock();
        let forge = first_claim_forge();
        *forge
            .non_fast_forward_rejections
            .lock()
            .expect("the counter is not poisoned") = 1;

        block_on(claim(Some(&forge), request(ClaimTarget::Direct))).expect("one retry converges");
        assert_eq!(forge.commit_calls(), 2, "one rejection, one retry, then success");

        // One `_clock` for the whole test: `crate::test::env::lock()` is a plain
        // `std::sync::Mutex`, so taking a second guard here would deadlock.
        let twice = first_claim_forge();
        *twice
            .non_fast_forward_rejections
            .lock()
            .expect("the counter is not poisoned") = 2;
        let error = block_on(claim(Some(&twice), request(ClaimTarget::Direct)))
            .expect_err("a second rejection surfaces rather than looping");
        assert!(
            matches!(&error, ClaimError::Forge(ForgeError::NonFastForward { .. })),
            "the second rejection is raised, not retried: {error:?}"
        );
        assert_eq!(twice.commit_calls(), 2, "exactly one retry, never a loop");
    }

    /// C-051 under the **git** transport — the retry covers the commit *and*
    /// the pull request, because that is where the push lives there.
    ///
    /// `commit_files` performs no network write under `git`: it writes objects
    /// and a local ref, and the push happens inside
    /// `open_or_update_pull_request`. So a lost race is reported by the request
    /// call, not by the commit — and a retry wrapped around `commit_files`
    /// alone gives up under exactly the transport that needs it most, losing a
    /// claim to a concurrent writer that a rerun would have carried.
    ///
    /// Both halves in one function on purpose: converging on one rejection
    /// alone passes for an unbounded loop, and the second-rejection half alone
    /// passes for no retry at all.
    ///
    /// Reds on: matching only `NonFastForward`; retrying around `commit_files`
    /// alone; a `loop` (the count exceeds two).
    #[test]
    fn a_stale_lease_at_the_pull_request_retries_the_commit_and_the_request() {
        let _clock = pinned_clock();
        let forge = first_claim_forge();
        *forge
            .stale_lease_rejections
            .lock()
            .expect("the counter is not poisoned") = 1;

        block_on(claim(Some(&forge), request(ClaimTarget::Direct))).expect("one retry converges");
        assert_eq!(forge.commit_calls(), 2, "the commit is redone against the re-read base");
        assert_eq!(forge.pull_request_calls(), 2, "and the request is opened on the retry");

        // One `_clock` for the whole test: `crate::test::env::lock()` is a plain
        // `std::sync::Mutex`, so taking a second guard here would deadlock.
        let twice = first_claim_forge();
        *twice
            .stale_lease_rejections
            .lock()
            .expect("the counter is not poisoned") = 2;
        let error = block_on(claim(Some(&twice), request(ClaimTarget::Direct)))
            .expect_err("a second rejection surfaces rather than looping");
        assert!(
            matches!(&error, ClaimError::Forge(ForgeError::StaleLease { .. })),
            "the second rejection is raised, not retried: {error:?}"
        );
        assert_eq!(twice.pull_request_calls(), 2, "exactly one retry, never a loop");
    }

    /// The retry's re-read finds no root at the winning head ⇒
    /// [`ClaimError::MissingHeadRoot`], unclassified.
    ///
    /// Reds on: defaulting to the freshly-rendered root — the concurrent writer
    /// whose commit won the race is then silently clobbered.
    #[test]
    fn missing_head_root_after_retry_is_refused() {
        use crate::cli::ClassifyExitCode;

        let _clock = pinned_clock();
        let mut forge = first_claim_forge();
        forge.refs.insert(CLAIM_BRANCH.to_string(), "branchsha".to_string());
        forge.comparison = Some(BranchComparison::Ahead);
        // The branch exists and is ahead, but carries no root — so the post-
        // rejection re-read has nothing to regenerate against.
        *forge
            .non_fast_forward_rejections
            .lock()
            .expect("the counter is not poisoned") = 1;

        let error = block_on(claim(Some(&forge), request(ClaimTarget::Direct)))
            .expect_err("a winning head with no root cannot be regenerated against");
        assert!(
            matches!(&error, ClaimError::MissingHeadRoot { branch, .. } if branch == CLAIM_BRANCH),
            "the error names the branch whose head could not be read: {error:?}"
        );
        assert_eq!(
            error.classify(),
            None,
            "a broken invariant is deliberately unclassified, the GitPushFailed precedent"
        );
    }

    /// A missing index base ref ⇒ [`ClaimError::MissingBaseRef`], unclassified.
    ///
    /// Reds on: `unwrap_or_default()` on the base sha — the commit is then
    /// parented on an empty string.
    #[test]
    fn missing_base_ref_is_refused() {
        use crate::cli::ClassifyExitCode;

        let _clock = pinned_clock();
        let mut forge = first_claim_forge();
        forge.refs.remove(INDEX_BASE_REF);

        let error =
            block_on(claim(Some(&forge), request(ClaimTarget::Direct))).expect_err("no base ref, no commit parent");
        assert!(
            matches!(&error, ClaimError::MissingBaseRef { base_ref, .. } if base_ref == INDEX_BASE_REF),
            "{error:?}"
        );
        assert_eq!(error.classify(), None);
    }

    // ── C-067 / C-072: one rendering of the provenance word ──────────────────

    /// C-072's WP-9 half — the `owner_identity_source` word in the request body
    /// and the word on [`ClaimOutcome`] are **one rendering of one value**.
    ///
    /// The plan named this `owner_identity_source_agrees_across_stderr_report_and_body`,
    /// but two of those three surfaces are outside this crate: the JSON report is
    /// the CLI's file, and stderr is the process's. The three-surface assertion
    /// is an acceptance test; the single-source property is what is provable
    /// here, and it is the one a second rendering breaks.
    ///
    /// Reds on: rendering the word a second time in the body renderer with one
    /// spelling changed (`ci_environment` for `ci-environment`).
    #[test]
    fn owner_identity_source_is_rendered_once_for_body_and_outcome() {
        let _clock = pinned_clock();
        let forge = first_claim_forge();

        let outcome = block_on(claim(Some(&forge), request(ClaimTarget::Direct))).expect("the claim succeeds");
        let body = forge
            .calls()
            .into_iter()
            .find_map(|call| match call {
                Call::OpenOrUpdatePullRequest { body, .. } => Some(body),
                _ => None,
            })
            .expect("a request was opened");

        let word = outcome.owner_identity_source.to_string();
        assert!(
            body.contains(&word),
            "the body carries the outcome's own word verbatim: {word} not in {body}"
        );
        for other in OwnerIdentitySource::ALL {
            if other != outcome.owner_identity_source {
                assert!(
                    !body.contains(&other.to_string()),
                    "and carries no second, differently-spelled provenance word: {other} in {body}"
                );
            }
        }
    }

    /// C-048's confirm half, end to end: the server's canonical login spelling
    /// reaches the **root bytes** and the **request body**, not just the resolved
    /// list.
    ///
    /// A build that overrides the spelling when resolving and then renders the
    /// *supplied* spelling into one of the two surfaces passes a
    /// resolution-level assertion alone.
    ///
    /// Reds on: feeding either surface from the pre-resolution `OwnerSpec`.
    #[test]
    fn owner_canonical_login_replaces_supplied_spelling() {
        let _clock = pinned_clock();
        let mut users = HashMap::new();
        users.insert("alice".to_string(), identity("alice", 7, false));
        let forge = FakeForge {
            users: Some(users),
            refs: HashMap::from([(INDEX_BASE_REF.to_string(), BASE_SHA.to_string())]),
            forbid_mergeability: true,
            ..FakeForge::default()
        };

        let mut req = request(ClaimTarget::Direct);
        req.owners = vec![OwnerSpec::Login("AliCe".to_string())];

        let outcome = block_on(claim(Some(&forge), req)).expect("a resolvable login claims");
        assert_eq!(
            outcome.owners,
            vec![ResolvedOwner {
                login: "alice".to_string(),
                id: 7
            }],
            "the server's canonical spelling and id win over the supplied ones"
        );
        assert_eq!(outcome.owner_identity_source, OwnerIdentitySource::Resolved);

        for call in forge.calls() {
            match call {
                Call::CommitFiles { files, .. } => {
                    let root = String::from_utf8(files.get(ROOT_PATH).expect("the commit carries the root").clone())
                        .expect("the root is UTF-8");
                    assert!(root.contains("\"login\": \"alice\""), "root: {root}");
                    assert!(
                        !root.contains("AliCe"),
                        "the supplied spelling never reaches the root: {root}"
                    );
                }
                Call::OpenOrUpdatePullRequest { body, .. } => {
                    assert!(body.contains("alice:7"), "body: {body}");
                    assert!(!body.contains("AliCe"), "nor the request body: {body}");
                }
                _ => {}
            }
        }
    }

    // ── `--out` ──────────────────────────────────────────────────────────────

    /// A `--out` run is **always** `updated`, and it never reads
    /// `ensure_push_access`.
    ///
    /// Two divergences from announce in one run, both of which a builder copying
    /// announce gets wrong. Claim compares against the open claim **branch**
    /// (a committed root would already have exited 65), and `--out` writes no
    /// branch — so there is nothing for it to be unchanged against. And S-011's
    /// `push-access: skipped` is satisfied by **not calling**
    /// `ensure_push_access`: on GitHub an unreadable `permissions` is exit 80,
    /// not a `skipped` row, so asking the forge for one would refuse the very
    /// unauthenticated `--out` run S-011 describes.
    ///
    /// Reds on: copying announce's committed-root comparison into the `--out` arm
    /// (a repeated `--out` run then reports `unchanged`), or calling
    /// `ensure_push_access` under `--out`.
    #[test]
    fn out_target_is_always_updated_and_skips_the_push_probe() {
        use crate::forge::{CapabilityName, CheckStatus};

        let output = tempfile::TempDir::new().expect("a temp dir is created");
        for run in 0..2 {
            let _clock = pinned_clock();
            let forge = first_claim_forge();

            let outcome = block_on(claim(
                Some(&forge),
                request(ClaimTarget::Out(output.path().to_path_buf())),
            ))
            .expect("the --out run succeeds");

            assert_eq!(
                outcome.status,
                ClaimStatus::Updated,
                "run {run}: --out is always updated"
            );
            assert_eq!(outcome.written_paths, vec![ROOT_PATH.to_string()], "run {run}");
            assert!(outcome.pull_request.is_none(), "run {run}: --out opens nothing");
            assert!(
                !forge.calls().iter().any(|call| matches!(call, Call::EnsurePushAccess)),
                "run {run}: S-011 — the rows come from `skipped_all()`, never from asking the forge"
            );
            assert_eq!(
                outcome.push_access.status(CapabilityName::PushAccess),
                CheckStatus::Skipped,
                "run {run}: and the row still reports, so a pipeline can assert the preflight ran"
            );
            assert!(
                !outcome.push_access.checks().is_empty(),
                "run {run}: non-empty on every run (C-060)"
            );
        }
    }

    /// A `None` forge is refused — `--out` still reads the committed root.
    #[test]
    fn a_forge_is_required_in_every_mode() {
        use crate::cli::ClassifyExitCode;

        let output = tempfile::TempDir::new().expect("a temp dir is created");
        let error = block_on(claim(None, request(ClaimTarget::Out(output.path().to_path_buf()))))
            .expect_err("even --out needs the forge for the C-050 read");
        assert!(matches!(error, ClaimError::ForgeRequired), "{error:?}");
        assert_eq!(
            error.classify(),
            None,
            "an internal wiring fault, not an operator error"
        );
    }

    // ── C-067: no operator free text in the request ─────────────────────────

    /// C-067 — the request title and body are a fixed template over structured
    /// values; no operator free text is interpolated.
    ///
    /// Driven end to end, because that is where the red is reachable: the pure
    /// `request_body` signature cannot see the `--upstream-*` values at all, so a
    /// unit-level negative assertion would be unfalsifiable by construction. Here
    /// the whole `ClaimRequest` is in scope, and the mutation — widening the body
    /// renderer to take the upstream and interpolating the disclaimer into it —
    /// compiles and reds.
    ///
    /// The disclaimer carries `[x](https://evil)` and `@alice` on purpose:
    /// whatever lands in a title or description renders as markdown for the G-04
    /// reviewer, and an `@` fires a mention. The negative half is paired with the
    /// positive one below it — a pure denylist also passes for an empty body.
    ///
    /// The `--upstream-*` values must still reach the **root**, where the
    /// serializer escapes them, so the last assertion is the control that
    /// separates "not interpolated into the request" from "dropped entirely".
    ///
    /// Reds on: interpolating any `--upstream-*` value into the title or the body;
    /// dropping the owners, repository or branch from the body.
    #[test]
    fn request_body_is_a_fixed_template() {
        let _clock = pinned_clock();
        let forge = first_claim_forge();

        let mut req = request(ClaimTarget::Direct);
        req.upstream = Some(Upstream {
            org: "Evil @alice Org".to_string(),
            repository_url: "https://evil.test/[x](https://evil)".to_string().into(),
            disclaimer: Some("[x](https://evil) cc @alice".to_string()),
        });

        block_on(claim(Some(&forge), req)).expect("the claim succeeds");

        let (title, body) = forge
            .calls()
            .into_iter()
            .find_map(|call| match call {
                Call::OpenOrUpdatePullRequest { title, body, .. } => Some((title, body)),
                _ => None,
            })
            .expect("a request was opened");

        for free_text in [
            "[x](https://evil)",
            "@alice",
            "Evil @alice Org",
            "https://evil.test",
            "cc @alice",
        ] {
            assert!(!title.contains(free_text), "operator free text in the title: {title}");
            assert!(!body.contains(free_text), "operator free text in the body: {body}");
        }
        assert!(
            !body.contains('@'),
            "and no `@` at all, so the request fires no mentions: {body}"
        );

        // The positive half: a denylist alone passes for an empty body.
        for structured in [
            "ocx.sh/acme/widget",
            "oci://ghcr.io/acme/widget",
            CLAIM_BRANCH,
            "alice:1234",
        ] {
            assert!(body.contains(structured), "the body must carry {structured}: {body}");
        }

        // The control: the free text does reach the root file, where the
        // serializer escapes it. Absent here, the assertions above would also
        // pass for a build that dropped `--upstream-*` on the floor.
        let root = forge
            .calls()
            .into_iter()
            .find_map(|call| match call {
                Call::CommitFiles { files, .. } => files.get(ROOT_PATH).cloned(),
                _ => None,
            })
            .expect("the commit carries the root");
        let root = String::from_utf8(root).expect("the root is UTF-8");
        assert!(
            root.contains("cc @alice"),
            "the disclaimer reaches the root file: {root}"
        );
    }
}
