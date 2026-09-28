// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Public request/outcome types for the announce orchestration.

use std::path::PathBuf;

use crate::forge::{ForkIdentity, PullRequest, PushAccess, RepoCoordinate};

/// How the caller curated the tag set.
#[derive(Debug, Clone)]
pub enum TagSelection {
    /// `--tags`: the list is the universe; a committed tag absent from it is dropped.
    Replace(Vec<String>),
    /// `--tags-file`: union with the committed root; only
    /// [`Replace`](TagSelection::Replace) deletes.
    UnionFile(Vec<String>),
    /// `--refresh`: re-observe every committed tag (catching moved digests)
    /// without scanning the registry or touching yank markers.
    Refresh,
    /// `--tags-from-registry`: union of every tag the physical repository holds
    /// with the committed root; nothing committed is dropped and yank markers survive.
    FromRegistry,
}

/// Where announce writes its rebuilt root + CAS objects.
#[derive(Debug, Clone)]
pub enum AnnounceTarget {
    /// Write the root and new CAS files locally under this directory — no forge
    /// mutation.
    Out(PathBuf),
    /// Open (or update) a pull request from a fork; a missing fork is created
    /// under the coordinate's namespace.
    Fork(RepoCoordinate),
    /// Commit onto the index repository itself and open the pull request from
    /// it, for a credential that can push there (GitHub refuses to fork a
    /// repository into the organization that owns it).
    ///
    /// Still a pull request, never a default-branch push, which would bypass the
    /// governance gate and the `refresh`/`new-package` labelling.
    Direct,
}

/// One package's announce request; the caller loops for multi-root.
#[derive(Debug, Clone)]
pub struct AnnounceRequest {
    /// The logical `<namespace>/<package>` identifier.
    pub package: ocx_oci::PackageRef,
    /// The curated tag selection.
    pub curated: TagSelection,
    /// The write target.
    pub target: AnnounceTarget,
    /// The index repository coordinate (default `ocx-sh/index`).
    pub index_repo: RepoCoordinate,
    /// Tags to mark yanked.
    pub yank: Vec<String>,
    /// Tags to clear the yank marker from.
    pub unyank: Vec<String>,
    /// The reason recorded for every `--yank` in this run.
    pub yank_reason: String,
    /// SSRF escape-hatch hosts/CIDRs for the physical registry, sourced only from
    /// the selected `[registries."<ns>"]` entry.
    pub trusted_hosts: Vec<String>,
    /// Registry authorities (`host[:port]`) allowed over plain HTTP; the dial
    /// scheme decides which proxy variable applies
    /// ([`DialScheme::for_registry`](ocx_oci::ssrf::DialScheme::for_registry)).
    pub insecure_hosts: Vec<String>,
}

/// Whether the announce changed the committed root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnounceStatus {
    /// Byte-identical root and no new CAS object: no commit, no pull request.
    /// `--out` still writes its files.
    Unchanged,
    /// The rebuilt root differed.
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
    /// Whether the root changed.
    pub status: AnnounceStatus,
    /// The opened or updated pull request, if any.
    pub pull_request: Option<PullRequest>,
    /// The verified fork identity, when the run used a fork.
    pub fork: Option<ForkIdentity>,
    /// The paths written under `--out` (sorted, the whole entry even when
    /// unchanged); empty otherwise.
    pub written_paths: Vec<String>,
    /// Whether the `__ocx.desc` observation moved the root's `desc` object.
    pub desc_status: AnnounceStatus,
    /// Reserved tags (the `__ocx` namespace and legacy `<algorithm>.<hex>` keep
    /// tags) dropped from the curated set — reported, not refused.
    pub reserved_tags_dropped: Vec<String>,
    /// The announce branch written; empty under [`AnnounceTarget::Out`], which
    /// pushes nothing.
    pub branch: String,
    /// What the write preflight checked; a run that never probed carries every
    /// row as [`Skipped`](crate::forge::CheckStatus::Skipped). Render through
    /// [`PushAccess::checks`].
    pub capability_checks: PushAccess,
}
