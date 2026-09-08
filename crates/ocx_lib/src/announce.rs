// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx package announce` orchestration (ocx-sh/ocx#216).
//!
//! A self-contained, forge-neutral routine (reused by `ocx-mirror`) that a
//! publisher runs to update **one** package's entry in the `ocx-sh/index`
//! repository. It observes an owner-curated set of registry tags, rebuilds the
//! package's index root byte-exactly (CONTRACTS §14, via
//! [`crate::oci::index::serialize_root`]) and stores each observed tag's image
//! index verbatim as a content-addressed object, then writes the result locally
//! (`--out`), opens/updates a pull request from a fork of the index (`--fork`),
//! or opens the same pull request from a branch on the index repository itself
//! (neither flag — for a publisher whose credential can already push there).
//! Server-side privileged verification (ownership, claim re-derivation) happens
//! in the index CI, never here.
//!
//! Every remote mode ends in a **pull request** — the fork-free path narrows
//! design register S3 ("always fork") to "always a reviewed pull request",
//! because GitHub cannot fork a repository into the organization that owns it,
//! so first-party publishers had no working path at all.
//!
//! The pipeline behaviourally matches the Python reference tool
//! (`ocx-sh/index` `bot/cli/announce.py`, design register FP-9), with the
//! owner-ratified C-cell decisions layered on: branch-head base (C4),
//! replace/union/refresh/from-registry curation (C3/C5), the unchanged
//! short-circuit (C6),
//! owner-only yank markers (C7), fork auto-create at the upstream base SHA (C8),
//! and one atomic multi-file commit (C15). The SSRF guard runs before the first
//! registry request (X3); the announce credential never leaves the ambient
//! `OCX_ANNOUNCE_TOKEN` (X6), carried only by the passed-in
//! [`Forge`](crate::forge::Forge) implementation.
//!
//! ADR: `adr_announce_publisher_surface.md` (D5 — orchestration in `ocx_lib`,
//! the CLI a thin wrapper).

pub mod error;
mod pipeline;
pub mod request;

pub use error::AnnounceError;
pub use request::{AnnounceOutcome, AnnounceRequest, AnnounceStatus, AnnounceTarget, TagSelection};

use std::collections::BTreeMap;

use serde_json::Value;

use crate::forge::{
    BranchComparison, CommitBase, Forge, Mergeability, PullRequest, PushAccess, RefUpdate, RepoCoordinate,
};
use crate::oci::index::serialize_root;
use crate::publisher::Publisher;

/// The main branch of the index repository (design register C10).
const INDEX_BASE_REF: &str = "main";

/// Announce one package (design register C11/C12 — one package per call).
///
/// `forge` is `Some` for every mode that reaches a remote: it reads the
/// committed root (C10) and, for [`AnnounceTarget::Fork`] and
/// [`AnnounceTarget::Direct`], commits and opens the pull request. A `None`
/// forge cannot read the committed root, so announce returns
/// [`AnnounceError::ForgeRequired`].
///
/// # Errors
///
/// Returns an [`AnnounceError`] for a missing forge, an unclaimed package, an
/// SSRF-forbidden physical host, a curated tag that does not resolve, a
/// yank/unyank input error, or any forge / filesystem failure. See
/// [`AnnounceError`] for the full taxonomy.
pub async fn announce(
    publisher: &Publisher,
    forge: Option<&dyn Forge>,
    request: AnnounceRequest,
) -> Result<AnnounceOutcome, AnnounceError> {
    let forge = forge.ok_or(AnnounceError::ForgeRequired)?;
    let package = request.package.repository().to_string();
    let root_path = format!("p/{package}.json");
    let branch = format!("indexbot-announce-{}", package.replace('/', "-"));
    // The capability array is non-empty on **every** run, so it is seeded here
    // rather than at the one path that probes: `ensure_push_access` is reached
    // only from the fork-free commit arm, and `--out`, both unchanged arms and
    // the whole fork path would otherwise report nothing at all. Held as the
    // `PushAccess` itself — see `AnnounceOutcome::capability_checks` for why the
    // rows are never copied out into a bare vector.
    let mut capability_checks = PushAccess::skipped_all();

    // 0. Resolve the fork's REAL identity first, so every later endpoint is
    //    built from it — the announce branch lives on the fork, and `--fork
    //    <owner>/<repo>` may name one renamed away from the upstream's
    //    repository name. Deliberately read-only (`find_fork`, never
    //    `ensure_fork`): an unchanged run must not provoke a fork create (C6).
    let fork_target = match &request.target {
        AnnounceTarget::Fork(target) => Some(target),
        AnnounceTarget::Direct | AnnounceTarget::Out(_) => None,
    };
    let existing_fork = match fork_target {
        Some(target) => forge.find_fork(&request.index_repo, target).await?,
        None => None,
    };
    // Where the announce branch lives *right now*: a fork that already exists,
    // the index repository itself on the fork-free path, nowhere for `--out`.
    // Every later endpoint is built from this one value, so the fork and direct
    // paths share the whole branch-state / root-read / commit sequence below.
    let branch_repo = match &request.target {
        AnnounceTarget::Direct => Some(request.index_repo.clone()),
        AnnounceTarget::Fork(_) => existing_fork.as_ref().map(|fork| fork.coordinate(&request.index_repo)),
        AnnounceTarget::Out(_) => None,
    };

    // 0b. Is the announce branch still carrying work, or is it residue?
    //     The branch is per package (C4) and outlives every pull request opened
    //     from it, so its mere existence says nothing. Accumulating onto a spent
    //     branch re-proposes already-merged commits and the pull request
    //     conflicts on the root file every announce edits (#228).
    let branch_state = resolve_branch_state(forge, &request.index_repo, branch_repo.as_ref(), &branch).await?;

    // 1. Read the committed root (C10), from the announce branch head while the
    //    branch is live (C4) so sequential announces accumulate, else from the
    //    index main — carrying a stale branch's tag delta forward (D1).
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
    // D1: a stale branch supplies its tags but never its shape — that shape is
    // what froze in #399. `committed_bytes` deliberately stays the BASE's raw
    // bytes while the regeneration input becomes the merged root: comparing C6
    // against the merged root would read `unchanged` for exactly the runs this
    // rebuild exists to unfreeze.
    if let Some(carried) = &root_read.carried_tags {
        pipeline::carry_branch_tags(&mut committed_root, carried);
    }

    // 2/3/4. Resolve the curated tag set (C3/C5), observe it (X3), regenerate
    //        the tags (C6-preserving), apply yank markers (C7), and assemble
    //        the atomic file set (C15) — one timestamp shared by all of them so
    //        the run is self-consistent.
    let now = pipeline::current_timestamp();
    let Rebuilt {
        root_bytes: new_root_bytes,
        files,
        observed,
        mut reserved_dropped,
        desc_updated,
    } = observe_and_rebuild(publisher, &committed_root, &request, &now, &root_path, &package).await?;
    let mut desc_status = AnnounceStatus::from_changed(desc_updated);

    // C6: byte-identical root AND no new CAS object ⇒ nothing moved.
    let unchanged = new_root_bytes == committed_bytes && pipeline::new_cas_count(&committed_root, &observed) == 0;
    let mut status = if unchanged {
        AnnounceStatus::Unchanged
    } else {
        AnnounceStatus::Updated
    };
    let message = format!("announce: curate {package}");
    let pull_request_body = format!("Publisher-curated tag update for `{package}`.");

    // 7. Dispatch: write locally (`--out`), or commit and open/update the pull
    //    request — from a fork (`--fork`) or from the index repository itself.
    match &request.target {
        AnnounceTarget::Out(directory) => {
            // `--out` writes on EVERY run, unchanged included. C6 is scoped to
            // "no commit, no pull request" (design register), and a local write
            // is neither: a pipeline shaped `announce --out dir && publish dir`
            // must not silently publish an empty directory just because nothing
            // moved. The byte contract makes the repeated write idempotent, and
            // `status` still reports `unchanged` so callers can tell.
            let written_paths = pipeline::write_out(directory, &files).await?;
            Ok(AnnounceOutcome {
                package,
                status,
                pull_request: None,
                fork: None,
                written_paths,
                reserved_tags_dropped: reserved_dropped,
                desc_status,
                // No branch is read and none is pushed, so none is reported —
                // the name is derivable on every run, and naming it here would
                // tell a consumer a branch was written.
                branch: String::new(),
                capability_checks,
            })
        }
        AnnounceTarget::Fork(_) | AnnounceTarget::Direct => {
            if unchanged {
                // C6: a pure no-op — unless a prior announce left commits
                // on the branch that the index base does not have. Such a run
                // may have committed content whose pull request then failed to
                // open, stranding the update; ensure an open PR exists so it is
                // never lost (design register C6 amendment).
                //
                // Exactly two states carry something unmerged, and they need
                // opposite handling. `Live` (`branch_sha` is `Some`) accumulates
                // and may still be missing its pull request. `Stale` already HAS
                // one — the variant holds it — so the only open question there
                // is whether that request can still merge (D2). Every other
                // state carries nothing unmerged, and opening a pull request for
                // it produces the unmergeable one #228 is about.
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
            // C8: reuse the already-resolved fork, else create one under the
            // requested owner (S12 shared fork), then commit onto the
            // accumulating branch head (C4, live branch) or the upstream base
            // SHA (C8 — first announce, and equally a spent branch, whose head
            // `read_committed_root` was told to ignore) — one atomic multi-file
            // commit (C15).
            //
            // Both resolutions sit AFTER the unchanged short-circuit on purpose:
            // C6 says a run that moves nothing must provoke no fork create, and
            // symmetrically must not demand push permission the direct path only
            // needs in order to write.
            let (commit_repo, fork) = match fork_target {
                Some(target) => {
                    let fork = match existing_fork {
                        Some(fork) => fork,
                        None => forge.ensure_fork(&request.index_repo, Some(&target.namespace)).await?,
                    };
                    let coordinate = fork.coordinate(&request.index_repo);
                    // The commit below parents off a SHA read from the upstream
                    // repository but is written to the fork, so the base object
                    // reaches it only through the shared fork network. Land that
                    // object in the fork's own history first (see `sync_fork`).
                    forge.sync_fork(&coordinate, INDEX_BASE_REF).await;
                    (coordinate, Some(fork))
                }
                None => {
                    capability_checks = forge.ensure_push_access(&request.index_repo).await?;
                    (request.index_repo.clone(), None)
                }
            };
            // C4/C8 and the stale-fork guard in one place: an accumulating run
            // bases on the branch head in the repository the branch lives in; a
            // fresh or rebuilt branch bases on the **upstream** index's default
            // branch, never on the fork's own copy of it, which is routinely far
            // behind and would re-propose content the index already has.
            // The SHA is the one the root was READ at, never a fresh resolution
            // of the same ref name — see `RootRead::base_sha`.
            let (base_repo, base_branch) = match root_read.branch_sha {
                Some(_) => (commit_repo.clone(), branch.as_str()),
                None => (request.index_repo.clone(), INDEX_BASE_REF),
            };
            let base_sha = root_read.base_sha;
            // F2/C4: the ref update is the CAS an accumulating branch needs —
            // a rebuilt one (spent or stale) repoints with `Reset` on purpose,
            // see `BranchState::ref_update`. Either way, if the base moved
            // between our read and our commit, re-read the new head, re-run the
            // WHOLE regeneration against it, and retry exactly once so the
            // concurrent change is preserved, never overwritten.
            //
            // The commit and the pull request are **one unit of work** for the
            // purposes of that race, because which of the two loses it depends
            // on the transport. Under `api` the commit's compare-and-swap is
            // rejected. Under `git`, `commit_files` performs no network write at
            // all — objects and a local ref only — and the push happens inside
            // `open_or_update_pull_request`, so the rejection arrives there.
            // Retrying around the commit alone therefore lets a concurrent
            // announce be lost under exactly the transport that needs it most.
            // The one line this path emits, and it exists because #436 could
            // not be explained from a run's own output: two announces both
            // reported `updated` and exit 0 while one deleted the other's tag,
            // and reconstructing which commit each had been built on took a
            // commit-diff forensics session. These four fields name that
            // directly. INFO because it is the default console level, so the
            // next report of this class arrives with the answer attached.
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
                // Re-announce reuses the open request without patching
                // title/body — the C4 branch-head commits carry the update.
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
                // The two spellings of "the base moved under us", one per
                // transport. `api` refuses the fast-forward. `git` pushes a
                // `RefUpdate::Reset` rebuild with `--force-with-lease` (C-040)
                // and loses the lease, which is `StaleLease` and never
                // `NonFastForward`. Matching only the second would retry under
                // `api` and give up under `git` for the identical race — and
                // since both classify to exit 75, no assertion on the outcome
                // could tell the two behaviours apart.
                Err(crate::forge::ForgeError::NonFastForward { .. } | crate::forge::ForgeError::StaleLease { .. }) => {
                    // WHOSE head to re-read is the same question the first
                    // attempt already answered — and the ref update it chose is
                    // what recorded the answer. An accumulating run raced
                    // another announce on the branch, so the branch head is the
                    // winner. A rebuilt run (spent or stale) never based on the
                    // branch at all — it lost to the index main moving under it,
                    // and re-reading the branch head here would turn the rebuild
                    // straight back into accumulate-on-stale, which is #399 in
                    // the retry path.
                    //
                    // `root_read.branch_sha` stood in for this and is wrong on
                    // one arm: it is `None` for a rebuild AND for an `Absent`
                    // accumulate, so an `Absent` run that raced re-read the index
                    // main, regenerated the same main-derived root, and was
                    // refused again — one silent tag loss traded for a permanent
                    // exit 75 (#436). `ref_update()` separates the two, and it is
                    // what the ADR's own wording names ("whenever the first
                    // attempt used `Reset`").
                    // Announce never renders a root independently of its base,
                    // so it never asks for `Accumulate`; the arm is here because
                    // the enum is total, not because the state machine reaches it.
                    let branch_head = match branch_state.ref_update() {
                        RefUpdate::FastForward | RefUpdate::Accumulate => {
                            forge.get_ref_sha(&commit_repo, &format!("heads/{branch}")).await?
                        }
                        RefUpdate::Reset => None,
                    };
                    // No branch head on an accumulating retry means the branch
                    // genuinely is not there — the rejection came from somewhere
                    // other than a race on it (the open call, under `api`) — and
                    // the index base is the only head there is. Should the ref
                    // read be lying, as it was in #436, the commit is refused a
                    // second time and the run exits 75 rather than deriving from
                    // main unnoticed: `commit_files` backstops this arm.
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
                    // D1 again: the re-read base carries the shape, the stale
                    // branch the tags. Skipping this would drop on the retry
                    // exactly what the first attempt carried.
                    if let Some(carried) = &root_read.carried_tags {
                        pipeline::carry_branch_tags(&mut head_root, carried);
                    }
                    let merged =
                        observe_and_rebuild(publisher, &head_root, &request, &now, &root_path, &package).await?;
                    // The retry re-resolved the curated set against the winning
                    // head, so its drop list supersedes — never unions with —
                    // the pre-race one: for `--tags-file`/`--refresh` the base
                    // root differs between passes, so the two lists legitimately
                    // differ and only the second describes what was announced.
                    reserved_dropped = merged.reserved_dropped;
                    desc_status = AnnounceStatus::from_changed(merged.desc_updated);
                    // Re-apply C6 against the head that WON the race: two
                    // identical racing announces both regenerate the same
                    // bytes, so committing again would push a commit whose tree
                    // equals its base — an empty diff, a governance threat class
                    // the index bot tests for (X7). Skip the commit and fall
                    // through to the ensure-PR call below, which still runs, so
                    // nothing is stranded.
                    if merged.root_bytes == head_bytes && pipeline::new_cas_count(&head_root, &merged.observed) == 0 {
                        status = AnnounceStatus::Unchanged;
                        // The same D2 tripwire as the first pass, for the same
                        // reason: this arm commits nothing, so a conflicting
                        // request stays conflicting and would be reported as a
                        // benign `unchanged` — #399 in the retry path. Reachable
                        // when a concurrent writer wins the race on a stale
                        // branch: GitLab sends a per-file `last_commit_id` even
                        // under `Reset` and maps its rejection to
                        // `NonFastForward`.
                        //
                        // Control still falls through to the open call below, so
                        // the winning commit is never stranded without a request.
                        refuse_if_unmergeable(forge, &request.index_repo, &branch_state, &branch).await?;
                    } else {
                        // The retry commits against a different ref than the
                        // line above announced — the branch's own repository
                        // when accumulating, the index main when rebuilding —
                        // so a run that raced would otherwise leave a trace
                        // naming a base it never committed on, which is the
                        // exact reading #436 needed and did not have.
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
                            // Same ref discipline as the first attempt, for the
                            // same reasons. Accumulating: the winning head IS
                            // our base now, so the update is a fast-forward by
                            // construction — and it stays CAS-checked, since a
                            // third announce could have advanced the branch
                            // again while we regenerated. Rebuilt: the branch
                            // still holds the commits the base does not, so the
                            // repoint is still deliberately not a fast-forward.
                            .commit_files(
                                &commit_repo,
                                &branch,
                                // The base is wherever the head was re-read
                                // from: the branch's own repository when
                                // accumulating, the index main when rebuilding.
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
                    // One shot, by construction: a second rejection propagates
                    // rather than starting a third pass (S-023 — still rejected
                    // is exit 75, and the caller reruns the command).
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
                // Every other failure is settled, not raced. Widening this to
                // `Err(_)` once two calls feed one `match` is the natural
                // mistake and the costly one: `MergeRequestUnconfirmed` means
                // the push already landed, so retrying pushes a second time.
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

/// One regenerated announce payload.
struct Rebuilt {
    /// The canonical root bytes (CONTRACTS §14) — what the C6 byte comparison
    /// reads.
    root_bytes: Vec<u8>,
    /// The atomic file set a commit would carry: the root plus every CAS object
    /// (C15).
    files: BTreeMap<String, Vec<u8>>,
    /// What every curated tag was observed to hold — the C6 "no new CAS object"
    /// input.
    observed: Vec<pipeline::Observed>,
    /// Reserved tags this pass dropped from the curated set (D7).
    reserved_dropped: Vec<String>,
    /// Whether the `__ocx.desc` observation moved this pass (D6) — false when
    /// the description is unchanged or the package has none.
    desc_updated: bool,
}

/// One full regeneration pass over `base_root`: resolve the curated tag universe
/// (C3/C5) and drop its reserved tags (D7), observe every curated tag behind the SSRF pre-flight (X3), rebuild
/// the tags (C6), apply the yank markers (C7), serialize the root, and assemble
/// the atomic file set (C15).
///
/// Shared verbatim by the initial commit and the C4 non-fast-forward retry, so
/// the retry re-derives the *whole* sequence from the winning head instead of
/// replaying a tag universe resolved against the pre-race root. Re-resolving is
/// what preserves a concurrent announce's additions: `--tags-file` and
/// `--refresh` take the base root's tags as their starting set, so unioning
/// against the new head keeps a tag the winner added — replaying a stale set
/// would delete it, since [`pipeline::regenerate`] replaces `tags` wholesale.
/// `--tags` stays a deliberate replace (C3): its universe is the flag, not the
/// base root, so a tag it omits is still dropped on the retry — the publisher
/// asked for exactly that set.
///
/// # Errors
///
/// Propagates the curated-resolution (C3/C5), observe/SSRF (X3) and yank/unyank
/// (C7) failures of the steps it composes.
async fn observe_and_rebuild(
    publisher: &Publisher,
    base_root: &Value,
    request: &AnnounceRequest,
    now: &str,
    root_path: &str,
    package: &str,
) -> Result<Rebuilt, AnnounceError> {
    let base_tags = pipeline::committed_tag_names(base_root);
    // X3: the physical target is remote-controlled data (a root `repository`
    // pointer), so it is resolved and validated once, ahead of the first
    // registry request of any kind. Under `--tags-from-registry` that first
    // request is the tag listing rather than an observe — a pre-flight inside
    // the observe loop would guard the wrong thing.
    let physical = pipeline::guarded_physical(
        base_root,
        &request.trusted_hosts,
        &request.insecure_hosts,
        &crate::oci::ssrf::proxy_rules(),
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
    // D6: the description is a floating tag of its own, observed after the
    // curated set (reference parity) and behind the same pre-flight — the root's
    // `desc` object is rewritten only when its tag digest moved, so `--refresh`
    // on a package whose description never changes stays byte-identical. Its CAS
    // blobs ride along on every run regardless, exactly as the curated tags' do.
    let desc = pipeline::observe_desc(publisher, &physical, base_root).await?;
    let mut root = pipeline::regenerate(base_root, &observed, now);
    // #436's second guard, and it is load-bearing only because of the first:
    // `GitWorkspace::commit_files` now refuses a fast-forward onto a head this
    // run did not read, so `base_root` IS the tree the commit lands on. A tag it
    // carries that the regenerated set does not is therefore a deletion from the
    // index, not the artefact of having read one commit and committed onto
    // another — which is exactly what made #436 invisible.
    //
    // Scoped to the additive selections: `Replace` names its own universe (C3)
    // and `reserved_dropped` is D7's deliberate removal, so both are excluded
    // rather than refused. `regenerate` is the only step that rewrites `tags`;
    // `apply_yank_markers` below marks entries and never removes one.
    // Spelled as the three positives rather than `!Replace`: a negation opts a
    // total match out of the exhaustiveness the compiler would otherwise give
    // it, so a fifth selection would silently join the refusing set.
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
        // Replacing an existing key keeps its position (preserve_order), the
        // same mechanism `regenerate` relies on for `tags`. Every root carries
        // `desc` — the index schema requires the key, `null` when unset.
        object.insert("desc".to_string(), updated.clone());
    }
    pipeline::apply_yank_markers(&mut root, &request.yank, &request.unyank, &request.yank_reason, now)?;
    let root_bytes = serialize_root(&root);
    let files = pipeline::build_files(root_path, &root_bytes, package, &observed, &desc.blobs);
    Ok(Rebuilt {
        root_bytes,
        files,
        observed,
        reserved_dropped,
        desc_updated: desc.desc.is_some(),
    })
}

/// D2's tripwire: refuse an outcome that moves nothing while its open pull
/// request cannot merge.
///
/// A no-op for every state but [`BranchState::Stale`]. That is not an
/// optimisation but the whole design: every other outcome either commits — and
/// a commit repoints the branch with [`RefUpdate::Reset`], making the request
/// mergeable by construction — or has no open request to be stuck. So the
/// mergeability read costs a round trip only where announce has already decided
/// to write nothing, never on the hot path.
///
/// [`Mergeability::Unknown`] is deliberately benign: the forge may still be
/// computing a verdict, and the next run re-asks. No polling.
///
/// # Errors
///
/// [`AnnounceError::PullRequestUnmergeable`] when the forge reports a conflict —
/// the one corner announce cannot clear itself — or any forge failure.
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

/// The outcome of reading the committed root: its bytes (`None` when the path is
/// absent), the fork branch head SHA when the announce branch already exists
/// (C4, reused as the commit base), and the ref it was read from.
struct RootRead {
    bytes: Option<Vec<u8>>,
    branch_sha: Option<String>,
    /// The commit the bytes were actually read at, and therefore the only sound
    /// commit base. Reading the content through a *ref name* and resolving that
    /// ref to a SHA in a second call leaves a window: the ref can advance in
    /// between, and the commit would then be based on a head whose version of
    /// the root it never saw — on GitLab that even passes the `last_commit_id`
    /// check, because the check is against the newer commit. Resolving first and
    /// reading at the resolved SHA closes it.
    base_sha: String,
    base_ref: String,
    /// The stale announce branch head's own `tags` object, merged onto the
    /// base's root before regeneration (D1) so no tag already announced into
    /// the still-open pull request is lost. `None` for every state but
    /// [`BranchState::Stale`], and `None` there too when the branch head
    /// carries no root or no `tags`.
    carried_tags: Option<Value>,
}

/// What the per-package announce branch on the fork is currently worth.
///
/// The branch name is derived from the package alone, so it outlives every pull
/// request opened from it. "The ref exists" therefore answers nothing on its own,
/// and reading it that way is what made a second announce for a package
/// unmergeable once the first one's pull request had been squash-merged
/// (ocx-sh/ocx#228).
enum BranchState {
    /// No announce branch on the fork — the first announce for this package.
    Absent,
    /// The branch is still carrying work and its commits fast-forward onto the
    /// index base. Accumulate onto it (C4), so two announces before a merge land
    /// in one pull request with both tag sets.
    Live,
    /// The branch holds unmerged commits, the index base has moved on
    /// underneath them, and a pull request over the result is still open.
    ///
    /// Reading that branch head as the committed root is what froze 34 packages
    /// for up to 21 days: its root may predate a base-wide shape migration, so
    /// every later run reproduced the pre-migration bytes and reported a benign
    /// `unchanged` while the pull request stayed unmergeable (ocx-sh/ocx#399).
    /// The base supplies the shape, the branch supplies its tag delta, and the
    /// ref is repointed at the result — one commit on the current base, carrying
    /// every tag the open request already had. Holds that request so the
    /// unchanged path can reuse it without a second lookup.
    Stale(PullRequest),
    /// The branch exists and carries nothing the base needs — its pull request
    /// merged (leaving it `Diverged` under a squash merge, `Behind` under a merge
    /// commit) or was closed unmerged. Residue: rebuild from the base and repoint
    /// the ref at the result.
    Spent,
}

impl BranchState {
    /// The state's name, for the one log line the announce path emits.
    ///
    /// Not `Debug`: `Stale` carries a whole [`PullRequest`], and a log line that
    /// prints it is one nobody reads.
    fn name(&self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Live => "live",
            Self::Stale(_) => "stale",
            Self::Spent => "spent",
        }
    }

    /// Whether the branch's own head may serve as the committed root for this
    /// announce. A stale branch may not: its shape is the frozen one.
    fn is_live(&self) -> bool {
        matches!(self, Self::Live)
    }

    /// How the ref update must behave. Repointing a spent or stale branch at a
    /// commit built on the upstream base is deliberately not a fast-forward —
    /// refusing the rewrite would preserve exactly what makes the branch
    /// unusable: the already-merged commits of a spent one, the pre-migration
    /// root of a stale one.
    fn ref_update(&self) -> RefUpdate {
        match self {
            Self::Spent | Self::Stale(_) => RefUpdate::Reset,
            Self::Absent | Self::Live => RefUpdate::FastForward,
        }
    }
}

/// Classify the announce branch (see [`BranchState`]).
///
/// Ancestry is asked first and unconditionally, so an indeterminate or
/// unmodelled compare fails closed on **every** run rather than only on the runs
/// that happen to reach it. Three of the four answers are decisive on their own;
/// only [`BranchComparison::Diverged`] needs a second question, because git
/// cannot tell "my commits were squash-merged and the base now carries them"
/// from "my commits are unmerged and the base moved on underneath me". An open
/// pull request means the latter — [`BranchState::Stale`], rebuilt on the base
/// with its tags carried forward, never read as the committed root itself.
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
        // Strictly ahead: the commits are unmerged and they fast-forward, so
        // keep them whether or not a pull request is open — when none is, the
        // C6 amendment opens the one they never got.
        BranchComparison::Ahead => Ok(BranchState::Live),
        BranchComparison::Identical | BranchComparison::Behind => Ok(BranchState::Spent),
        BranchComparison::Diverged => match forge.find_open_pull_request(index_repo, fork, branch).await? {
            Some(pull_request) => Ok(BranchState::Stale(pull_request)),
            None => Ok(BranchState::Spent),
        },
    }
}

/// Read the committed root per C4/C10: the announce branch head while the
/// branch is [`BranchState::Live`] (accumulate), else the index repository's
/// `main`.
///
/// `branch_repo` is the **verified** coordinate the branch lives in — `None` for
/// `--out` and for a fork target with no fork yet, both of which can only read
/// the index base. Reading the branch through the verified coordinate rather
/// than the raw `--fork` value is what makes C4 accumulation work for a fork
/// renamed away from the upstream's repository name.
///
/// The liveness decision is the caller's, passed in as `branch_state` rather
/// than re-derived here: the base SHA this returns is the one the bytes were
/// READ at (see [`RootRead::base_sha`]), and a second, independent answer to
/// "is this branch usable" could disagree with the one the commit is built on.
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
    // The announce branch only ever exists on the fork; an absent branch reads
    // back as `None`, falling through to `main`.
    if let Some(repo) = branch_repo.filter(|_| branch_state.is_live())
        && let Some(branch_sha) = forge.get_ref_sha(repo, &format!("heads/{branch}")).await?
    {
        let bytes = forge.get_file_contents(repo, root_path, &branch_sha).await?;
        return Ok(RootRead {
            bytes,
            branch_sha: Some(branch_sha.clone()),
            base_sha: branch_sha,
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
    // D1: the base supplies the shape, a stale branch supplies the tags it
    // announced into the still-open pull request. One extra read, for `tags`
    // alone — every other key of that root may be the pre-migration form.
    let carried_tags = match (branch_repo, branch_state) {
        (Some(repo), BranchState::Stale(_)) => branch_head_tags(forge, repo, root_path, branch).await?,
        _ => None,
    };
    Ok(RootRead {
        bytes,
        branch_sha: None,
        base_sha,
        base_ref: INDEX_BASE_REF.to_string(),
        carried_tags,
    })
}

/// The `tags` object of the announce branch head's root.
///
/// `None` when the ref, the file, or the key is absent: a branch with no root to
/// read carries no delta, and failing the run instead would keep the package
/// frozen — the outcome this whole path exists to end. A root that is present
/// but unparseable is a different claim (announce wrote it, so it cannot be
/// malformed) and is refused.
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
    use crate::cli::{ExitCode, classify_error};
    use crate::forge::{CapabilityName, CheckStatus, ForgeError, ForgeIdentity, ForkIdentity};
    use crate::oci;
    use crate::oci::client::test_transport::{StubTransport, StubTransportData};

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

    fn request(curated: TagSelection) -> AnnounceRequest {
        AnnounceRequest {
            package: oci::Identifier::new_registry("acme/widget", "ocx.sh"),
            curated,
            target: AnnounceTarget::Out(std::path::PathBuf::from("unused")),
            index_repo: crate::forge::RepoCoordinate {
                host: None,
                namespace: "ocx-sh".to_string(),
                project: "index".to_string(),
            },
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
        let manifest = oci::Manifest::ImageIndex(oci::ImageIndex {
            schema_version: oci::INDEX_SCHEMA_VERSION,
            media_type: Some(oci::OCI_IMAGE_INDEX_MEDIA_TYPE.to_string()),
            artifact_type: None,
            manifests: vec![oci::ImageIndexEntry {
                media_type: oci::OCI_IMAGE_MEDIA_TYPE.to_string(),
                digest: format!("sha256:{}", "a".repeat(64)),
                size: 0,
                platform: Some(oci::native::Platform {
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
        let digest = oci::Algorithm::Sha256.hash(&bytes);
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
        let readme_digest = oci::Algorithm::Sha256.hash(readme);
        data.write().blobs.insert(readme_digest.to_string(), readme.to_vec());
        let manifest = oci::Manifest::Image(oci::ImageManifest {
            artifact_type: Some(crate::MEDIA_TYPE_DESCRIPTION_V1.to_string()),
            layers: vec![oci::Descriptor {
                media_type: crate::MEDIA_TYPE_MARKDOWN.to_string(),
                digest: readme_digest.to_string(),
                size: i64::try_from(readme.len()).expect("test blob fits i64"),
                urls: None,
                artifact_type: None,
                annotations: None,
            }],
            annotations: Some([(oci::annotations::TITLE.to_string(), "Widget".to_string())].into()),
            ..Default::default()
        });
        let bytes = serde_json::to_vec_pretty(&manifest).expect("manifest serializes");
        let digest = oci::Algorithm::Sha256.hash(&bytes);
        data.write()
            .manifests
            .insert("127.0.0.1/x:__ocx.desc".to_string(), (bytes, digest.to_string()));
        readme_digest.hex().to_string()
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
        let publisher = Publisher::new(oci::Client::with_transport(Box::new(StubTransport::new(data))));
        let root = committed_root(serde_json::json!({}));

        let rebuilt = observe_and_rebuild(
            &publisher,
            &root,
            &request(TagSelection::Replace(vec!["1.0.0".to_string()])),
            "2026-07-25T00:00:00Z",
            "p/acme/widget.json",
            "acme/widget",
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
        let publisher = Publisher::new(oci::Client::with_transport(Box::new(StubTransport::new(data))));
        let keep = format!("__ocx.keep.sha256-{}", "a".repeat(64));
        let root = committed_root(serde_json::json!({}));
        let request = request(TagSelection::Replace(vec![
            "1.0.0".to_string(),
            "__ocx.desc".to_string(),
            keep.clone(),
        ]));

        let rebuilt = observe_and_rebuild(
            &publisher,
            &root,
            &request,
            "2026-07-25T00:00:00Z",
            "p/acme/widget.json",
            "acme/widget",
        )
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
        let publisher = Publisher::new(oci::Client::with_transport(Box::new(StubTransport::new(data.clone()))));
        let root = committed_root(serde_json::json!({}));
        let request = request(TagSelection::Replace(vec![
            "1.0.0".to_string(),
            "__ocx.desc".to_string(),
        ]));

        observe_and_rebuild(
            &publisher,
            &root,
            &request,
            "2026-07-25T00:00:00Z",
            "p/acme/widget.json",
            "acme/widget",
        )
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
        let publisher = Publisher::new(oci::Client::with_transport(Box::new(StubTransport::new(data))));
        let root = committed_root(serde_json::json!({
            "1.0.0": { "content": format!("sha256:{}", "b".repeat(64)), "observed": "2026-07-01T00:00:00Z" }
        }));

        let rebuilt = observe_and_rebuild(
            &publisher,
            &root,
            &request(TagSelection::FromRegistry),
            "2026-07-25T00:00:00Z",
            "p/acme/widget.json",
            "acme/widget",
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
        let publisher = Publisher::new(oci::Client::with_transport(Box::new(StubTransport::new(data.clone()))));
        let root = committed_root(serde_json::json!({}));
        // The default `request` trusts loopback; this one does not, so the
        // committed root's `oci://127.0.0.1/x` pointer is forbidden.
        let mut request = request(TagSelection::FromRegistry);
        request.trusted_hosts = Vec::new();

        let result = observe_and_rebuild(
            &publisher,
            &root,
            &request,
            "2026-07-25T00:00:00Z",
            "p/acme/widget.json",
            "acme/widget",
        )
        .await;

        assert!(
            matches!(result, Err(AnnounceError::Ssrf(_))),
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
        commits: Vec<(BTreeMap<String, Vec<u8>>, RefUpdate)>,
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
        fn commits(&self) -> Vec<(BTreeMap<String, Vec<u8>>, RefUpdate)> {
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
    fn committed_root_bytes(commit: &(BTreeMap<String, Vec<u8>>, RefUpdate)) -> String {
        String::from_utf8(
            commit
                .0
                .get("p/acme/widget.json")
                .expect("every commit carries the root")
                .clone(),
        )
        .expect("the root is UTF-8")
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
            _path: &str,
            r#ref: &str,
        ) -> Result<Option<Vec<u8>>, ForgeError> {
            Ok(self.roots.get(r#ref).cloned())
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
            files: &BTreeMap<String, Vec<u8>>,
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
        let publisher = Publisher::new(oci::Client::with_transport(Box::new(StubTransport::new(registry))));
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

    /// The 79 a release wrapper branches on is produced by the **CLI
    /// classifier**, not by `AnnounceError::classify` alone.
    ///
    /// Calling `classify()` directly proves only that the variant carries a
    /// code; the process exits 79 only because `AnnounceError` is registered in
    /// the downcast ladder, and deleting that registration leaves such a test
    /// green while every real run drops to exit 1. Driving `classify_error` over
    /// a **boxed** error is what exercises the registration, because that is the
    /// shape the ladder actually receives.
    #[test]
    fn an_unclaimed_namespace_still_exits_not_found_through_the_cli_classifier() {
        let error: Box<dyn std::error::Error + 'static> = Box::new(AnnounceError::UnclaimedPackage {
            package: "acme/widget".to_string(),
            path: "p/acme/widget.json".to_string(),
            base_ref: INDEX_BASE_REF.to_string(),
        });

        assert_eq!(
            classify_error(error.as_ref()),
            ExitCode::NotFound,
            "announcing into an unclaimed package must stay discriminable from a crash"
        );
    }
}
