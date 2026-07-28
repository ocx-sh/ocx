// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx package announce` orchestration (ocx-sh/ocx#216).
//!
//! A self-contained, forge-neutral routine (reused by `ocx-mirror`) that a
//! publisher runs to update **one** package's entry in the `ocx-sh/index`
//! repository. It observes an owner-curated set of registry tags, rebuilds the
//! package's index root byte-exactly (CONTRACTS §14, via
//! [`crate::oci::index::serialize_root`]) and stores each observed tag's image
//! index verbatim as a content-addressed object, then either writes the result
//! locally (`--out`) or opens/updates a fork pull request against the index
//! (`--fork`). Server-side privileged verification (ownership, claim
//! re-derivation) happens in the index CI, never here.
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
//! [`GitHubForge`](crate::forge::GitHubForge).
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

use crate::forge::{BranchComparison, ForkIdentity, GitHubForge, RefUpdate, RepoCoordinate};
use crate::oci::index::serialize_root;
use crate::publisher::Publisher;

/// The main branch of the index repository (design register C10).
const INDEX_BASE_REF: &str = "main";

/// Announce one package (design register C11/C12 — one package per call).
///
/// `forge` is `Some` for every mode that reaches a remote: it reads the
/// committed root (C10) and, for [`AnnounceTarget::Fork`], commits and opens the
/// pull request. A `None` forge cannot read the committed root, so announce
/// returns [`AnnounceError::ForgeRequired`].
///
/// # Errors
///
/// Returns an [`AnnounceError`] for a missing forge, an unclaimed namespace, an
/// SSRF-forbidden physical host, a curated tag that does not resolve, a
/// yank/unyank input error, or any forge / filesystem failure. See
/// [`AnnounceError`] for the full taxonomy.
pub async fn announce(
    publisher: &Publisher,
    forge: Option<&GitHubForge>,
    request: AnnounceRequest,
) -> Result<AnnounceOutcome, AnnounceError> {
    let forge = forge.ok_or(AnnounceError::ForgeRequired)?;
    let package = request.package.repository().to_string();
    let root_path = format!("p/{package}.json");
    let branch = format!("indexbot-announce-{}", package.replace('/', "-"));

    // 0. Resolve the fork's REAL identity first, so every later endpoint is
    //    built from it — the announce branch lives on the fork, and `--fork
    //    <owner>/<repo>` may name one renamed away from the upstream's
    //    repository name. Deliberately read-only (`find_fork`, never
    //    `ensure_fork`): an unchanged run must not provoke a fork create (C6).
    let existing_fork = match &request.target {
        AnnounceTarget::Fork(target) => forge.find_fork(&request.index_repo, target).await?,
        AnnounceTarget::Out(_) => None,
    };
    let fork_coordinate = existing_fork.as_ref().map(ForkIdentity::coordinate);

    // 0b. Is the announce branch still carrying work, or is it residue?
    //     The branch is per package (C4) and outlives every pull request opened
    //     from it, so its mere existence says nothing. Accumulating onto a spent
    //     branch re-proposes already-merged commits and the pull request
    //     conflicts on the root file every announce edits (#228).
    let branch_state = resolve_branch_state(forge, &request.index_repo, fork_coordinate.as_ref(), &branch).await?;

    // 1. Read the committed root (C10), from the fork branch head while the
    //    branch is live (C4) so sequential announces accumulate, else from the
    //    index main.
    let root_read = read_committed_root(
        forge,
        &request.index_repo,
        fork_coordinate.as_ref().filter(|_| branch_state.is_live()),
        &root_path,
        &branch,
    )
    .await?;
    let committed_bytes = pipeline::require_root(&package, &root_path, &root_read.base_ref, root_read.bytes)?;
    let committed_root: Value =
        serde_json::from_slice(&committed_bytes).map_err(|source| AnnounceError::RootParse {
            path: root_path.clone(),
            source,
        })?;
    if !committed_root.is_object() {
        return Err(AnnounceError::RootNotObject { path: root_path });
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

    // 7. Dispatch: write locally (`--out`) or open/update a fork PR (`--fork`).
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
            })
        }
        AnnounceTarget::Fork(target) => {
            if unchanged {
                // C6: a pure no-op — unless a prior fork announce left commits
                // on the branch that the index base does not have. Such a run
                // may have committed content whose pull request then failed to
                // open, stranding the update; ensure an open PR exists so it is
                // never lost (design register C6 amendment). `branch_sha` is
                // `Some` only for a LIVE branch, which is the whole predicate:
                // a branch sitting at, already merged into, or squash-merged
                // away from the base carries nothing unmerged, and opening a
                // pull request for it produces the unmergeable one #228 is
                // about.
                if let Some(fork) = &existing_fork
                    && root_read.branch_sha.is_some()
                {
                    let pull_request = forge
                        .open_or_update_pull_request(
                            &request.index_repo,
                            &fork.owner,
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
                        fork: Some(fork.clone()),
                        written_paths: Vec::new(),
                        reserved_tags_dropped: reserved_dropped,
                        desc_status,
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
                });
            }
            // C8: reuse the already-resolved fork, else create one under the
            // requested owner (S12 shared fork), then commit onto the
            // accumulating branch head (C4, live branch) or the upstream base
            // SHA (C8 — first announce, and equally a spent branch, whose head
            // `read_committed_root` was told to ignore) — one atomic multi-file
            // commit (C15).
            let fork = match existing_fork {
                Some(fork) => fork,
                None => forge.ensure_fork(&request.index_repo, Some(&target.owner)).await?,
            };
            let base_sha = match root_read.branch_sha {
                Some(sha) => sha,
                None => forge
                    .get_ref_sha(&request.index_repo, &format!("heads/{INDEX_BASE_REF}"))
                    .await?
                    .ok_or_else(|| AnnounceError::MissingBaseRef {
                        repo: request.index_repo.full_name(),
                    })?,
            };
            // F2/C4: the ref update is fast-forward-only (CAS). If a concurrent
            // announce advanced the branch between our read and our commit,
            // re-read the new head, re-run the WHOLE regeneration against it,
            // and retry exactly once so the concurrent change is preserved,
            // never overwritten.
            match forge
                .commit_files(
                    &fork.coordinate(),
                    &branch,
                    &base_sha,
                    &message,
                    &files,
                    branch_state.ref_update(),
                )
                .await
            {
                Ok(_) => {}
                Err(crate::forge::ForgeError::NonFastForward { .. }) => {
                    let head_sha = forge
                        .get_ref_sha(&fork.coordinate(), &format!("heads/{branch}"))
                        .await?
                        .ok_or_else(|| AnnounceError::MissingBaseRef {
                            repo: fork.full_name.clone(),
                        })?;
                    let head_bytes = forge
                        .get_file_contents(&fork.coordinate(), &root_path, &head_sha)
                        .await?
                        .ok_or_else(|| AnnounceError::MissingHeadRoot {
                            repo: fork.full_name.clone(),
                            path: root_path.clone(),
                            sha: head_sha.clone(),
                        })?;
                    let head_root: Value =
                        serde_json::from_slice(&head_bytes).map_err(|source| AnnounceError::RootParse {
                            path: root_path.clone(),
                            source,
                        })?;
                    let merged =
                        observe_and_rebuild(publisher, &head_root, &request, &now, &root_path, &package).await?;
                    // The retry re-resolved the curated set against the winning
                    // head, so its drop list supersedes — never unions with —
                    // the pre-race one: for `--tags-from-file`/`--refresh` the base
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
                    } else {
                        forge
                            // The winning head IS our base now, so this is a
                            // fast-forward by construction — and it must stay
                            // CAS-checked, since a third announce could have
                            // advanced the branch again while we regenerated.
                            .commit_files(
                                &fork.coordinate(),
                                &branch,
                                &head_sha,
                                &message,
                                &merged.files,
                                RefUpdate::FastForward,
                            )
                            .await?;
                    }
                }
                Err(other) => return Err(other.into()),
            }
            // Re-announce reuses the open PR without patching title/body — the
            // C4 branch-head commits carry the update.
            let pull_request = forge
                .open_or_update_pull_request(
                    &request.index_repo,
                    &fork.owner,
                    &branch,
                    INDEX_BASE_REF,
                    &message,
                    &pull_request_body,
                )
                .await?;
            Ok(AnnounceOutcome {
                package,
                status,
                pull_request: Some(pull_request),
                fork: Some(fork),
                written_paths: Vec::new(),
                reserved_tags_dropped: reserved_dropped,
                desc_status,
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
/// what preserves a concurrent announce's additions: `--tags-from-file` and
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
    let physical = pipeline::guarded_physical(base_root, &request.trusted_hosts).await?;
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

/// The outcome of reading the committed root: its bytes (`None` when the path is
/// absent), the fork branch head SHA when the announce branch already exists
/// (C4, reused as the commit base), and the ref it was read from.
struct RootRead {
    bytes: Option<Vec<u8>>,
    branch_sha: Option<String>,
    base_ref: String,
}

/// Read the committed root per C4/C10: the fork's own announce branch head when
/// it exists (accumulate), else the index repository's `main`.
///
/// `fork` is the **verified** fork coordinate — `None` for `--out` and for a
/// fork target with no fork yet, both of which can only read the index base.
/// Reading the branch through the verified coordinate rather than the raw
/// `--fork` value is what makes C4 accumulation work for a fork renamed away
/// from the upstream's repository name.
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
    /// The branch is still carrying work: an open pull request holds it, or its
    /// commits fast-forward onto the index base. Accumulate onto it (C4), so two
    /// announces before a merge land in one pull request with both tag sets.
    Live,
    /// The branch exists and carries nothing the base needs — its pull request
    /// merged (leaving it `Diverged` under a squash merge, `Behind` under a merge
    /// commit) or was closed unmerged. Residue: rebuild from the base and repoint
    /// the ref at the result.
    Spent,
}

impl BranchState {
    /// Whether the branch may serve as the base for this announce.
    fn is_live(&self) -> bool {
        matches!(self, Self::Live)
    }

    /// How the ref update must behave. Repointing a spent branch at a commit
    /// built on the upstream base is deliberately not a fast-forward — refusing
    /// the rewrite would preserve the already-merged commits that make the branch
    /// unusable in the first place.
    fn ref_update(&self) -> RefUpdate {
        match self {
            Self::Spent => RefUpdate::Reset,
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
/// pull request means the latter.
async fn resolve_branch_state(
    forge: &GitHubForge,
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
    match forge
        .compare_branch(index_repo, INDEX_BASE_REF, &fork.owner, branch)
        .await?
    {
        // Strictly ahead: the commits are unmerged and they fast-forward, so
        // keep them whether or not a pull request is open — when none is, the
        // C6 amendment opens the one they never got.
        BranchComparison::Ahead => Ok(BranchState::Live),
        BranchComparison::Identical | BranchComparison::Behind => Ok(BranchState::Spent),
        BranchComparison::Diverged => {
            if forge
                .find_open_pull_request(index_repo, &fork.owner, branch)
                .await?
                .is_some()
            {
                Ok(BranchState::Live)
            } else {
                Ok(BranchState::Spent)
            }
        }
    }
}

async fn read_committed_root(
    forge: &GitHubForge,
    index_repo: &RepoCoordinate,
    fork: Option<&RepoCoordinate>,
    root_path: &str,
    branch: &str,
) -> Result<RootRead, AnnounceError> {
    // The announce branch only ever exists on the fork; an absent branch reads
    // back as `None`, falling through to `main`.
    if let Some(fork) = fork {
        let branch_sha = forge.get_ref_sha(fork, &format!("heads/{branch}")).await?;
        if branch_sha.is_some() {
            let bytes = forge.get_file_contents(fork, root_path, branch).await?;
            return Ok(RootRead {
                bytes,
                branch_sha,
                base_ref: branch.to_string(),
            });
        }
    }
    let bytes = forge.get_file_contents(index_repo, root_path, INDEX_BASE_REF).await?;
    Ok(RootRead {
        bytes,
        branch_sha: None,
        base_ref: INDEX_BASE_REF.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
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
                owner: "ocx-sh".to_string(),
                repo: "index".to_string(),
            },
            yank: Vec::new(),
            unyank: Vec::new(),
            yank_reason: String::new(),
            // The loopback physical host is forbidden by default; trusting it
            // is what lets the observe loop reach the stub transport.
            trusted_hosts: vec!["127.0.0.1".to_string()],
        }
    }

    /// Seed the stub with a one-platform image index served at `127.0.0.1/x:<tag>`.
    ///
    /// Pretty-printed for the same reason as `pipeline::tests::seed_manifest`:
    /// the served encoding must differ from serde's canonical one, or a
    /// re-serializing regression stays invisible to every byte assertion.
    fn seed_index(data: &StubTransportData, tag: &str) {
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
        let canonical = format!("sha256.{}", "a".repeat(64));
        let root = committed_root(serde_json::json!({}));
        let request = request(TagSelection::Replace(vec![
            "1.0.0".to_string(),
            "__ocx.desc".to_string(),
            canonical.clone(),
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
            vec!["__ocx.desc".to_string(), canonical],
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
}
