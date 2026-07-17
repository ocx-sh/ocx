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
/// `platform` is the requested platform (typically
/// `oci::Platform::current().unwrap_or_else(oci::Platform::any)` from the
/// CLI). The resolver is invoked through the supplied `manager` — caller is
/// responsible for passing a [`PackageManager::offline_view`] when network
/// access must be forbidden.
///
/// Resolution failures (cache miss, unknown platform, missing layer) are
/// treated as "tool not installed" and added to `missing`. This matches
/// the prompt-hook contract: a missing tool must never fail the prompt.
pub async fn collect_applied(
    manager: &PackageManager,
    lock: &ProjectLock,
    platform: &oci::Platform,
) -> crate::Result<AppliedSet> {
    let mut infos = Vec::new();
    let mut entries = Vec::new();
    let mut missing = Vec::new();

    for tool in &lock.tools {
        if tool.group != DEFAULT_GROUP {
            continue;
        }

        // The lock pins the host-platform leaf digest — read it directly
        // (host key resolution, `Any`-offer fallback) and skip a
        // `manager.resolve()` round-trip. An absent OR ambiguous host leaf is
        // treated as "tool not installed" — the prompt-hook contract must
        // never fail or block on a disambiguation the shell can't perform.
        let oci::Selection::Found((leaf, _key)) = crate::project::lookup_host_leaf(&tool.platforms, platform) else {
            missing.push(tool.name.clone());
            continue;
        };
        let host_id = tool.repository.clone_with_digest(leaf.clone());
        let host_identifier = match oci::PinnedIdentifier::try_from(host_id) {
            Ok(p) => p,
            Err(_) => {
                missing.push(tool.name.clone());
                continue;
            }
        };
        let fingerprint_digest = leaf.to_string();
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
