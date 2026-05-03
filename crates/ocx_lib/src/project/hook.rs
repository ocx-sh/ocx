// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Read-only helpers shared by the prompt-hook commands (`shell-hook`,
//! `hook-env`) and the future `generate direnv` writer.
//!
//! Two free functions encode the duplicated bookkeeping that previously
//! lived in `ocx_cli`:
//!
//! - [`load_project_state`] resolves the project tier, loads the matching
//!   `ocx.toml` + `ocx.lock`, and reports whether the lock is stale.
//! - [`collect_applied`] walks the locked default group, partitioning tools
//!   into "installed and resolvable" (suitable for export) vs "missing"
//!   (caller emits a stderr note and skips).
//!
//! Both functions are I/O-only — they emit no messages of their own.
//! The caller decides how to surface stale-lock warnings, missing-tool
//! notes, and the empty-input case.
//!
//! Design rationale: the hook trio (and the upcoming Phase 8 / Phase 9
//! commands) all need the same "load → resolve → partition" sequence;
//! extracting the helpers here keeps each CLI command file in the
//! command-pattern shape (transform → call → report) without three copies
//! of the same loop.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::oci;
use crate::package::install_info::InstallInfo;
use crate::package_manager::PackageManager;
use crate::project::{ProjectConfig, ProjectLock, declaration_hash};
use crate::shell::AppliedEntry;

use super::internal::DEFAULT_GROUP;

/// Return type of [`load_project_state`].
///
/// `stale` is computed once at load time so the caller does not have to
/// recompute the declaration hash later. The two paths (`config_path`,
/// `lock_path`) are carried so the caller can produce diagnostic messages
/// referencing the on-disk locations.
pub struct ProjectState {
    /// Parsed `ocx.toml`.
    pub config: ProjectConfig,
    /// Parsed `ocx.lock`.
    pub lock: ProjectLock,
    /// Resolved path to `ocx.toml`.
    pub config_path: PathBuf,
    /// Resolved path to `ocx.lock`.
    pub lock_path: PathBuf,
    /// `true` when the lock's declaration hash does not match the current
    /// config — caller decides UX (warn vs error). Hook commands warn and
    /// continue with stale digests; `ocx exec` errors with exit 65.
    pub stale: bool,
}

/// Reasons [`load_project_state`] returns `Ok(None)`.
///
/// Returned via [`MissingState`] so the caller can distinguish "no project
/// at all" (no message) from "project exists but lock is missing"
/// (caller emits a stderr note pointing the user at `ocx lock`).
pub enum MissingState {
    /// No `ocx.toml` is in scope (CWD walk + home fallback both miss, or
    /// `OCX_NO_PROJECT=1`). Caller exits silently.
    NoProject,
    /// `ocx.toml` was found but the matching `ocx.lock` does not exist.
    /// Caller is expected to emit a one-line stderr note pointing at
    /// `lock_path`.
    LockMissing {
        /// Path the lock would have been at — `<config_dir>/ocx.lock`.
        lock_path: PathBuf,
    },
}

/// Load the project tier `(ProjectConfig, ProjectLock)` for the current
/// working directory.
///
/// `cwd` is the working directory the CLI saw at invocation time;
/// `project_path_override` is the explicit `--project` / `OCX_PROJECT_FILE`
/// path if one was supplied. Home-tier fallback is *not* consulted in this
/// helper — the prompt-hook trio is project-tier only until Phase 9.
///
/// Returns:
///
/// - `Ok(Ok(state))` — both files loaded, with `state.stale` set.
/// - `Ok(Err(MissingState::NoProject))` — no project in scope, caller emits
///   nothing.
/// - `Ok(Err(MissingState::LockMissing { lock_path }))` — config in scope
///   but the lock is missing, caller emits a stderr note keyed off
///   `lock_path`.
/// - `Err(_)` — config or lock file failed to load; caller propagates.
pub async fn load_project_state(
    cwd: &Path,
    project_path_override: Option<&Path>,
) -> crate::Result<Result<ProjectState, MissingState>> {
    let resolved = ProjectConfig::resolve(Some(cwd), project_path_override, None).await?;
    let Some((config_path, lock_path)) = resolved else {
        return Ok(Err(MissingState::NoProject));
    };

    let config = ProjectConfig::from_path(&config_path).await?;

    let Some(lock) = ProjectLock::from_path(&lock_path).await? else {
        return Ok(Err(MissingState::LockMissing { lock_path }));
    };

    let current_hash = declaration_hash(&config);
    let stale = lock.metadata.declaration_hash != current_hash;

    Ok(Ok(ProjectState {
        config,
        lock,
        config_path,
        lock_path,
        stale,
    }))
}

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
