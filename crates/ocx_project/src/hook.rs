// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Read-only project-tier loader for `ocx direnv export`; it emits no
//! messages, the caller surfaces staleness and the no-project case.

use std::path::{Path, PathBuf};

use crate::{ProjectConfig, ProjectLock};

/// Return type of [`load_project_state`].
pub struct ProjectState {
    pub config: ProjectConfig,
    pub lock: ProjectLock,
    pub config_path: PathBuf,
    pub lock_path: PathBuf,
    /// The lock does not bind to the config ([`ProjectLock::is_current`]);
    /// the caller decides warn or error.
    pub stale: bool,
}

/// Why [`load_project_state`] found no state.
pub enum MissingState {
    /// No `ocx.toml` in scope, or `OCX_NO_PROJECT=1`.
    NoProject,
    /// `ocx.toml` found, its `ocx.lock` missing.
    LockMissing {
        /// `<config_dir>/ocx.lock`.
        lock_path: PathBuf,
    },
}

/// Load `ocx.toml` + `ocx.lock` for `cwd` or the explicit project path, with
/// no home-tier fallback. `Ok(Err(_))` says why there is no state; `Err(_)`
/// means a file failed to load.
pub async fn load_project_state(
    cwd: &Path,
    project_path_override: Option<&Path>,
) -> crate::Result<Result<ProjectState, MissingState>> {
    // `global: false`: the global tier stays isolated from this resolver.
    let resolved = ProjectConfig::resolve(Some(cwd), project_path_override, None, false).await?;
    let Some((config_path, lock_path)) = resolved else {
        return Ok(Err(MissingState::NoProject));
    };

    let config = ProjectConfig::from_path(&config_path).await?;

    let Some(lock) = ProjectLock::from_path(&lock_path).await? else {
        return Ok(Err(MissingState::LockMissing { lock_path }));
    };

    let stale = !lock.is_current(&config);

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
    use crate::declaration_hash;

    /// Minimal `ocx.toml` with no bindings — its declaration hash is
    /// stable, which lets tests construct matching lock metadata
    /// without resolving any registry.
    const EMPTY_OCX_TOML: &str = "[tools]\n";

    fn write_empty_lock(lock_path: &Path, declaration_hash: &str) {
        let body = format!(
            "[metadata]\nlock_version = 3\ndeclaration_hash_version = 1\n\
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
        let env = ocx_util::env::overrides::lock();
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
