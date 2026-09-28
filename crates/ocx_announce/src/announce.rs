// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx package announce` orchestration: updates one package's entry in the
//! `ocx-sh/index` repository (`adr_announce_publisher_surface.md`).
//! "Reference parity" means matching `ocx-sh/index` `bot/cli/announce.py`.
//!
//! The SSRF guard runs before the first registry request; `OCX_ANNOUNCE_TOKEN`
//! reaches only the passed-in [`Forge`](crate::forge::Forge).

pub mod error;
pub(crate) mod pipeline;
pub mod request;

pub use error::AnnounceError;
pub use request::{AnnounceOutcome, AnnounceRequest, AnnounceStatus, AnnounceTarget, TagSelection};

use std::collections::BTreeMap;

use serde_json::Value;

use crate::forge::{
    BranchComparison, CommitBase, FileChange, Forge, Mergeability, PullRequest, PushAccess, RefUpdate, RepoCoordinate,
};
use ocx_index::serialize_root;
use ocx_package::publisher::Publisher;

/// The main branch of the index repository.
const INDEX_BASE_REF: &str = "main";

/// Announce one package.
///
/// # Errors
///
/// [`AnnounceError::ForgeRequired`] when `forge` is `None`, since every mode
/// reads the committed root through it; otherwise an unclaimed package, an
/// SSRF-forbidden host, an unresolvable curated tag, a yank/unyank input error,
/// or any forge or filesystem failure.
pub async fn announce(
    publisher: &Publisher,
    forge: Option<&dyn Forge>,
    request: AnnounceRequest,
) -> Result<AnnounceOutcome, AnnounceError> {
    let forge = forge.ok_or(AnnounceError::ForgeRequired)?;
    let package = request.package.repository().to_string();
    let root_path = format!("p/{package}.json");
    let branch = format!("indexbot-announce-{}", package.replace('/', "-"));
    // Seeded here, not in the one arm that probes, or every other run reports no capability rows.
    let mut capability_checks = PushAccess::skipped_all();

    // Endpoints come from the fork's real identity, since `--fork` may name one renamed away from upstream.
    // Read-only `find_fork`, never `ensure_fork`: an unchanged run must not create a fork.
    let fork_target = match &request.target {
        AnnounceTarget::Fork(target) => Some(target),
        AnnounceTarget::Direct | AnnounceTarget::Out(_) => None,
    };
    let existing_fork = match fork_target {
        Some(target) => forge.find_fork(&request.index_repo, target).await?,
        None => None,
    };
    let branch_repo = match &request.target {
        AnnounceTarget::Direct => Some(request.index_repo.clone()),
        AnnounceTarget::Fork(_) => existing_fork.as_ref().map(|fork| fork.coordinate(&request.index_repo)),
        AnnounceTarget::Out(_) => None,
    };

    let branch_state = resolve_branch_state(forge, &request.index_repo, branch_repo.as_ref(), &branch).await?;

    let root_read = read_committed_root(
        forge,
        &request.index_repo,
        branch_repo.as_ref(),
        &branch_state,
        &root_path,
        &branch,
    )
    .await?;
    let committed_bytes = pipeline::require_root(&package, &root_path, &root_read.base_ref, root_read.bytes)?;
    let mut committed_root: Value =
        serde_json::from_slice(&committed_bytes).map_err(|source| AnnounceError::RootParse {
            path: root_path.clone(),
            source,
        })?;
    if !committed_root.is_object() {
        return Err(AnnounceError::RootNotObject { path: root_path });
    }
    // A root whose `name` differs or is absent is another package's entry, which announcing would rewrite.
    let expected_name = crate::claim::root_name(&request.package);
    let committed_name = committed_root.get("name").and_then(Value::as_str).unwrap_or_default();
    if committed_name != expected_name {
        return Err(AnnounceError::RootNameMismatch {
            path: root_path,
            committed: committed_name.to_string(),
            expected: expected_name,
        });
    }
    // `committed_bytes` stays the base's bytes: comparing against the carried root would report
    // `unchanged` on exactly the stale runs this rebuild must unfreeze.
    if let Some(carried) = &root_read.carried_tags {
        pipeline::carry_branch_tags(&mut committed_root, carried);
    }

    // One timestamp for the whole run, race retry included, so every written object agrees.
    let now = pipeline::current_timestamp();
    let Rebuilt {
        root_bytes: new_root_bytes,
        files,
        observed,
        mut reserved_dropped,
        desc_updated,
    } = observe_and_rebuild(
        publisher,
        forge,
        RootBase {
            root: &committed_root,
            repo: &root_read.repo,
            sha: &root_read.base_sha,
        },
        &request,
        &now,
        &root_path,
        &package,
    )
    .await?;
    let mut desc_status = AnnounceStatus::from_changed(desc_updated);

    let unchanged = new_root_bytes == committed_bytes && pipeline::new_cas_count(&committed_root, &observed) == 0;
    let mut status = if unchanged {
        AnnounceStatus::Unchanged
    } else {
        AnnounceStatus::Updated
    };
    let message = format!("announce: curate {package}");
    let pull_request_body = format!("Publisher-curated tag update for `{package}`.");

    match &request.target {
        AnnounceTarget::Out(directory) => {
            // Writes even when unchanged, or `announce --out dir && publish dir` publishes an empty directory.
            let written_paths = pipeline::write_out(directory, &files).await?;
            Ok(AnnounceOutcome {
                package,
                status,
                pull_request: None,
                fork: None,
                written_paths,
                reserved_tags_dropped: reserved_dropped,
                desc_status,
                branch: String::new(),
                capability_checks,
            })
        }
        AnnounceTarget::Fork(_) | AnnounceTarget::Direct => {
            if unchanged {
                // A prior run may have committed without opening its pull request, stranding the update.
                if let BranchState::Stale(pull_request) = &branch_state {
                    refuse_if_unmergeable(forge, &request.index_repo, &branch_state, &branch).await?;
                    return Ok(AnnounceOutcome {
                        package,
                        status,
                        pull_request: Some(pull_request.clone()),
                        fork: existing_fork,
                        written_paths: Vec::new(),
                        reserved_tags_dropped: reserved_dropped,
                        desc_status,
                        branch: branch.clone(),
                        capability_checks: capability_checks.clone(),
                    });
                }
                // `branch_sha` is `Some` only for `Live`.
                if let Some(repo) = &branch_repo
                    && root_read.branch_sha.is_some()
                {
                    let pull_request = forge
                        .open_or_update_pull_request(
                            &request.index_repo,
                            repo,
                            &branch,
                            INDEX_BASE_REF,
                            &message,
                            &pull_request_body,
                        )
                        .await?;
                    return Ok(AnnounceOutcome {
                        package,
                        status,
                        pull_request: Some(pull_request),
                        fork: existing_fork,
                        written_paths: Vec::new(),
                        reserved_tags_dropped: reserved_dropped,
                        desc_status,
                        branch: branch.clone(),
                        capability_checks: capability_checks.clone(),
                    });
                }
                // No other state carries unmerged work; opening a request here yields a spent branch's unmergeable one.
                return Ok(AnnounceOutcome {
                    package,
                    status,
                    pull_request: None,
                    fork: None,
                    written_paths: Vec::new(),
                    reserved_tags_dropped: reserved_dropped,
                    desc_status,
                    branch: branch.clone(),
                    capability_checks: capability_checks.clone(),
                });
            }
            // After the unchanged return, so a no-op run neither creates a fork nor demands push permission.
            let (commit_repo, fork) = match fork_target {
                Some(target) => {
                    let fork = match existing_fork {
                        Some(fork) => fork,
                        None => forge.ensure_fork(&request.index_repo, Some(&target.namespace)).await?,
                    };
                    let coordinate = fork.coordinate(&request.index_repo);
                    // The commit parents off an upstream SHA the fork may lack until `sync_fork` lands it.
                    forge.sync_fork(&coordinate, INDEX_BASE_REF).await;
                    (coordinate, Some(fork))
                }
                None => {
                    capability_checks = forge.ensure_push_access(&request.index_repo).await?;
                    (request.index_repo.clone(), None)
                }
            };
            // A non-accumulating run bases on upstream `main`, never the fork's lagging copy,
            // which would re-propose content the index already has.
            let (base_repo, base_branch) = match root_read.branch_sha {
                Some(_) => (commit_repo.clone(), branch.as_str()),
                None => (request.index_repo.clone(), INDEX_BASE_REF),
            };
            let base_sha = root_read.base_sha;
            // INFO (the default level) so a lost tag can be traced to the base each announce committed on.
            tracing::info!(
                branch = %branch,
                state = branch_state.name(),
                base_ref = %base_branch,
                base_sha = %base_sha,
                ref_update = ?branch_state.ref_update(),
                "committing the announce"
            );
            let first_attempt = match forge
                .commit_files(
                    &commit_repo,
                    &branch,
                    CommitBase {
                        repo: &base_repo,
                        sha: &base_sha,
                        branch: base_branch,
                    },
                    &message,
                    &files,
                    branch_state.ref_update(),
                )
                .await
            {
                // Under `git` the push happens here, not in `commit_files`, so the race retry must cover this call.
                Ok(_) => {
                    forge
                        .open_or_update_pull_request(
                            &request.index_repo,
                            &commit_repo,
                            &branch,
                            INDEX_BASE_REF,
                            &message,
                            &pull_request_body,
                        )
                        .await
                }
                Err(error) => Err(error),
            };
            let pull_request = match first_attempt {
                Ok(pull_request) => pull_request,
                // One race, two spellings (`api` refuses the fast-forward, `git` loses the lease): match both
                // or one transport exits 75 where the other retries.
                Err(crate::forge::ForgeError::NonFastForward { .. } | crate::forge::ForgeError::StaleLease { .. }) => {
                    // A rebuild lost to `main` moving; re-reading the branch head would make it accumulate-on-stale.
                    let branch_head = match branch_state.ref_update() {
                        // Keyed on `ref_update()`, not `branch_sha` (also `None` for `Absent`), or a raced
                        // `Absent` run re-reads `main` and is refused forever (exit 75).
                        RefUpdate::FastForward | RefUpdate::Accumulate => {
                            forge.get_ref_sha(&commit_repo, &format!("heads/{branch}")).await?
                        }
                        RefUpdate::Reset => None,
                    };
                    // No branch head (a rebuild, or no branch yet): the index `main` is the only head to re-read.
                    let (retry_repo, retry_branch, head_sha) = match branch_head {
                        Some(sha) => (&commit_repo, branch.as_str(), sha),
                        None => {
                            let sha = forge
                                .get_ref_sha(&request.index_repo, &format!("heads/{INDEX_BASE_REF}"))
                                .await?
                                .ok_or_else(|| AnnounceError::MissingBaseRef {
                                    repo: request.index_repo.full_path(),
                                })?;
                            (&request.index_repo, INDEX_BASE_REF, sha)
                        }
                    };
                    let head_bytes = forge
                        .get_file_contents(retry_repo, &root_path, &head_sha)
                        .await?
                        .ok_or_else(|| AnnounceError::MissingHeadRoot {
                            repo: retry_repo.full_path(),
                            path: root_path.clone(),
                            sha: head_sha.clone(),
                        })?;
                    let mut head_root: Value =
                        serde_json::from_slice(&head_bytes).map_err(|source| AnnounceError::RootParse {
                            path: root_path.clone(),
                            source,
                        })?;
                    // Skipping the carry would drop on the retry the stale tags the first attempt carried.
                    if let Some(carried) = &root_read.carried_tags {
                        pipeline::carry_branch_tags(&mut head_root, carried);
                    }
                    let merged = observe_and_rebuild(
                        publisher,
                        forge,
                        RootBase {
                            root: &head_root,
                            repo: retry_repo,
                            sha: &head_sha,
                        },
                        &request,
                        &now,
                        &root_path,
                        &package,
                    )
                    .await?;
                    // Replace, never union: only the retry's drop list describes what was announced.
                    reserved_dropped = merged.reserved_dropped;
                    desc_status = AnnounceStatus::from_changed(merged.desc_updated);
                    // Identical racing announces regenerate the same bytes; committing would push an empty-diff
                    // commit, a governance threat the index bot flags.
                    if merged.root_bytes == head_bytes && pipeline::new_cas_count(&head_root, &merged.observed) == 0 {
                        status = AnnounceStatus::Unchanged;
                        // Commits nothing, so a conflicting stale request would otherwise report a benign `unchanged`.
                        refuse_if_unmergeable(forge, &request.index_repo, &branch_state, &branch).await?;
                    } else {
                        // The retry commits on a different base than the first log line named.
                        tracing::info!(
                            branch = %branch,
                            state = branch_state.name(),
                            base_ref = %retry_branch,
                            base_sha = %head_sha,
                            ref_update = ?branch_state.ref_update(),
                            retry = true,
                            "committing the announce"
                        );
                        forge
                            // Same `ref_update()`: still CAS-checked, since a third announce could advance the branch.
                            .commit_files(
                                &commit_repo,
                                &branch,
                                CommitBase {
                                    repo: retry_repo,
                                    sha: &head_sha,
                                    branch: retry_branch,
                                },
                                &message,
                                &merged.files,
                                branch_state.ref_update(),
                            )
                            .await?;
                    }
                    // One retry only: a second rejection propagates as exit 75 and the caller reruns.
                    forge
                        .open_or_update_pull_request(
                            &request.index_repo,
                            &commit_repo,
                            &branch,
                            INDEX_BASE_REF,
                            &message,
                            &pull_request_body,
                        )
                        .await?
                }
                // Widening the retry to `Err(_)` re-pushes after `MergeRequestUnconfirmed`, whose push already landed.
                Err(other) => return Err(other.into()),
            };
            Ok(AnnounceOutcome {
                package,
                status,
                pull_request: Some(pull_request),
                fork,
                written_paths: Vec::new(),
                reserved_tags_dropped: reserved_dropped,
                desc_status,
                branch: branch.clone(),
                capability_checks: capability_checks.clone(),
            })
        }
    }
}

/// The root a regeneration pass runs against and the tree it was read from, kept
/// together so the orphan diff cannot diff one root and probe another's tree.
#[derive(Clone, Copy)]
struct RootBase<'a> {
    /// The committed root, after the branch-tag carry.
    root: &'a Value,
    /// The repository `sha` lives in.
    repo: &'a RepoCoordinate,
    /// The commit `root` was read at.
    sha: &'a str,
}

/// One regenerated announce payload.
struct Rebuilt {
    root_bytes: Vec<u8>,
    /// The root plus every CAS object, committed as one atomic set.
    files: BTreeMap<String, FileChange>,
    observed: Vec<pipeline::Observed>,
    reserved_dropped: Vec<String>,
    /// Whether the `__ocx.desc` observation moved this pass.
    desc_updated: bool,
}

/// One full regeneration pass over `base.root`, producing the atomic file set.
///
/// The race retry reruns all of it against the winning head: replaying the first
/// pass's tag set would delete a tag the winner added.
///
/// # Errors
///
/// Curated-resolution, observe/SSRF and yank/unyank failures.
async fn observe_and_rebuild(
    publisher: &Publisher,
    forge: &dyn Forge,
    base: RootBase<'_>,
    request: &AnnounceRequest,
    now: &str,
    root_path: &str,
    package: &str,
) -> Result<Rebuilt, AnnounceError> {
    let base_root = base.root;
    let base_tags = pipeline::committed_tag_names(base_root);
    // `repository` is remote-controlled: it passes the SSRF guard before any registry request, tag listing included.
    let repository = base_root
        .get("repository")
        .and_then(Value::as_str)
        .ok_or(AnnounceError::RootMissingField { field: "repository" })?;
    let physical = pipeline::guarded_physical(
        repository,
        request.package.registry(),
        &request.trusted_hosts,
        &request.insecure_hosts,
        &ocx_oci::ssrf::proxy_rules(),
    )
    .await?;
    let discovered = match &request.curated {
        TagSelection::FromRegistry => pipeline::list_registry_tags(publisher, &physical).await?,
        TagSelection::Replace(_) | TagSelection::UnionFile(_) | TagSelection::Refresh => Vec::new(),
    };
    let pipeline::ResolvedTags {
        tags: curated,
        reserved_dropped,
    } = pipeline::resolve_curated_tags(&request.curated, &base_tags, &discovered)?;
    let observed = pipeline::observe_curated(publisher, &physical, &curated).await?;
    let desc = pipeline::observe_desc(publisher, &physical, base_root).await?;
    let mut root = pipeline::regenerate(base_root, &observed, now);
    // A committed tag the regenerated set lacks is a real deletion from the index; additive selections refuse it.
    if matches!(
        request.curated,
        TagSelection::UnionFile(_) | TagSelection::Refresh | TagSelection::FromRegistry
    ) {
        let dropped = pipeline::dropped_committed_tags(&base_tags, &root, &reserved_dropped);
        if !dropped.is_empty() {
            return Err(AnnounceError::CommittedTagsDropped {
                path: root_path.to_string(),
                tags: dropped,
            });
        }
    }
    if let Some(updated) = &desc.desc
        && let Some(object) = root.as_object_mut()
    {
        // The schema requires a `desc` key, so this replaces in place and keeps the key order (preserve_order).
        object.insert("desc".to_string(), updated.clone());
    }
    pipeline::apply_yank_markers(&mut root, &request.yank, &request.unyank, &request.yank_reason, now)?;
    // Diffed against the commit's own base, never a second read, or a concurrent writer's objects get swept.
    let orphans = pipeline::orphan_paths(Some(base_root), &root, package, forge, base.repo, base.sha).await?;
    let root_bytes = serialize_root(&root);
    let files = pipeline::build_files(root_path, &root_bytes, package, &observed, &desc.blobs, &orphans);
    Ok(Rebuilt {
        root_bytes,
        files,
        observed,
        reserved_dropped,
        desc_updated: desc.desc.is_some(),
    })
}

/// Tripwire: refuse an outcome that moves nothing while its open pull request cannot merge.
///
/// A no-op for every state but [`BranchState::Stale`]; [`Mergeability::Unknown`]
/// passes, and the next run re-asks.
///
/// # Errors
///
/// [`AnnounceError::PullRequestUnmergeable`] on a conflict, or any forge failure.
async fn refuse_if_unmergeable(
    forge: &dyn Forge,
    index_repo: &RepoCoordinate,
    branch_state: &BranchState,
    branch: &str,
) -> Result<(), AnnounceError> {
    let BranchState::Stale(pull_request) = branch_state else {
        return Ok(());
    };
    if matches!(
        forge.pull_request_mergeability(index_repo, pull_request.number).await?,
        Mergeability::Conflicting
    ) {
        return Err(AnnounceError::PullRequestUnmergeable {
            number: pull_request.number,
            url: pull_request.html_url.clone(),
            branch: branch.to_string(),
        });
    }
    Ok(())
}

/// The committed root as read: its bytes (`None` when absent) and where they were read.
struct RootRead {
    bytes: Option<Vec<u8>>,
    /// The announce branch head, `Some` only while the branch is `Live`.
    branch_sha: Option<String>,
    /// The commit the bytes were read at, the only sound commit base: re-resolving
    /// the ref could base the commit on a head whose root was never read.
    base_sha: String,
    /// The repository holding `base_sha`; a SHA alone names no tree to read.
    repo: RepoCoordinate,
    base_ref: String,
    /// The stale branch head's `tags`, merged onto the base root so no tag
    /// announced into the open pull request is lost; `None` unless [`BranchState::Stale`].
    carried_tags: Option<Value>,
}

/// What the per-package announce branch is worth; its name outlives every pull
/// request, so the ref existing says nothing on its own.
enum BranchState {
    /// No announce branch — the first announce for this package.
    Absent,
    /// Unmerged commits that fast-forward onto the base: accumulate, so two
    /// announces before a merge share one pull request.
    Live,
    /// Unmerged commits the base moved away from, with a pull request still open.
    ///
    /// Rebuilt on the base with its tag delta carried: reading its head as the
    /// root reproduces a pre-migration shape as a benign `unchanged` forever.
    Stale(PullRequest),
    /// Carries nothing the base needs (merged, or closed unmerged): rebuild from
    /// the base and repoint the ref.
    Spent,
}

impl BranchState {
    /// The state's name, for the commit log line.
    fn name(&self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Live => "live",
            Self::Stale(_) => "stale",
            Self::Spent => "spent",
        }
    }

    /// Whether the branch head may serve as the committed root; a stale one's shape is frozen.
    fn is_live(&self) -> bool {
        matches!(self, Self::Live)
    }

    /// How the ref update behaves: a fast-forward-only repoint of a spent or stale
    /// branch would keep its merged commits or frozen root.
    fn ref_update(&self) -> RefUpdate {
        match self {
            Self::Spent | Self::Stale(_) => RefUpdate::Reset,
            Self::Absent | Self::Live => RefUpdate::FastForward,
        }
    }
}

/// Classify the announce branch.
///
/// Ancestry is asked unconditionally, so an indeterminate compare fails closed
/// on every run. [`BranchComparison::Diverged`] cannot tell squash-merged from
/// unmerged: an open pull request means unmerged ([`BranchState::Stale`]).
async fn resolve_branch_state(
    forge: &dyn Forge,
    index_repo: &RepoCoordinate,
    fork: Option<&RepoCoordinate>,
    branch: &str,
) -> Result<BranchState, AnnounceError> {
    let Some(fork) = fork else {
        return Ok(BranchState::Absent);
    };
    if forge.get_ref_sha(fork, &format!("heads/{branch}")).await?.is_none() {
        return Ok(BranchState::Absent);
    }
    match forge.compare_branch(index_repo, INDEX_BASE_REF, fork, branch).await? {
        // Kept even with no open request; the unchanged path opens the one they never got.
        BranchComparison::Ahead => Ok(BranchState::Live),
        BranchComparison::Identical | BranchComparison::Behind => Ok(BranchState::Spent),
        BranchComparison::Diverged => match forge.find_open_pull_request(index_repo, fork, branch).await? {
            Some(pull_request) => Ok(BranchState::Stale(pull_request)),
            None => Ok(BranchState::Spent),
        },
    }
}

/// Read the committed root from the announce branch head while [`BranchState::Live`], else the index `main`.
///
/// `branch_repo` is the verified coordinate the branch lives in (`None` for
/// `--out` and a fork target with no fork yet). Liveness comes from the caller's
/// `branch_state`, never re-asked, so it agrees with the commit's base.
///
/// # Errors
///
/// [`AnnounceError::MissingBaseRef`] when the index base ref does not resolve,
/// [`AnnounceError::RootParse`] when a stale branch head's root is not JSON, or
/// any forge failure.
async fn read_committed_root(
    forge: &dyn Forge,
    index_repo: &RepoCoordinate,
    branch_repo: Option<&RepoCoordinate>,
    branch_state: &BranchState,
    root_path: &str,
    branch: &str,
) -> Result<RootRead, AnnounceError> {
    if let Some(repo) = branch_repo.filter(|_| branch_state.is_live())
        && let Some(branch_sha) = forge.get_ref_sha(repo, &format!("heads/{branch}")).await?
    {
        let bytes = forge.get_file_contents(repo, root_path, &branch_sha).await?;
        return Ok(RootRead {
            bytes,
            branch_sha: Some(branch_sha.clone()),
            base_sha: branch_sha,
            repo: repo.clone(),
            base_ref: branch.to_string(),
            carried_tags: None,
        });
    }
    let base_sha = forge
        .get_ref_sha(index_repo, &format!("heads/{INDEX_BASE_REF}"))
        .await?
        .ok_or_else(|| AnnounceError::MissingBaseRef {
            repo: index_repo.full_path(),
        })?;
    let bytes = forge.get_file_contents(index_repo, root_path, &base_sha).await?;
    // Only `tags` from a stale branch; every other key of its root may be pre-migration.
    let carried_tags = match (branch_repo, branch_state) {
        (Some(repo), BranchState::Stale(_)) => branch_head_tags(forge, repo, root_path, branch).await?,
        _ => None,
    };
    Ok(RootRead {
        bytes,
        branch_sha: None,
        base_sha,
        repo: index_repo.clone(),
        base_ref: INDEX_BASE_REF.to_string(),
        carried_tags,
    })
}

/// The `tags` object of the announce branch head's root.
///
/// `None` when the ref, the file or the key is absent: failing instead would keep
/// the package frozen.
///
/// # Errors
///
/// [`AnnounceError::RootParse`] when the branch head's root is not valid JSON,
/// or any forge failure.
async fn branch_head_tags(
    forge: &dyn Forge,
    repo: &RepoCoordinate,
    root_path: &str,
    branch: &str,
) -> Result<Option<Value>, AnnounceError> {
    let Some(branch_sha) = forge.get_ref_sha(repo, &format!("heads/{branch}")).await? else {
        return Ok(None);
    };
    let Some(bytes) = forge.get_file_contents(repo, root_path, &branch_sha).await? else {
        return Ok(None);
    };
    let root: Value = serde_json::from_slice(&bytes).map_err(|source| AnnounceError::RootParse {
        path: root_path.to_string(),
        source,
    })?;
    Ok(root.get("tags").filter(|tags| tags.is_object()).cloned())
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, VecDeque};
    use std::sync::Mutex;

    use super::*;
    use crate::forge::{CapabilityName, CheckStatus, ForgeError, ForgeIdentity, ForkIdentity};

    use ocx_oci::client::test_transport::{StubTransport, StubTransportData};

    /// A committed root for `acme/widget` on a loopback physical repository,
    /// carrying `tags` verbatim.
    fn committed_root(tags: Value) -> Value {
        serde_json::json!({
            "name": "ocx.sh/acme/widget",
            "repository": "oci://127.0.0.1/x",
            "owners": [{ "github": "alice", "github_id": 1 }],
            "status": "active",
            "created": "2026-07-24",
            "desc": null,
            "tags": tags,
        })
    }

    /// The index repository every fixture announces into.
    fn index_repo() -> RepoCoordinate {
        RepoCoordinate {
            host: None,
            namespace: "ocx-sh".to_string(),
            project: "index".to_string(),
        }
    }

    fn request(curated: TagSelection) -> AnnounceRequest {
        AnnounceRequest {
            package: ocx_oci::PackageRef::new_registry("acme/widget", "ocx.sh"),
            curated,
            target: AnnounceTarget::Out(std::path::PathBuf::from("unused")),
            index_repo: index_repo(),
            yank: Vec::new(),
            unyank: Vec::new(),
            yank_reason: String::new(),
            // The loopback physical host is forbidden by default; trusting it
            // is what lets the observe loop reach the stub transport.
            trusted_hosts: vec!["127.0.0.1".to_string()],
            // No plain-HTTP allowance: the fixture's dial scheme is https, so
            // the guard's route decision does not depend on the ambient
            // environment.
            insecure_hosts: Vec::new(),
        }
    }

    /// Seed the stub with a one-platform image index served at
    /// `127.0.0.1/x:<tag>`, returning the digest it resolves to.
    ///
    /// Pretty-printed for the same reason as `pipeline::tests::seed_manifest`:
    /// the served encoding must differ from serde's canonical one, or a
    /// re-serializing regression stays invisible to every byte assertion.
    ///
    /// The digest is returned because a **C6 no-op needs it**: a committed root
    /// whose tag already records exactly this digest is one that re-observes
    /// byte-identically, which is the only honest way to drive an unchanged run.
    fn seed_index(data: &StubTransportData, tag: &str) -> String {
        let manifest = ocx_oci::Manifest::ImageIndex(ocx_oci::ImageIndex {
            schema_version: ocx_oci::INDEX_SCHEMA_VERSION,
            media_type: Some(ocx_oci::OCI_IMAGE_INDEX_MEDIA_TYPE.to_string()),
            artifact_type: None,
            manifests: vec![ocx_oci::ImageIndexEntry {
                media_type: ocx_oci::OCI_IMAGE_MEDIA_TYPE.to_string(),
                digest: format!("sha256:{}", "a".repeat(64)),
                size: 0,
                platform: Some(ocx_oci::native::Platform {
                    architecture: "amd64".into(),
                    os: "linux".into(),
                    os_version: None,
                    os_features: None,
                    variant: None,
                    features: None,
                }),
                artifact_type: None,
                annotations: None,
            }],
            annotations: None,
        });
        let bytes = serde_json::to_vec_pretty(&manifest).expect("index serializes");
        let digest = ocx_oci::Algorithm::Sha256.hash(&bytes);
        data.write()
            .manifests
            .insert(format!("127.0.0.1/x:{tag}"), (bytes, digest.to_string()));
        digest.to_string()
    }

    /// Seed a `__ocx.desc` artifact (readme layer only) at `127.0.0.1/x`, and
    /// return the readme bytes' own hex digest — the CAS name the announce must
    /// write it under.
    fn seed_description(data: &StubTransportData) -> String {
        let readme = b"# widget\n".as_slice();
        let readme_digest = ocx_oci::Algorithm::Sha256.hash(readme);
        data.write().blobs.insert(readme_digest.to_string(), readme.to_vec());
        let manifest = ocx_oci::Manifest::Image(ocx_oci::ImageManifest {
            artifact_type: Some(ocx_oci::media_type::MEDIA_TYPE_DESCRIPTION_V1.to_string()),
            layers: vec![ocx_oci::Descriptor {
                media_type: ocx_oci::media_type::MEDIA_TYPE_MARKDOWN.to_string(),
                digest: readme_digest.to_string(),
                size: i64::try_from(readme.len()).expect("test blob fits i64"),
                urls: None,
                artifact_type: None,
                annotations: None,
            }],
            annotations: Some([(ocx_oci::annotations::TITLE.to_string(), "Widget".to_string())].into()),
            ..Default::default()
        });
        let bytes = serde_json::to_vec_pretty(&manifest).expect("manifest serializes");
        let digest = ocx_oci::Algorithm::Sha256.hash(&bytes);
        data.write()
            .manifests
            .insert("127.0.0.1/x:__ocx.desc".to_string(), (bytes, digest.to_string()));
        readme_digest.hex().to_string()
    }

    /// The commit the fixtures' base root is read at. Only the orphan diff's
    /// logo probe reads anything there, and [`FakeForge`] answers by ref, so a
    /// fixture that seeds no root at this sha probes "absent" — which is what
    /// every pass below wants, none of them having a logo.
    const BASE_SHA: &str = "base-sha";

    /// One regeneration pass over `root`, read at [`BASE_SHA`] in the index
    /// repository — the shape every pass fixture wants, with the timestamp and
    /// the package pinned so a row states only what it varies.
    async fn rebuild(
        publisher: &Publisher,
        forge: &FakeForge,
        root: &Value,
        request: &AnnounceRequest,
    ) -> Result<Rebuilt, AnnounceError> {
        observe_and_rebuild(
            publisher,
            forge,
            RootBase {
                root,
                repo: &request.index_repo,
                sha: BASE_SHA,
            },
            request,
            "2026-07-25T00:00:00Z",
            ROOT_PATH,
            "acme/widget",
        )
        .await
    }

    /// The `repository` lift moved out of `extract_physical` and into the pass,
    /// and a field lift is exactly the kind of refactor that drops the refusal
    /// it used to carry: a root with no pointer names no registry, so it must
    /// still fail rather than resolve one from somewhere else.
    ///
    /// No registry is seeded and none is reached — the refusal is ahead of the
    /// SSRF pre-flight, which is ahead of every request.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_root_without_a_repository_pointer_is_still_refused() {
        let publisher = Publisher::new(ocx_oci::Client::with_transport(Box::new(StubTransport::new(
            StubTransportData::new(),
        ))));
        let root = serde_json::json!({ "name": "ocx.sh/acme/widget", "tags": {} });

        let Err(error) = rebuild(
            &publisher,
            &FakeForge::new(),
            &root,
            &request(TagSelection::Replace(vec!["1.0.0".to_string()])),
        )
        .await
        else {
            panic!("a root carrying no repository pointer must be refused, not regenerated");
        };

        assert!(
            matches!(error, AnnounceError::RootMissingField { field: "repository" }),
            "the refusal names the absent field, got {error:?}"
        );
    }

    /// A moved description is written back into the root's EXISTING `desc` slot,
    /// not appended: the index CI re-serializes the committed root and rejects
    /// any byte that differs, so a `desc` landing after `tags` fails the PR.
    /// Its readme rides along in the same atomic file set (C15).
    #[tokio::test(flavor = "multi_thread")]
    async fn a_moved_description_rewrites_desc_in_place_and_carries_its_readme() {
        let data = StubTransportData::new();
        seed_index(&data, "1.0.0");
        let readme_hex = seed_description(&data);
        let publisher = Publisher::new(ocx_oci::Client::with_transport(Box::new(StubTransport::new(data))));
        let root = committed_root(serde_json::json!({}));

        let rebuilt = rebuild(
            &publisher,
            &FakeForge::new(),
            &root,
            &request(TagSelection::Replace(vec!["1.0.0".to_string()])),
        )
        .await
        .expect("the description observes alongside the tag");

        assert!(rebuilt.desc_updated, "the description moved from null to an object");
        let announced: Value = serde_json::from_slice(&rebuilt.root_bytes).expect("the root is JSON");
        let fields: Vec<&str> = announced
            .as_object()
            .expect("the root is an object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            fields,
            vec!["name", "repository", "owners", "status", "created", "desc", "tags"],
            "desc keeps its committed position — CONTRACTS field order is a wire contract"
        );
        assert!(announced["desc"]["digest"].is_string(), "desc is no longer null");
        assert!(
            rebuilt
                .files
                .contains_key(&format!("p/acme/widget/o/sha256/{readme_hex}.md")),
            "the readme blob ships in the same commit as the root: {:?}",
            rebuilt.files.keys().collect::<Vec<_>>()
        );
    }

    /// The D7 drop list is threaded out of the regeneration pass, not recomputed
    /// or dropped on the floor: every `AnnounceOutcome` construction site reads
    /// it from here, and the retry pass supersedes it from the same field.
    #[tokio::test(flavor = "multi_thread")]
    async fn regeneration_threads_the_reserved_drops_out_of_the_pass() {
        let data = StubTransportData::new();
        seed_index(&data, "1.0.0");
        let publisher = Publisher::new(ocx_oci::Client::with_transport(Box::new(StubTransport::new(data))));
        let keep = format!("__ocx.keep.sha256-{}", "a".repeat(64));
        let root = committed_root(serde_json::json!({}));
        let request = request(TagSelection::Replace(vec![
            "1.0.0".to_string(),
            "__ocx.desc".to_string(),
            keep.clone(),
        ]));

        let rebuilt = rebuild(&publisher, &FakeForge::new(), &root, &request)
            .await
            .expect("the one real version announces");

        assert_eq!(
            rebuilt.reserved_dropped,
            vec!["__ocx.desc".to_string(), keep],
            "both reserved tags ride out of the pass"
        );
        assert_eq!(rebuilt.observed.len(), 1, "only the real version was observed");
        assert_eq!(rebuilt.observed[0].tag, "1.0.0");
    }

    /// A reserved tag never reaches the registry: it is dropped at resolution,
    /// so no round trip is spent on a tag that cannot be a version.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_reserved_tag_costs_no_registry_round_trip() {
        let data = StubTransportData::new();
        seed_index(&data, "1.0.0");
        let publisher = Publisher::new(ocx_oci::Client::with_transport(Box::new(StubTransport::new(
            data.clone(),
        ))));
        let root = committed_root(serde_json::json!({}));
        let request = request(TagSelection::Replace(vec![
            "1.0.0".to_string(),
            "__ocx.desc".to_string(),
        ]));

        rebuild(&publisher, &FakeForge::new(), &root, &request)
            .await
            .expect("the one real version announces");

        let manifest_pulls = data.read().calls.iter().filter(|c| *c == "pull_manifest_raw").count();
        assert_eq!(
            manifest_pulls,
            1,
            "exactly one tag was fetched: {:?}",
            data.read().calls
        );
    }

    // ── --tags-from-registry ─────────────────────────────────────────────────

    /// The registry supplies the candidates: a tag the physical repository
    /// serves but the committed root has never carried gets announced.
    #[tokio::test(flavor = "multi_thread")]
    async fn from_registry_discovers_a_tag_the_committed_root_lacks() {
        let data = StubTransportData::new();
        seed_index(&data, "1.0.0");
        seed_index(&data, "2.0.0");
        data.write().tags = vec![vec!["1.0.0".to_string(), "2.0.0".to_string()]];
        let publisher = Publisher::new(ocx_oci::Client::with_transport(Box::new(StubTransport::new(data))));
        let root = committed_root(serde_json::json!({
            "1.0.0": { "content": format!("sha256:{}", "b".repeat(64)), "observed": "2026-07-01T00:00:00Z" }
        }));

        let rebuilt = rebuild(
            &publisher,
            &FakeForge::new(),
            &root,
            &request(TagSelection::FromRegistry),
        )
        .await
        .expect("both registry tags announce");

        let observed: Vec<&str> = rebuilt.observed.iter().map(|entry| entry.tag.as_str()).collect();
        assert_eq!(
            observed,
            vec!["1.0.0", "2.0.0"],
            "the committed tag first, then the one only the registry knew"
        );
    }

    /// X3 ordering, and the reason the pre-flight had to move: under
    /// `--tags-from-registry` the **tag listing** is the first registry request,
    /// so a pre-flight left inside the observe loop would have guarded nothing.
    /// A forbidden host must therefore cost zero registry calls of any kind —
    /// not merely zero observes. The `__ocx.desc` probe is the third kind of
    /// request in the pass and is seeded here so a mis-ordered one would show up
    /// as a recorded call rather than a silent absence.
    #[tokio::test(flavor = "multi_thread")]
    async fn from_registry_refuses_a_forbidden_host_before_listing_any_tag() {
        let data = StubTransportData::new();
        data.write().tags = vec![vec!["1.0.0".to_string()]];
        seed_description(&data);
        let publisher = Publisher::new(ocx_oci::Client::with_transport(Box::new(StubTransport::new(
            data.clone(),
        ))));
        let root = committed_root(serde_json::json!({}));
        // The default `request` trusts loopback; this one does not, so the
        // committed root's `oci://127.0.0.1/x` pointer is forbidden.
        let mut request = request(TagSelection::FromRegistry);
        request.trusted_hosts = Vec::new();

        let result = rebuild(&publisher, &FakeForge::new(), &root, &request).await;

        assert!(
            matches!(result, Err(AnnounceError::Ssrf { .. })),
            "a forbidden physical host must be refused"
        );
        let inner = data.read();
        assert!(
            inner.calls.is_empty(),
            "no registry call — listing included — may precede the SSRF refusal: {:?}",
            inner.calls
        );
        assert!(inner.auth_calls.is_empty(), "no auth call may precede the SSRF refusal");
    }

    // ── A scripted forge ─────────────────────────────────────────────────────

    /// The announce branch every fixture below writes to, and the two refs the
    /// orchestration resolves.
    const BRANCH: &str = "indexbot-announce-acme-widget";
    const BRANCH_REF: &str = "heads/indexbot-announce-acme-widget";
    const MAIN_REF: &str = "heads/main";

    /// The one root path every fixture announces — the path [`FakeForge`]
    /// answers out of its `roots` map, so a read of anything else is a read of
    /// a CAS object and is answered from `files`.
    const ROOT_PATH: &str = "p/acme/widget.json";

    /// A scripted [`Forge`] that records every call.
    ///
    /// The workspace's only two `impl Forge` are REST clients, and
    /// [`announce`] takes `&dyn Forge`, so nothing here could drive the
    /// orchestration end to end without this. It **counts** calls as well as
    /// answering them because most of what has to be proved is *how many times*
    /// a method ran: [`ForgeError::NonFastForward`] and
    /// [`ForgeError::StaleLease`] classify to the same exit code, so a retry
    /// that silently never fires for one of them is invisible to any assertion
    /// on the outcome alone.
    ///
    /// The methods announce must never reach panic instead of answering, which
    /// makes their absence an assertion rather than a silence.
    struct FakeForge {
        /// `<ref>` → the shas successive [`Forge::get_ref_sha`] calls answer
        /// with, the last entry repeating. A two-entry ref is how "the base
        /// moved between the first read and the retry's re-read" is expressed;
        /// an absent key is a ref that does not exist.
        refs: HashMap<String, Vec<Option<String>>>,
        /// Commit sha → the root bytes [`Forge::get_file_contents`] serves at
        /// it. An absent sha is an absent file, which is what an unclaimed
        /// namespace looks like from here.
        roots: HashMap<String, Vec<u8>>,
        /// `(commit sha, path)` → the bytes served there, for every path that
        /// is not the root. Keyed by path as well as ref because the orphan
        /// diff's logo probe asks for one exact object and takes the answer as
        /// "this extension is the one on disk": a fixture that served the root
        /// at every path would report both `.png` and `.svg` present.
        files: HashMap<(String, String), Vec<u8>>,
        /// Every read of a non-root path fails — the transport fault the orphan
        /// probe must propagate rather than read as "no such object".
        failing_reads: bool,
        comparison: BranchComparison,
        open_request: Option<PullRequest>,
        mergeability: Mergeability,
        fork: Option<ForkIdentity>,
        state: Mutex<FakeState>,
    }

    /// What the double has recorded, and what it has left to answer with.
    #[derive(Default)]
    struct FakeState {
        /// How many times each `<ref>` has been resolved so far.
        ref_reads: HashMap<String, usize>,
        /// Scripted [`Forge::commit_files`] failures, consumed in order. `None`
        /// — and an exhausted queue — means the call succeeds.
        commit_failures: VecDeque<Option<ForgeError>>,
        /// The same for [`Forge::open_or_update_pull_request`].
        open_failures: VecDeque<Option<ForgeError>>,
        /// One entry per `commit_files` call: the atomic file set it carried and
        /// the ref update it asked for.
        commits: Vec<(BTreeMap<String, FileChange>, RefUpdate)>,
        opens: usize,
        push_access_probes: usize,
        mergeability_reads: usize,
    }

    impl FakeForge {
        /// A forge that knows no ref and no root: every read answers "absent".
        fn new() -> Self {
            Self {
                refs: HashMap::new(),
                roots: HashMap::new(),
                files: HashMap::new(),
                failing_reads: false,
                comparison: BranchComparison::Ahead,
                open_request: None,
                mergeability: Mergeability::Mergeable,
                fork: None,
                state: Mutex::new(FakeState::default()),
            }
        }

        /// `r#ref` resolves to `shas` in call order; the last entry repeats.
        fn with_ref(mut self, r#ref: &str, shas: &[Option<&str>]) -> Self {
            self.refs.insert(
                r#ref.to_string(),
                shas.iter().map(|sha| sha.map(ToString::to_string)).collect(),
            );
            self
        }

        /// The canonical bytes of `root` are served at commit `sha` — canonical
        /// because that is what a committed root is, and the C6 byte comparison
        /// reads it directly.
        fn with_root(mut self, sha: &str, root: &Value) -> Self {
            self.roots.insert(sha.to_string(), serialize_root(root));
            self
        }

        /// `path` exists at commit `sha` — the fixture for an object the orphan
        /// probe can find.
        fn with_file(mut self, sha: &str, path: &str, bytes: &[u8]) -> Self {
            self.files.insert((sha.to_string(), path.to_string()), bytes.to_vec());
            self
        }

        /// Every object read fails with a server error.
        fn failing_reads(mut self) -> Self {
            self.failing_reads = true;
            self
        }

        /// A diverged branch under an open request — [`BranchState::Stale`].
        fn stale_with(mut self, pull_request: PullRequest) -> Self {
            self.comparison = BranchComparison::Diverged;
            self.open_request = Some(pull_request);
            self
        }

        fn with_mergeability(mut self, mergeability: Mergeability) -> Self {
            self.mergeability = mergeability;
            self
        }

        fn with_fork(mut self, fork: ForkIdentity) -> Self {
            self.fork = Some(fork);
            self
        }

        fn failing_commits(mut self, failures: Vec<Option<ForgeError>>) -> Self {
            self.state
                .get_mut()
                .expect("the fixture lock is uncontended")
                .commit_failures = failures.into();
            self
        }

        fn failing_opens(mut self, failures: Vec<Option<ForgeError>>) -> Self {
            self.state
                .get_mut()
                .expect("the fixture lock is uncontended")
                .open_failures = failures.into();
            self
        }

        /// One entry per `commit_files` call, in call order.
        fn commits(&self) -> Vec<(BTreeMap<String, FileChange>, RefUpdate)> {
            self.state
                .lock()
                .expect("the fixture lock is uncontended")
                .commits
                .clone()
        }

        fn opens(&self) -> usize {
            self.state.lock().expect("the fixture lock is uncontended").opens
        }

        fn push_access_probes(&self) -> usize {
            self.state
                .lock()
                .expect("the fixture lock is uncontended")
                .push_access_probes
        }

        fn mergeability_reads(&self) -> usize {
            self.state
                .lock()
                .expect("the fixture lock is uncontended")
                .mergeability_reads
        }
    }

    /// The root bytes one recorded `commit_files` call carried.
    fn committed_root_bytes(commit: &(BTreeMap<String, FileChange>, RefUpdate)) -> String {
        let FileChange::Put(bytes) = commit
            .0
            .get("p/acme/widget.json")
            .expect("every commit carries the root")
        else {
            panic!("the root is written, never deleted")
        };
        String::from_utf8(bytes.clone()).expect("the root is UTF-8")
    }

    #[async_trait::async_trait]
    impl Forge for FakeForge {
        async fn authenticated_identity(&self) -> Result<Option<ForgeIdentity>, ForgeError> {
            unreachable!("announce resolves no identity — that is the claim command's ladder")
        }

        async fn resolve_user(&self, _login: &str) -> Result<Option<ForgeIdentity>, ForgeError> {
            unreachable!("announce resolves no login")
        }

        async fn get_file_contents(
            &self,
            _repo: &RepoCoordinate,
            path: &str,
            r#ref: &str,
        ) -> Result<Option<Vec<u8>>, ForgeError> {
            if path == ROOT_PATH {
                return Ok(self.roots.get(r#ref).cloned());
            }
            if self.failing_reads {
                return Err(ForgeError::Status {
                    url: path.to_string(),
                    status: 500,
                    detail: "the fixture refuses every object read".to_string(),
                });
            }
            Ok(self.files.get(&(r#ref.to_string(), path.to_string())).cloned())
        }

        async fn get_ref_sha(&self, _repo: &RepoCoordinate, r#ref: &str) -> Result<Option<String>, ForgeError> {
            let position = {
                let mut state = self.state.lock().expect("the fixture lock is uncontended");
                let reads = state.ref_reads.entry(r#ref.to_string()).or_default();
                let position = *reads;
                *reads += 1;
                position
            };
            let Some(answers) = self.refs.get(r#ref) else {
                return Ok(None);
            };
            Ok(answers.get(position).or_else(|| answers.last()).cloned().flatten())
        }

        async fn compare_branch(
            &self,
            _repo: &RepoCoordinate,
            _base: &str,
            _head: &RepoCoordinate,
            _head_branch: &str,
        ) -> Result<BranchComparison, ForgeError> {
            Ok(self.comparison)
        }

        async fn find_open_pull_request(
            &self,
            _index: &RepoCoordinate,
            _head: &RepoCoordinate,
            _branch: &str,
        ) -> Result<Option<PullRequest>, ForgeError> {
            Ok(self.open_request.clone())
        }

        async fn pull_request_mergeability(
            &self,
            _index: &RepoCoordinate,
            _number: u64,
        ) -> Result<Mergeability, ForgeError> {
            self.state
                .lock()
                .expect("the fixture lock is uncontended")
                .mergeability_reads += 1;
            Ok(self.mergeability)
        }

        async fn find_fork(
            &self,
            _upstream: &RepoCoordinate,
            _fork: &RepoCoordinate,
        ) -> Result<Option<ForkIdentity>, ForgeError> {
            Ok(self.fork.clone())
        }

        async fn ensure_fork(
            &self,
            _upstream: &RepoCoordinate,
            _target_owner: Option<&str>,
        ) -> Result<ForkIdentity, ForgeError> {
            unreachable!("every fork fixture here resolves an existing fork, so C6 provokes no create")
        }

        async fn sync_fork(&self, _fork: &RepoCoordinate, _branch: &str) {}

        async fn ensure_push_access(&self, _repo: &RepoCoordinate) -> Result<PushAccess, ForgeError> {
            self.state
                .lock()
                .expect("the fixture lock is uncontended")
                .push_access_probes += 1;
            // A row only a real probe can produce, so "the outcome was seeded"
            // and "the outcome was filled in by the preflight" are different
            // observable states rather than the same all-skipped array.
            let mut access = PushAccess::skipped_all();
            access.record(CapabilityName::PushAccess, CheckStatus::Passed, None);
            Ok(access)
        }

        async fn commit_files(
            &self,
            _repo: &RepoCoordinate,
            _branch: &str,
            _base: CommitBase<'_>,
            _message: &str,
            files: &BTreeMap<String, FileChange>,
            update: RefUpdate,
        ) -> Result<String, ForgeError> {
            let mut state = self.state.lock().expect("the fixture lock is uncontended");
            state.commits.push((files.clone(), update));
            let sequence = state.commits.len();
            if let Some(Some(failure)) = state.commit_failures.pop_front() {
                return Err(failure);
            }
            Ok(format!("commit-{sequence}"))
        }

        async fn open_or_update_pull_request(
            &self,
            _index: &RepoCoordinate,
            _head: &RepoCoordinate,
            _branch: &str,
            _base: &str,
            _title: &str,
            _body: &str,
        ) -> Result<PullRequest, ForgeError> {
            let mut state = self.state.lock().expect("the fixture lock is uncontended");
            state.opens += 1;
            if let Some(Some(failure)) = state.open_failures.pop_front() {
                return Err(failure);
            }
            Ok(PullRequest {
                number: 7,
                html_url: "https://example.invalid/pull/7".to_string(),
                updated: false,
            })
        }
    }

    // ── orphan_paths: the index drops what the new root stopped naming ───────

    /// A 64-hex digest string, told apart by its fill character.
    fn digest(fill: char) -> String {
        format!("sha256:{}", fill.to_string().repeat(64))
    }

    /// The CAS path `digest` lands at under this package.
    fn object(digest: &str, extension: &str) -> String {
        let hex = digest.trim_start_matches("sha256:");
        format!("p/acme/widget/o/sha256/{hex}.{extension}")
    }

    /// A root referencing `tags` by content digest, plus an optional
    /// description — the two halves of a root's referenced set.
    fn referencing(tags: &[String], description: Value) -> Value {
        let tags: serde_json::Map<String, Value> = tags
            .iter()
            .enumerate()
            .map(|(index, content)| (format!("{index}.0.0"), serde_json::json!({ "content": content })))
            .collect();
        serde_json::json!({ "tags": tags, "desc": description })
    }

    /// The orphan set between two roots, read at [`BASE_SHA`] in the index.
    async fn orphans(
        forge: &FakeForge,
        previous: Option<&Value>,
        new_root: &Value,
    ) -> Result<Vec<String>, AnnounceError> {
        pipeline::orphan_paths(previous, new_root, "acme/widget", forge, &index_repo(), BASE_SHA).await
    }

    /// The mandate's core case: a digest moved, so the object nobody reaches
    /// any more leaves the index in the same commit that stops reaching it —
    /// and the tag that did not move keeps its object, which is what says the
    /// diff is a diff and not a sweep of everything the previous root held.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_moved_tag_orphans_its_old_object_and_spares_the_unmoved_one() {
        let previous = referencing(&[digest('a'), digest('b')], Value::Null);
        let new_root = referencing(&[digest('c'), digest('b')], Value::Null);

        let paths = orphans(&FakeForge::new(), Some(&previous), &new_root)
            .await
            .expect("a tag's extension is known, so nothing is probed");

        assert_eq!(
            paths,
            vec![object(&digest('a'), "json")],
            "only the abandoned digest's object is dropped"
        );
    }

    /// A description edit moves the readme's own digest, and the markdown blob
    /// the old one named is as unreachable as an abandoned dispatch object.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_changed_readme_orphans_the_markdown_blob() {
        let previous = referencing(&[], serde_json::json!({ "readme": digest('a') }));
        let new_root = referencing(&[], serde_json::json!({ "readme": digest('b') }));

        let paths = orphans(&FakeForge::new(), Some(&previous), &new_root)
            .await
            .expect("a readme's extension is known too");

        assert_eq!(paths, vec![object(&digest('a'), "md")]);
    }

    /// The logo is the one reference whose extension the root does not record,
    /// so the old object is found by probing `.png` then `.svg`. A publisher
    /// who replaces a PNG logo with an SVG leaves the PNG behind, and this is
    /// the read that finds it.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_logo_that_changed_format_is_found_by_probing_the_parent_commit() {
        let previous = referencing(&[], serde_json::json!({ "readme": digest('a'), "logo": digest('c') }));
        let new_root = referencing(&[], serde_json::json!({ "readme": digest('a'), "logo": digest('d') }));
        let forge = FakeForge::new().with_file(BASE_SHA, &object(&digest('c'), "png"), b"\x89PNG");

        let paths = orphans(&forge, Some(&previous), &new_root)
            .await
            .expect("the probe answers from the parent commit");

        assert_eq!(
            paths,
            vec![object(&digest('c'), "png")],
            "the extension comes from the tree, not from a guess"
        );
    }

    /// Neither extension answering means the object was never committed —
    /// nothing to delete, and a `Delete` naming it would be noise in every
    /// commit. The readme beside it still goes, so this row is not passing
    /// because the whole diff was empty.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_logo_no_commit_ever_wrote_is_skipped() {
        let previous = referencing(&[], serde_json::json!({ "readme": digest('a'), "logo": digest('c') }));
        let new_root = referencing(&[], Value::Null);

        let paths = orphans(&FakeForge::new(), Some(&previous), &new_root)
            .await
            .expect("an absent object is not an error");

        assert_eq!(paths, vec![object(&digest('a'), "md")], "only the readme is nameable");
    }

    /// A failed probe is not an absent object. Reading the failure as "no logo
    /// here" would leave the orphan behind and report a clean sweep, so the run
    /// stops and says the read failed.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_logo_probe_propagates_rather_than_skipping() {
        let previous = referencing(&[], serde_json::json!({ "readme": digest('a'), "logo": digest('c') }));
        let new_root = referencing(&[], Value::Null);

        let result = orphans(&FakeForge::new().failing_reads(), Some(&previous), &new_root).await;

        assert!(
            matches!(result, Err(AnnounceError::Forge(_))),
            "the transport failure reaches the caller, got {result:?}"
        );
    }

    /// A fresh claim has no previous root, so there is nothing to diff against
    /// and no reason to spend a round trip. The forge refuses every read here:
    /// the `Ok` is only reachable if no probe was made.
    #[tokio::test(flavor = "multi_thread")]
    async fn no_previous_root_orphans_nothing_without_probing() {
        let new_root = referencing(&[], serde_json::json!({ "readme": digest('a'), "logo": digest('c') }));

        let paths = orphans(&FakeForge::new().failing_reads(), None, &new_root)
            .await
            .expect("no probe is made, so the refusing forge is never asked");

        assert!(
            paths.is_empty(),
            "nothing was referenced before, so nothing is orphaned"
        );
    }

    /// A root document is remote data, and its digest strings become path
    /// components. Anything that is not a well-formed `<algorithm>:<hex>` is
    /// dropped before it can be joined into one — a traversal in a `content`
    /// field names no file this run may delete.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_malformed_digest_never_becomes_a_path() {
        let previous = serde_json::json!({
            "tags": {
                "1.0.0": { "content": "../../../p/other/widget.json" },
                "2.0.0": { "content": "sha256:../x" },
            },
            "desc": { "readme": "not-a-digest-at-all", "logo": "sha256:zz" },
        });
        let new_root = referencing(&[], Value::Null);

        let paths = orphans(&FakeForge::new().failing_reads(), Some(&previous), &new_root)
            .await
            .expect("a malformed logo reference is dropped before it can be probed");

        assert!(paths.is_empty(), "no unparseable reference produced a path: {paths:?}");
    }

    /// The default [`request`] with a different write target.
    fn request_to(curated: TagSelection, target: AnnounceTarget) -> AnnounceRequest {
        AnnounceRequest {
            target,
            ..request(curated)
        }
    }

    /// Serve `tags` as one-platform image indices on the loopback physical
    /// repository, and return the digest they all resolve to.
    ///
    /// One digest for every tag is not a simplification: [`seed_index`] builds
    /// the same manifest whatever the tag, so the fixture genuinely serves one
    /// blob under several names.
    fn seed_tags(tags: &[&str]) -> (StubTransportData, String) {
        let data = StubTransportData::new();
        let mut digest = String::new();
        for tag in tags {
            digest = seed_index(&data, tag);
        }
        (data, digest)
    }

    /// A committed root that records `tag` at `digest` — what the registry
    /// currently serves, so re-observing it moves no byte and the run is a C6
    /// no-op.
    fn unchanged_root(tag: &str, digest: &str) -> Value {
        committed_root(serde_json::json!({
            tag: { "content": digest, "observed": "2026-07-01T00:00:00Z" }
        }))
    }

    /// Drive [`announce`] against `forge` and a seeded registry.
    async fn run_announce(
        forge: &FakeForge,
        curated: TagSelection,
        target: AnnounceTarget,
        registry: StubTransportData,
    ) -> Result<AnnounceOutcome, AnnounceError> {
        let publisher = Publisher::new(ocx_oci::Client::with_transport(Box::new(StubTransport::new(registry))));
        announce(&publisher, Some(forge as &dyn Forge), request_to(curated, target)).await
    }

    /// The whole declared capability set, in declaration order, with
    /// `push-access` reading `expected` on this path.
    ///
    /// Asserting the names rather than merely `!is_empty()` is deliberate: a
    /// truncated or reordered array is still non-empty, and the order is what a
    /// report consumer indexes into.
    fn assert_capability_rows(outcome: &AnnounceOutcome, expected: CheckStatus, path: &str) {
        let names: Vec<CapabilityName> = outcome
            .capability_checks
            .checks()
            .iter()
            .map(|check| check.name)
            .collect();
        assert_eq!(
            names,
            CapabilityName::ALL.to_vec(),
            "{path}: the rows are the declared set, in declaration order"
        );
        assert_eq!(
            outcome.capability_checks.status(CapabilityName::PushAccess),
            expected,
            "{path}: the push-access row"
        );
    }

    /// A base root whose one committed tag records a digest the registry no
    /// longer serves, so re-observing it moves the bytes and the run is not a
    /// C6 no-op.
    fn root_with_a_moved_tag() -> Value {
        committed_root(serde_json::json!({
            "0.9.0": { "content": format!("sha256:{}", "b".repeat(64)), "observed": "2026-07-01T00:00:00Z" }
        }))
    }

    fn pull_request(number: u64) -> PullRequest {
        PullRequest {
            number,
            html_url: format!("https://example.invalid/pull/{number}"),
            updated: true,
        }
    }

    fn fork_identity() -> ForkIdentity {
        ForkIdentity {
            full_path: "forkuser/index".to_string(),
            namespace: "forkuser".to_string(),
            project: "index".to_string(),
            id: None,
        }
    }

    fn fork_target() -> AnnounceTarget {
        AnnounceTarget::Fork(RepoCoordinate {
            host: None,
            namespace: "forkuser".to_string(),
            project: "index".to_string(),
        })
    }

    // ── C-055: the two new outcome fields ────────────────────────────────────

    /// Every outcome carries the announce branch and the whole capability array,
    /// on all five construction sites rather than only the committing one.
    ///
    /// Four of the five return before `ensure_push_access` is reached — `--out`,
    /// both unchanged arms, and the whole fork path — so a test driving the
    /// committing path alone stays green while the other four ship an empty
    /// array and an empty branch. `--out` is the one path that reports **no**
    /// branch: it reads none and pushes nothing, and the name is derivable on
    /// every run, so reporting it there would claim a branch was written.
    #[tokio::test(flavor = "multi_thread")]
    async fn announce_outcome_carries_branch_and_capability_checks() {
        // `--out`: writes locally, touches no branch.
        let directory = tempfile::TempDir::new().expect("a scratch directory");
        let (registry, digest) = seed_tags(&["1.0.0"]);
        let forge = FakeForge::new()
            .with_ref(MAIN_REF, &[Some("base")])
            .with_root("base", &unchanged_root("1.0.0", &digest));
        let outcome = run_announce(
            &forge,
            TagSelection::Refresh,
            AnnounceTarget::Out(directory.path().to_path_buf()),
            registry,
        )
        .await
        .expect("--out writes on every run");
        assert!(
            outcome.branch.is_empty(),
            "--out reads no branch and pushes nothing, so it reports none: {:?}",
            outcome.branch
        );
        assert!(!outcome.written_paths.is_empty(), "--out writes the root");
        assert_capability_rows(&outcome, CheckStatus::Skipped, "--out");
        assert_eq!(forge.push_access_probes(), 0, "--out demands no push permission");

        // Unchanged with a stale branch: returns the request the branch already
        // has, without committing.
        let (registry, digest) = seed_tags(&["1.0.0"]);
        let forge = FakeForge::new()
            .with_ref(BRANCH_REF, &[Some("branch")])
            .with_ref(MAIN_REF, &[Some("base")])
            .with_root("base", &unchanged_root("1.0.0", &digest))
            .with_root("branch", &unchanged_root("1.0.0", &digest))
            .stale_with(pull_request(3));
        let outcome = run_announce(&forge, TagSelection::Refresh, AnnounceTarget::Direct, registry)
            .await
            .expect("an unchanged stale branch reports its open request");
        assert_eq!(outcome.status, AnnounceStatus::Unchanged);
        assert_eq!(outcome.branch, BRANCH, "the stale-branch arm names the branch");
        assert_capability_rows(&outcome, CheckStatus::Skipped, "unchanged, stale branch");

        // Unchanged with a live branch: ensures the request the branch may be
        // missing, still without committing.
        let (registry, digest) = seed_tags(&["1.0.0"]);
        let forge = FakeForge::new()
            .with_ref(BRANCH_REF, &[Some("branch")])
            .with_ref(MAIN_REF, &[Some("base")])
            .with_root("branch", &unchanged_root("1.0.0", &digest));
        let outcome = run_announce(&forge, TagSelection::Refresh, AnnounceTarget::Direct, registry)
            .await
            .expect("an unchanged live branch ensures its request");
        assert_eq!(outcome.branch, BRANCH, "the live-branch arm names the branch");
        assert_capability_rows(&outcome, CheckStatus::Skipped, "unchanged, live branch");

        // Unchanged with nothing unmerged: no request at all.
        let (registry, digest) = seed_tags(&["1.0.0"]);
        let forge = FakeForge::new()
            .with_ref(MAIN_REF, &[Some("base")])
            .with_root("base", &unchanged_root("1.0.0", &digest));
        let outcome = run_announce(&forge, TagSelection::Refresh, AnnounceTarget::Direct, registry)
            .await
            .expect("a pure no-op still reports");
        assert!(outcome.pull_request.is_none(), "a pure no-op opens nothing");
        assert_eq!(outcome.branch, BRANCH, "the no-op arm names the branch");
        assert_capability_rows(&outcome, CheckStatus::Skipped, "unchanged, nothing unmerged");

        // The fork-free commit path: the one path that probes push access, so
        // the one path whose row is not `skipped`.
        let (registry, _) = seed_tags(&["1.0.0"]);
        let forge = FakeForge::new()
            .with_ref(MAIN_REF, &[Some("base")])
            .with_root("base", &committed_root(serde_json::json!({})));
        let outcome = run_announce(
            &forge,
            TagSelection::Replace(vec!["1.0.0".to_string()]),
            AnnounceTarget::Direct,
            registry,
        )
        .await
        .expect("the direct path commits and opens");
        assert_eq!(outcome.status, AnnounceStatus::Updated);
        assert_eq!(outcome.branch, BRANCH, "the committing arm names the branch");
        assert_eq!(forge.push_access_probes(), 1, "the fork-free path probes exactly once");
        assert_capability_rows(&outcome, CheckStatus::Passed, "direct commit");

        // The fork path reaches the same construction site and deliberately does
        // not probe (C6: an unchanged run must demand no permission), so its row
        // is the seeded `skipped` rather than an absent one.
        let (registry, _) = seed_tags(&["1.0.0"]);
        let forge = FakeForge::new()
            .with_ref(MAIN_REF, &[Some("base")])
            .with_root("base", &committed_root(serde_json::json!({})))
            .with_fork(fork_identity());
        let outcome = run_announce(
            &forge,
            TagSelection::Replace(vec!["1.0.0".to_string()]),
            fork_target(),
            registry,
        )
        .await
        .expect("the fork path commits and opens");
        assert!(outcome.fork.is_some(), "the fork path reports its fork");
        assert_eq!(outcome.branch, BRANCH, "the fork arm names the branch");
        assert_eq!(forge.push_access_probes(), 0, "the fork path probes no push access");
        assert_capability_rows(&outcome, CheckStatus::Skipped, "fork commit");
    }

    // ── C-056: the retry wraps the commit-and-open pair ──────────────────────

    /// The rejection is scripted on **`open_or_update_pull_request`**, not on
    /// `commit_files`, and that is the whole point.
    ///
    /// Under the `git` transport `commit_files` performs no network write at
    /// all: the push — and therefore the rejection — happens inside
    /// `open_or_update_pull_request`. With the retry wrapped around the commit
    /// alone, that error escapes and a concurrent announce is silently lost. A
    /// test scripting the rejection on `commit_files` instead passes against the
    /// unwidened code and proves nothing.
    #[tokio::test(flavor = "multi_thread")]
    async fn non_fast_forward_retry_wraps_commit_and_open() {
        let (registry, _) = seed_tags(&["1.0.0"]);
        let forge = FakeForge::new()
            .with_ref(MAIN_REF, &[Some("base"), Some("moved")])
            .with_root("base", &committed_root(serde_json::json!({})))
            .with_root("moved", &committed_root(serde_json::json!({})))
            .failing_opens(vec![Some(ForgeError::NonFastForward {
                branch: BRANCH.to_string(),
            })]);

        let outcome = run_announce(
            &forge,
            TagSelection::Replace(vec!["1.0.0".to_string()]),
            AnnounceTarget::Direct,
            registry,
        )
        .await
        .expect("the run recovers from a rejection raised by the open call");

        assert!(outcome.pull_request.is_some(), "the retry produced a request");
        assert_eq!(
            forge.commits().len(),
            2,
            "the retry re-ran the commit against the winning head"
        );
        assert_eq!(forge.opens(), 2, "the open call was retried, not abandoned");
    }

    /// A rejection on both attempts must **converge**: exactly one retry, then
    /// the error.
    ///
    /// A fixture that rejects once cannot tell "retried once" from "retries
    /// forever", so the call count is the assertion and the error is only the
    /// corroboration. Turning the one-shot arm into a bounded loop keeps the
    /// error identical and moves the count.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_persistent_race_converges_after_exactly_one_retry() {
        let rejection = || {
            Some(ForgeError::NonFastForward {
                branch: BRANCH.to_string(),
            })
        };
        let (registry, _) = seed_tags(&["1.0.0"]);
        let forge = FakeForge::new()
            .with_ref(MAIN_REF, &[Some("base"), Some("moved")])
            .with_root("base", &committed_root(serde_json::json!({})))
            .with_root("moved", &committed_root(serde_json::json!({})))
            .failing_opens(vec![rejection(), rejection()]);

        let result = run_announce(
            &forge,
            TagSelection::Replace(vec!["1.0.0".to_string()]),
            AnnounceTarget::Direct,
            registry,
        )
        .await;

        assert!(
            matches!(result, Err(AnnounceError::Forge(ForgeError::NonFastForward { .. }))),
            "a still-rejected retry surfaces the race, not a generic failure"
        );
        assert_eq!(forge.opens(), 2, "exactly one retry, never a loop");
        assert_eq!(forge.commits().len(), 2, "exactly one regeneration pass");
    }

    /// The retry regenerates against the head that **won** the race, rather than
    /// re-proposing the pre-race bytes.
    ///
    /// A widened retry that merely re-calls the open request would republish the
    /// content the race already superseded — the loss C-056 exists to prevent,
    /// one layer down. The winner is distinguished by a non-tag field, so the
    /// difference cannot be confused with the curated set being re-resolved.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_retry_regenerates_against_the_winning_head() {
        let (registry, _) = seed_tags(&["1.0.0"]);
        let mut winner = committed_root(serde_json::json!({}));
        winner["created"] = serde_json::json!("2026-08-01");
        let forge = FakeForge::new()
            .with_ref(MAIN_REF, &[Some("base"), Some("moved")])
            .with_root("base", &committed_root(serde_json::json!({})))
            .with_root("moved", &winner)
            .failing_opens(vec![Some(ForgeError::NonFastForward {
                branch: BRANCH.to_string(),
            })]);

        run_announce(
            &forge,
            TagSelection::Replace(vec!["1.0.0".to_string()]),
            AnnounceTarget::Direct,
            registry,
        )
        .await
        .expect("the retry succeeds");

        let commits = forge.commits();
        assert_eq!(commits.len(), 2, "the retry commits again");
        assert_ne!(
            commits[0].0, commits[1].0,
            "the second commit carries different bytes from the first"
        );
        assert!(
            committed_root_bytes(&commits[1]).contains("2026-08-01"),
            "the second commit is built on the winner's root: {}",
            committed_root_bytes(&commits[1])
        );
    }

    /// A lost `--force-with-lease` drives the same retry as a lost
    /// compare-and-swap.
    ///
    /// `Spent` and `Stale` branches are repointed with [`RefUpdate::Reset`],
    /// which the git transport spells as a leased force-push; losing that race
    /// surfaces as [`ForgeError::StaleLease`], never
    /// [`ForgeError::NonFastForward`]. A predicate naming only the second would
    /// retry under `api` and give up under `git` for the identical race — and
    /// because both variants classify to exit 75, **only the call count can see
    /// the difference**.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_stale_lease_drives_the_same_retry_as_a_lost_compare_and_swap() {
        let (registry, _) = seed_tags(&["1.0.0"]);
        let forge = FakeForge::new()
            .with_ref(BRANCH_REF, &[Some("branch")])
            .with_ref(MAIN_REF, &[Some("base"), Some("moved")])
            .with_root("base", &committed_root(serde_json::json!({})))
            .with_root("moved", &committed_root(serde_json::json!({})))
            .with_root("branch", &committed_root(serde_json::json!({})))
            .stale_with(pull_request(4))
            .failing_opens(vec![Some(ForgeError::StaleLease {
                branch: BRANCH.to_string(),
            })]);

        let result = run_announce(
            &forge,
            TagSelection::Replace(vec!["1.0.0".to_string()]),
            AnnounceTarget::Direct,
            registry,
        )
        .await;

        // The count is asserted **before** the result, deliberately: narrowing
        // the predicate back to `NonFastForward` alone leaves one commit and one
        // exit-75 error, and an assertion that reads the outcome first would
        // report the error rather than the missing retry.
        assert_eq!(
            forge.commits().len(),
            2,
            "a lost lease regenerates against the winning head, exactly as a lost compare-and-swap does"
        );
        let outcome = result.expect("a lost lease is a race, and a race is retried");
        assert!(outcome.pull_request.is_some(), "the retry produced a request");
    }

    /// The retry's fallback has nowhere to fall back to, and says so.
    ///
    /// An accumulating retry re-reads the branch; a branch that is genuinely
    /// absent leaves the index base as the only head there is. That arm is
    /// already ridden by every `Absent` retry row in this module — `FakeForge`
    /// arms no `BRANCH_REF` unless a row asks for one, so each of them reaches
    /// the fallback and would red with `MissingBaseRef` if it were deleted.
    /// What none of them reaches is the fallback's own refusal, because they
    /// all arm a base that repeats.
    ///
    /// A base that answers once and then vanishes is the one shape that gets
    /// there: an index whose main was deleted under a racing announce. The
    /// refusal has to be that error and not a panic or a silent absent-root,
    /// because the retry would otherwise commit against no base at all.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_retry_whose_base_ref_vanished_names_the_missing_ref() {
        let (registry, _) = seed_tags(&["1.0.0"]);
        let forge = FakeForge::new()
            .with_ref(MAIN_REF, &[Some("base"), None])
            .with_root("base", &committed_root(serde_json::json!({})))
            .failing_opens(vec![Some(ForgeError::NonFastForward {
                branch: BRANCH.to_string(),
            })]);

        let result = run_announce(
            &forge,
            TagSelection::Replace(vec!["1.0.0".to_string()]),
            AnnounceTarget::Direct,
            registry,
        )
        .await;

        assert!(
            matches!(result, Err(AnnounceError::MissingBaseRef { .. })),
            "a vanished base is named, never guessed at: {result:?}"
        );
        assert_eq!(
            forge.commits().len(),
            1,
            "and nothing is committed against a base the run could not read"
        );
    }

    /// A failure that is **not** a race must not retry.
    ///
    /// [`ForgeError::MergeRequestUnconfirmed`] means the push already succeeded
    /// and the server was slower than the poll, so retrying pushes a second
    /// time. Widening the arm to `Err(_)` once two calls feed one `match` is the
    /// natural mistake, and the exit code cannot catch it: 75 is shared with
    /// both race variants.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_non_race_failure_never_retries() {
        let (registry, _) = seed_tags(&["1.0.0"]);
        let forge = FakeForge::new()
            .with_ref(MAIN_REF, &[Some("base"), Some("moved")])
            .with_root("base", &committed_root(serde_json::json!({})))
            .with_root("moved", &committed_root(serde_json::json!({})))
            .failing_opens(vec![Some(ForgeError::MergeRequestUnconfirmed { deadline_secs: 30 })]);

        let result = run_announce(
            &forge,
            TagSelection::Replace(vec!["1.0.0".to_string()]),
            AnnounceTarget::Direct,
            registry,
        )
        .await;

        assert!(
            matches!(
                result,
                Err(AnnounceError::Forge(ForgeError::MergeRequestUnconfirmed { .. }))
            ),
            "an unconfirmed merge request surfaces as itself"
        );
        assert_eq!(forge.commits().len(), 1, "a settled push is never pushed again");
        assert_eq!(forge.opens(), 1, "and the request is never re-opened");
    }

    /// When the regenerated bytes equal the winning head, the commit is skipped
    /// — and the request must still be opened.
    ///
    /// Two identical racing announces both regenerate the same bytes, so a
    /// second commit would push an empty diff (X7). That arm already has an
    /// early-exit shape, which makes it the one place the widening can silently
    /// drop the open call and strand the winner's commit with no request.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_byte_identical_retry_still_opens_the_pull_request() {
        let (registry, digest) = seed_tags(&["0.9.0"]);
        let forge = FakeForge::new()
            .with_ref(MAIN_REF, &[Some("base"), Some("moved")])
            .with_root("base", &root_with_a_moved_tag())
            .with_root("moved", &unchanged_root("0.9.0", &digest))
            .failing_opens(vec![Some(ForgeError::NonFastForward {
                branch: BRANCH.to_string(),
            })]);

        let outcome = run_announce(&forge, TagSelection::Refresh, AnnounceTarget::Direct, registry)
            .await
            .expect("the byte-identical retry still reports a request");

        assert_eq!(
            outcome.status,
            AnnounceStatus::Unchanged,
            "re-applying C6 against the winner reports unchanged"
        );
        assert_eq!(forge.commits().len(), 1, "no empty-diff commit is pushed");
        assert!(
            outcome.pull_request.is_some(),
            "the winning commit is never stranded without a request"
        );
        assert_eq!(forge.opens(), 2, "the open call still runs after the skipped commit");
    }

    /// The D2 tripwire survives the widening: an unmergeable open request is
    /// refused from the **retry**'s byte-identical arm too, not only from the
    /// first pass.
    ///
    /// That arm commits nothing, so a conflicting request stays conflicting and
    /// would otherwise be reported as a benign `unchanged` — ocx-sh/ocx#399 in
    /// the retry path.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_conflicting_request_is_refused_from_the_retry_arm() {
        let (registry, digest) = seed_tags(&["0.9.0"]);
        let forge = FakeForge::new()
            .with_ref(BRANCH_REF, &[Some("branch")])
            .with_ref(MAIN_REF, &[Some("base"), Some("moved")])
            .with_root("base", &root_with_a_moved_tag())
            .with_root("moved", &unchanged_root("0.9.0", &digest))
            .with_root("branch", &committed_root(serde_json::json!({})))
            .stale_with(pull_request(5))
            .with_mergeability(Mergeability::Conflicting)
            .failing_opens(vec![Some(ForgeError::NonFastForward {
                branch: BRANCH.to_string(),
            })]);

        let result = run_announce(&forge, TagSelection::Refresh, AnnounceTarget::Direct, registry).await;

        assert!(
            matches!(result, Err(AnnounceError::PullRequestUnmergeable { .. })),
            "a conflicting request on a write-nothing retry is refused, never reported as unchanged"
        );
        assert_eq!(forge.mergeability_reads(), 1, "the tripwire ran on the retry path");
        assert_eq!(forge.commits().len(), 1, "the retry committed nothing");
    }

    /// S-025: the retry carries the stale branch's tag delta forward and keeps
    /// repointing the ref with [`RefUpdate::Reset`].
    ///
    /// The retry is the arm the widening rewrites, and its carry is the one a
    /// restructure loses: without it the re-read base has no tags at all, the
    /// regenerated root equals the winning head, and the second commit is
    /// skipped — so the tag already announced into the open request disappears.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_retry_carries_the_stale_branch_tags_and_resets_the_ref() {
        let (registry, _) = seed_tags(&["0.9.0"]);
        let forge = FakeForge::new()
            .with_ref(BRANCH_REF, &[Some("branch")])
            .with_ref(MAIN_REF, &[Some("base"), Some("moved")])
            .with_root("base", &committed_root(serde_json::json!({})))
            .with_root("moved", &committed_root(serde_json::json!({})))
            .with_root("branch", &root_with_a_moved_tag())
            .stale_with(pull_request(6))
            .failing_commits(vec![Some(ForgeError::NonFastForward {
                branch: BRANCH.to_string(),
            })]);

        run_announce(&forge, TagSelection::Refresh, AnnounceTarget::Direct, registry)
            .await
            .expect("the rebuild retries and lands");

        let commits = forge.commits();
        assert_eq!(commits.len(), 2, "the retry committed the carried delta");
        assert!(
            committed_root_bytes(&commits[1]).contains("0.9.0"),
            "the branch's tag survives the retry: {}",
            committed_root_bytes(&commits[1])
        );
        assert_eq!(
            commits[1].1,
            RefUpdate::Reset,
            "a rebuilt branch is still repointed, not fast-forwarded"
        );
    }

    /// S-024's half that announce controls: an unchanged run on a live branch
    /// makes the open call and **no commit at all**.
    ///
    /// Whether that call then writes anything is the transport's contract
    /// (C-042) and is proved at the acceptance layer; what the orchestration
    /// owes is that it never commits on this path.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_unchanged_live_branch_opens_the_request_and_commits_nothing() {
        let (registry, digest) = seed_tags(&["1.0.0"]);
        let forge = FakeForge::new()
            .with_ref(BRANCH_REF, &[Some("branch")])
            .with_ref(MAIN_REF, &[Some("base")])
            .with_root("branch", &unchanged_root("1.0.0", &digest));

        let outcome = run_announce(&forge, TagSelection::Refresh, AnnounceTarget::Direct, registry)
            .await
            .expect("an unchanged live branch ensures its request");

        assert_eq!(outcome.status, AnnounceStatus::Unchanged);
        assert!(
            outcome.pull_request.is_some(),
            "the request the branch may lack is ensured"
        );
        assert_eq!(forge.opens(), 1, "exactly one open call");
        assert!(forge.commits().is_empty(), "an unchanged run commits nothing");
    }

    // ── S-005: the unclaimed-namespace signal ────────────────────────────────

    /// An absent committed root is an unclaimed package, raised from the
    /// orchestration's own read rather than only from the classifier's table.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_absent_committed_root_is_an_unclaimed_namespace() {
        let (registry, _) = seed_tags(&["1.0.0"]);
        let forge = FakeForge::new().with_ref(MAIN_REF, &[Some("base")]);

        let result = run_announce(&forge, TagSelection::Refresh, AnnounceTarget::Direct, registry).await;

        assert!(
            matches!(result, Err(AnnounceError::UnclaimedPackage { .. })),
            "a package with no committed root goes through the human lane"
        );
    }

    // ── #477: the committed root's `name` is checked ─────────────────────────

    /// The three shapes a root's `name` can have against the identifier the run
    /// announces: matching (the run proceeds), differing, and absent.
    ///
    /// The differing and absent halves are refused at `DataError`, and the
    /// refusal costs **zero** registry requests — the check sits immediately
    /// after the root-shape check, before the SSRF pre-flight and before the
    /// first observe, so announcing into somebody else's entry cannot
    /// half-happen. The matching half is not decoration: without it a check
    /// that refused every root would pass both refusal assertions.
    ///
    /// Reds on deleting the check (differing and absent both become `Ok`) and
    /// on reading `name` with a `.unwrap_or(expected)`-shaped default (the
    /// absent half becomes `Ok`).
    #[tokio::test(flavor = "multi_thread")]
    async fn a_committed_root_naming_another_package_is_refused_before_any_registry_call() {
        // Matching: the fixture root names `ocx.sh/acme/widget` and the request
        // announces exactly that, so the run reaches the registry and succeeds.
        let (registry, digest) = seed_tags(&["1.0.0"]);
        let forge = FakeForge::new()
            .with_ref(MAIN_REF, &[Some("base")])
            .with_root("base", &unchanged_root("1.0.0", &digest));
        let outcome = run_announce(&forge, TagSelection::Refresh, AnnounceTarget::Direct, registry)
            .await
            .expect("a root whose name agrees announces");
        assert_eq!(outcome.status, AnnounceStatus::Unchanged);

        // Differing: the same root under another registry's name.
        let (registry, digest) = seed_tags(&["1.0.0"]);
        let mut other = unchanged_root("1.0.0", &digest);
        other["name"] = Value::from("other.example/acme/widget");
        let forge = FakeForge::new()
            .with_ref(MAIN_REF, &[Some("base")])
            .with_root("base", &other);
        let result = run_announce(&forge, TagSelection::Refresh, AnnounceTarget::Direct, registry.clone()).await;
        let Err(AnnounceError::RootNameMismatch {
            committed, expected, ..
        }) = result
        else {
            panic!("a root naming another package must be refused, got {result:?}");
        };
        assert_eq!(committed, "other.example/acme/widget");
        assert_eq!(expected, "ocx.sh/acme/widget", "the one spelling is claim::root_name");
        assert!(
            registry.read().calls.is_empty(),
            "the refusal precedes every registry request: {:?}",
            registry.read().calls
        );

        // Absent: fail closed, with an empty `committed` the message renders
        // as its own sentence rather than as `names , not …`.
        let (registry, digest) = seed_tags(&["1.0.0"]);
        let mut nameless = unchanged_root("1.0.0", &digest);
        nameless
            .as_object_mut()
            .expect("the fixture root is an object")
            .remove("name");
        let forge = FakeForge::new()
            .with_ref(MAIN_REF, &[Some("base")])
            .with_root("base", &nameless);
        let result = run_announce(&forge, TagSelection::Refresh, AnnounceTarget::Direct, registry).await;
        let Err(error @ AnnounceError::RootNameMismatch { .. }) = result else {
            panic!("a root with no name at all must be refused, got {result:?}");
        };
        let message = error.to_string();
        assert!(
            message.contains("carries no name") && message.contains("ocx.sh/acme/widget"),
            "the message must say what is missing and what was expected: {message}"
        );
    }

    // ── #487: a description-only refresh ─────────────────────────────────────

    /// A claimed but unreleased package carries `"tags": {}`, and `--refresh`
    /// over it used to collapse into `NoCuratedTags` (exit 64) before the
    /// description was ever observed.
    ///
    /// The run now completes with an empty curated set and reports `unchanged`
    /// — `AnnounceStatus` stays two-valued, and `desc_status` is the field that
    /// discriminates a description-only pass.
    ///
    /// Reds on restoring the unconditional empty-set refusal in
    /// `pipeline::resolve_curated_tags`.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_refresh_over_a_claimed_but_unreleased_root_is_a_description_pass() {
        let (registry, _) = seed_tags(&[]);
        let forge = FakeForge::new()
            .with_ref(MAIN_REF, &[Some("base")])
            .with_root("base", &committed_root(serde_json::json!({})));

        let outcome = run_announce(&forge, TagSelection::Refresh, AnnounceTarget::Direct, registry)
            .await
            .expect("an empty committed tag set is nothing to curate, not a usage error");

        assert_eq!(
            outcome.status,
            AnnounceStatus::Unchanged,
            "no tag and no description moved"
        );
        assert_eq!(outcome.desc_status, AnnounceStatus::Unchanged);
        assert!(forge.commits().is_empty(), "an unchanged run commits nothing");
    }

    /// The same run once the package publishes a description: the empty tag set
    /// still carries no version, the `__ocx.desc` observation moves, and the two
    /// existing status words say so — `updated` overall, `updated` on the
    /// description. No third outcome word was needed.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_description_only_refresh_reports_updated_through_desc_status() {
        let directory = tempfile::TempDir::new().expect("a scratch directory");
        let data = StubTransportData::new();
        let readme_hex = seed_description(&data);
        let forge = FakeForge::new()
            .with_ref(MAIN_REF, &[Some("base")])
            .with_root("base", &committed_root(serde_json::json!({})));

        let outcome = run_announce(
            &forge,
            TagSelection::Refresh,
            AnnounceTarget::Out(directory.path().to_path_buf()),
            data,
        )
        .await
        .expect("a description-only refresh succeeds");

        assert_eq!(outcome.status, AnnounceStatus::Updated);
        assert_eq!(outcome.desc_status, AnnounceStatus::Updated);
        assert!(
            outcome
                .written_paths
                .iter()
                .any(|path| path.ends_with(&format!("{readme_hex}.md"))),
            "the readme blob rides out with the root: {:?}",
            outcome.written_paths
        );
    }
}
