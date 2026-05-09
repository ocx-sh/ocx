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

        // The lock pins the registry-side digest (image-index for
        // multi-platform packages); the object store keys by the
        // platform-selected manifest digest. Walk the cached chain
        // through the offline-mode index to bridge the two.
        let identifier: oci::Identifier = tool.pinned.clone().into();
        let resolved_pinned = match manager.resolve(&identifier, platforms.to_vec()).await {
            Ok(chain) => chain.pinned,
            Err(_) => {
                missing.push(tool.name.clone());
                continue;
            }
        };
        match manager.find_plain(&resolved_pinned).await {
            Ok(Some(info)) => {
                // Fingerprint stability: the `manifest_digest` carried in
                // `AppliedEntry` MUST match the one `hook-env`'s fast path
                // computes from `lock.tools` directly, otherwise the
                // unchanged-prompt path never short-circuits. The lock-side
                // `tool.pinned.digest()` is the canonical choice — it's the
                // identity the lock file commits to, stable across machines
                // and platforms (image-index digest for multi-platform
                // packages). Using `info.identifier.digest()` here would
                // produce the platform-resolved manifest digest, which
                // diverges on multi-platform packages and silently breaks
                // the fast path.
                entries.push(AppliedEntry {
                    name: tool.name.clone(),
                    manifest_digest: tool.pinned.digest().to_string(),
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
