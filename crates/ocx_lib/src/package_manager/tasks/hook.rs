// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Project-tier "applied set" resolver for the shell-hook trio
//! (`shell-hook`, `shell-direnv`, future `generate direnv` writer).
//!
//! Lives in `package_manager::tasks` rather than `project::hook` so the
//! dependency direction stays one-way (`package_manager` consumes
//! `project`, not vice versa). Project-tier read helpers
//! (`load_project_state`, `ProjectState`, `MissingState`) remain in
//! `crate::project::hook` because they don't touch [`PackageManager`].
//!
//! Resolution failures (cache miss, unknown platform, missing layer) are
//! treated as "tool not installed" and added to `missing`. This matches
//! the prompt-hook contract: a missing tool must never fail the prompt.

use std::sync::Arc;

use crate::oci;
use crate::package::install_info::InstallInfo;
use crate::package_manager::PackageManager;
use crate::project::{DEFAULT_GROUP, ProjectLock};
use crate::shell::AppliedEntry;

/// Bundle of `(InstallInfo, AppliedEntry)` produced by [`collect_applied`].
///
/// `infos` and `entries` are 1:1: the i-th `AppliedEntry` describes the
/// i-th `InstallInfo`. `missing` carries the names of locked tools the
/// object store does not have, so the caller can emit a single-line
/// stderr note per missing tool ("# ocx: cmake not installed; run `ocx
/// pull` to fetch") without re-walking the lock.
pub struct AppliedSet {
    /// Resolved install info for tools present in the object store.
    pub infos: Vec<Arc<InstallInfo>>,
    /// Per-info applied-set entry suitable for fingerprint computation.
    pub entries: Vec<AppliedEntry>,
    /// Names of locked tools (in the default group) that resolve to no
    /// content in the object store.
    pub missing: Vec<String>,
}

/// Walk `lock.tools` filtered to the default group, resolve each tool
/// through `manager`, and partition the results.
///
/// `platforms` is the supported-platform list (typically
/// `crate::conventions::supported_platforms()` from the CLI). The resolver
/// is invoked through the supplied `manager` — caller is responsible for
/// passing a [`PackageManager::offline_view`] when network access must
/// be forbidden.
///
/// Resolution failures (cache miss, unknown platform, missing layer) are
/// treated as "tool not installed" and added to `missing`. This matches
/// the prompt-hook contract: a missing tool must never fail the prompt.
pub async fn collect_applied(
    manager: &PackageManager,
    lock: &ProjectLock,
    platforms: &[oci::Platform],
) -> crate::Result<AppliedSet> {
    let mut infos = Vec::new();
    let mut entries = Vec::new();
    let mut missing = Vec::new();

    for tool in &lock.tools {
        if tool.group != DEFAULT_GROUP {
            continue;
        }

        // V2 ([`LockedResolution::PerPlatform`]): the lock already pins the
        // host-platform leaf digest — reconstruct `repository`+leaf and skip
        // the `manager.resolve()` round-trip; the fingerprint is the resolved
        // leaf (changes on upgrade — acceptable per release notes).
        //
        // V1 ([`LockedResolution::LegacyIndex`]): keep the legacy bridge —
        // the lock pins the registry-side index digest while the object store
        // keys by the platform-selected manifest digest, so walk the cached
        // chain through the offline-mode index.
        let (host_identifier, fingerprint_digest) = match &tool.resolution {
            crate::project::LockedResolution::LegacyIndex(pinned) => {
                let identifier: oci::Identifier = pinned.clone().into();
                let resolved_pinned = match manager.resolve(&identifier, platforms.to_vec()).await {
                    Ok(chain) => chain.pinned,
                    Err(_) => {
                        missing.push(tool.name.clone());
                        continue;
                    }
                };
                (resolved_pinned, pinned.digest().to_string())
            }
            crate::project::LockedResolution::PerPlatform {
                repository,
                platforms: leaves,
            } => {
                // The lock already pins the host-platform leaf — read it
                // directly (host key → `"any"` fallback) and skip the
                // `manager.resolve()` round-trip. The fingerprint is the
                // resolved leaf digest (more correct than the old index
                // digest, which churned when *any* platform changed). An
                // absent host leaf is treated as "tool not installed".
                let leaf = platforms
                    .iter()
                    .find_map(|platform| crate::project::lookup_host_leaf(leaves, platform));
                let Some(leaf) = leaf else {
                    missing.push(tool.name.clone());
                    continue;
                };
                let host_id = repository.clone_with_digest(leaf.clone());
                let pinned = match oci::PinnedIdentifier::try_from(host_id) {
                    Ok(p) => p,
                    Err(_) => {
                        missing.push(tool.name.clone());
                        continue;
                    }
                };
                let fingerprint = leaf.to_string();
                (pinned, fingerprint)
            }
        };
        match manager.find_plain(&host_identifier).await {
            Ok(Some(info)) => {
                entries.push(AppliedEntry {
                    name: tool.name.clone(),
                    manifest_digest: fingerprint_digest,
                    group: tool.group.clone(),
                });
                infos.push(Arc::new(info));
            }
            Ok(None) | Err(_) => {
                missing.push(tool.name.clone());
            }
        }
    }

    Ok(AppliedSet {
        infos,
        entries,
        missing,
    })
}
