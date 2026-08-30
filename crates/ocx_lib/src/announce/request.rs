// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Public request/outcome types for the announce orchestration.

use std::path::PathBuf;

use crate::forge::{ForkIdentity, PullRequest, RepoCoordinate};
use crate::oci;

/// How the caller curated the tag set (design register C3/C5).
#[derive(Debug, Clone)]
pub enum TagSelection {
    /// `--tags`: the given list **is** the universe. A committed tag absent from
    /// the list is dropped (reference-impl replace semantics).
    Replace(Vec<String>),
    /// `--tags-file`: additive union of the file's tags with the committed root
    /// — deletion only ever happens via [`Replace`](TagSelection::Replace).
    UnionFile(Vec<String>),
    /// `--refresh`: re-observe every tag already in the committed root (catches
    /// moved digests such as `latest`/cascades) without scanning the registry or
    /// touching yank markers.
    Refresh,
    /// `--tags-from-registry`: additive union of every tag the physical
    /// repository currently holds with the committed root. The registry supplies
    /// the candidates instead of the caller; the merge is
    /// [`UnionFile`](TagSelection::UnionFile)'s, so nothing committed is ever
    /// dropped and yank markers survive.
    FromRegistry,
}

/// Where announce writes its rebuilt root + CAS objects.
#[derive(Debug, Clone)]
pub enum AnnounceTarget {
    /// Write the root and new CAS files locally under this directory — no forge
    /// mutation.
    Out(PathBuf),
    /// Open (or update) a fork pull request against the index repository. The
    /// coordinate's `owner` threads into the fork-create target (design register
    /// S12 shared `ocx-contrib/index` fork path).
    Fork(RepoCoordinate),
    /// Commit the announce branch onto the index repository **itself** and open
    /// the pull request from it — no fork anywhere.
    ///
    /// For a publisher whose credential can already push to
    /// [`AnnounceRequest::index_repo`]: OCX's own packages are published from
    /// repositories in the organization that owns the index, and GitHub refuses
    /// to fork a repository into the organization that already owns it, so the
    /// fork path is not merely redundant for them — it cannot run at all.
    ///
    /// Still a **pull request**, never a push to the index's default branch: the
    /// index's governance gate and its `refresh`/`new-package` labelling run on
    /// pull requests, so bypassing them would be strictly worse than forking.
    /// Narrows design register S3 ("always fork") to "always a reviewed pull
    /// request".
    Direct,
}

/// One package's announce request (design register C11/C12 — one package per
/// call; the caller loops for multi-root).
#[derive(Debug, Clone)]
pub struct AnnounceRequest {
    /// The logical `<namespace>/<package>` identifier.
    pub package: oci::Identifier,
    /// The curated tag selection.
    pub curated: TagSelection,
    /// The write target.
    pub target: AnnounceTarget,
    /// The index repository coordinate (default `ocx-sh/index`, design register
    /// C11).
    pub index_repo: RepoCoordinate,
    /// Tags to mark yanked (design register C7).
    pub yank: Vec<String>,
    /// Tags to clear the yank marker from (design register C7).
    pub unyank: Vec<String>,
    /// The reason recorded for every `--yank` in this run (design register C7).
    pub yank_reason: String,
    /// SSRF escape-hatch hosts/CIDRs for the physical registry (design register
    /// X2 — sourced only from the selected `[registries."<ns>"]` entry).
    pub trusted_hosts: Vec<String>,
}

/// Whether the announce changed the committed root (cross-track contract #4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnounceStatus {
    /// The rebuilt root was byte-identical to the committed root and no new CAS
    /// object was produced — no commit, no pull request (design register C6).
    /// `--out` still writes its files: C6 governs forge mutation, not the local
    /// write.
    Unchanged,
    /// The rebuilt root differed — a fork pull request was opened/updated
    /// (`--fork`), or the locally written files (`--out`) carry a change.
    Updated,
}

impl AnnounceStatus {
    /// [`Self::Updated`] when something moved, [`Self::Unchanged`] otherwise.
    pub fn from_changed(changed: bool) -> Self {
        if changed { Self::Updated } else { Self::Unchanged }
    }
}

/// The result of an announce run.
#[derive(Debug)]
pub struct AnnounceOutcome {
    /// The logical `<namespace>/<package>` identifier announced.
    pub package: String,
    /// Whether the root changed (design register C6).
    pub status: AnnounceStatus,
    /// The opened/updated pull request — `None` for `--out` and for an unchanged
    /// run.
    pub pull_request: Option<PullRequest>,
    /// The verified fork identity — `None` for `--out`, for an unchanged run,
    /// and for [`AnnounceTarget::Direct`], which has no fork by construction.
    pub fork: Option<ForkIdentity>,
    /// The relative paths written under the `--out` directory (sorted) — always
    /// the whole entry, unchanged runs included; empty in fork mode.
    pub written_paths: Vec<String>,
    /// Whether the `__ocx.desc` observation moved this run (D6).
    /// [`AnnounceStatus::Updated`] means the root's `desc` object was rebuilt
    /// and its readme (and logo) blobs written as new CAS objects;
    /// [`AnnounceStatus::Unchanged`] means the description tag sits where the
    /// committed root already recorded it, or the package publishes none.
    pub desc_status: AnnounceStatus,
    /// Reserved tags dropped from the curated set (D7) — the OCX-internal
    /// `__ocx` namespace (which carries the keep tag) and the frozen legacy
    /// `<algorithm>.<hex>` keep tags. Dropping them
    /// is not a failure, so they are reported here rather than refused; empty
    /// when the selection carried none.
    pub reserved_tags_dropped: Vec<String>,
}
