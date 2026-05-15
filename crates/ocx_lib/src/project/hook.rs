// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Read-only project-tier helper for the prompt-hook commands
//! (`shell-hook`, `shell-direnv`) and the future `generate direnv` writer.
//!
//! [`load_project_state`] resolves the project tier, loads the matching
//! `ocx.toml` + `ocx.lock`, and reports whether the lock is stale. The
//! function is I/O-only — it emits no messages of its own; the caller
//! decides how to surface stale-lock warnings and the no-project case.
//!
//! The companion `collect_applied` resolver — which walks the locked
//! default group through a [`PackageManager`] — lives at
//! [`crate::package_manager::collect_applied`] so this module stays a
//! pure project-tier leaf with no upward dependency on
//! `package_manager`.
//!
//! [`PackageManager`]: crate::package_manager::PackageManager

use std::path::{Path, PathBuf};

use crate::project::{ProjectConfig, ProjectLock, declaration_hash};

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
/// path if one was supplied. There is no implicit home-tier fallback; the
/// prompt-hook trio is project-tier only. Global-toolchain shell exposure
/// is handled at the `NoProject` arm of `shell hook` (C2.7,
/// adr_global_toolchain_tier.md §Decision 6), not by this resolver.
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
    // `global: false` — the prompt-hook trio resolves the project tier
    // only. Global-toolchain shell exposure is handled separately at the
    // `NoProject` arm (C2.7), not via this resolver (strict isolation).
    let resolved = ProjectConfig::resolve(Some(cwd), project_path_override, None, false).await?;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal `ocx.toml` with no bindings — its declaration hash is
    /// stable, which lets tests construct matching lock metadata
    /// without resolving any registry.
    const EMPTY_OCX_TOML: &str = "[tools]\n";

    fn write_empty_lock(lock_path: &Path, declaration_hash: &str) {
        let body = format!(
            "[metadata]\nlock_version = 1\ndeclaration_hash_version = 1\n\
             declaration_hash = \"{declaration_hash}\"\n\
             generated_by = \"ocx-test\"\n\
             generated_at = \"2026-04-19T00:00:00Z\"\n"
        );
        std::fs::write(lock_path, body).expect("write ocx.lock");
    }

    /// CWD walk hits no `ocx.toml` (and no home fallback supplied) →
    /// `MissingState::NoProject`.
    #[tokio::test]
    async fn load_returns_no_project_when_cwd_walk_misses() {
        let env = crate::test::env::lock();
        // `home = None` disables this helper's own home probe, but
        // `load_project_state` → `ConfigLoader::project_path` Tier 4 still
        // reads `$OCX_HOME` (default `~/.ocx/ocx.toml`). Sandbox it so a
        // developer's real `~/.ocx/ocx.toml` cannot turn this walk-miss
        // into a spurious project hit (green on clean CI, red locally).
        let _ocx_home = env.isolate_project_home();
        let tmp = tempfile::tempdir().expect("tempdir");
        let result = load_project_state(tmp.path(), None).await.expect("load ok");
        assert!(matches!(result, Err(MissingState::NoProject)));
    }

    /// `ocx.toml` exists but `ocx.lock` does not → `LockMissing`.
    #[tokio::test]
    async fn load_returns_lock_missing_when_config_present_lock_absent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let toml_path = tmp.path().join("ocx.toml");
        std::fs::write(&toml_path, EMPTY_OCX_TOML).expect("write ocx.toml");

        let result = load_project_state(tmp.path(), None).await.expect("load ok");
        let Err(MissingState::LockMissing { lock_path }) = result else {
            panic!("expected LockMissing");
        };
        assert_eq!(lock_path, tmp.path().join("ocx.lock"));
    }

    /// Lock present with a `declaration_hash` that does not match the
    /// current config → `stale = true`.
    #[tokio::test]
    async fn load_returns_state_with_stale_true_on_hash_mismatch() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let toml_path = tmp.path().join("ocx.toml");
        std::fs::write(&toml_path, EMPTY_OCX_TOML).expect("write ocx.toml");
        let lock_path = tmp.path().join("ocx.lock");
        // Deliberately wrong hash → staleness gate must trip.
        write_empty_lock(
            &lock_path,
            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        );

        let result = load_project_state(tmp.path(), None).await.expect("load ok");
        let Ok(state) = result else {
            panic!("expected ProjectState");
        };
        assert!(state.stale, "stale must be true when declaration_hash mismatches");
        assert_eq!(state.config_path, toml_path);
    }

    /// Lock's `declaration_hash` matches the config → `stale = false`.
    #[tokio::test]
    async fn load_returns_state_with_stale_false_on_hash_match() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let toml_path = tmp.path().join("ocx.toml");
        std::fs::write(&toml_path, EMPTY_OCX_TOML).expect("write ocx.toml");
        let lock_path = tmp.path().join("ocx.lock");

        // Compute the canonical declaration hash for the empty config so
        // the lock matches it byte-for-byte.
        let cfg = ProjectConfig::from_path(&toml_path).await.expect("parse cfg");
        let expected_hash = declaration_hash(&cfg);
        write_empty_lock(&lock_path, &expected_hash);

        let result = load_project_state(tmp.path(), None).await.expect("load ok");
        let Ok(state) = result else {
            panic!("expected ProjectState");
        };
        assert!(!state.stale, "stale must be false on hash match");
        assert_eq!(state.config_path, toml_path);
        assert_eq!(state.lock_path, lock_path);
    }

    /// `project_path_override` short-circuits the CWD walk — the helper
    /// must load the explicitly named config even when it lives outside
    /// `cwd`.
    #[tokio::test]
    async fn load_honours_explicit_project_path_override() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let proj_dir = tmp.path().join("custom");
        std::fs::create_dir_all(&proj_dir).expect("mkdir");
        let toml_path = proj_dir.join("workspace.toml");
        std::fs::write(&toml_path, EMPTY_OCX_TOML).expect("write workspace.toml");
        // Lock sits next to the explicit config (lock_path_for derives
        // `<parent>/ocx.lock` regardless of the config's filename).
        let lock_path = proj_dir.join("ocx.lock");
        let cfg = ProjectConfig::from_path(&toml_path).await.expect("parse cfg");
        write_empty_lock(&lock_path, &declaration_hash(&cfg));

        // CWD intentionally points at a different directory — the
        // override must win.
        let unrelated_cwd = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&unrelated_cwd).expect("mkdir cwd");

        let result = load_project_state(&unrelated_cwd, Some(&toml_path))
            .await
            .expect("load ok");
        let Ok(state) = result else {
            panic!("expected ProjectState");
        };
        assert_eq!(state.config_path, toml_path);
        assert_eq!(state.lock_path, lock_path);
    }
}
