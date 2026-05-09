// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Configuration discovery and loading.
//!
//! Discovery is deliberately separated from loading so that future tiers
//! (e.g., the project-level `ocx.toml` walk in #33) can be added by
//! extending [`ConfigLoader::discover_paths`] without rewriting any other
//! function. CWD is passed in via [`ConfigInputs`] rather than read from
//! the environment, keeping the loader testable without filesystem
//! side effects.

use std::path::{Path, PathBuf};

use futures::future::join_all;
use tokio::io::AsyncReadExt;

use crate::Result;
use crate::config::{Config, error::ConfigSource, error::Error};
use crate::log;

/// Upper bound for a single config file. Config files are expected to be
/// well under 1 KiB; 64 KiB is a generous safety cap that also rules out
/// accidentally pointing `--config` at a multi-megabyte file.
const MAX_CONFIG_SIZE: u64 = 64 * 1024;

/// Inputs to config discovery — captures all caller-provided context so the
/// loader never reads ambient state directly.
pub struct ConfigInputs<'a> {
    /// `--config FILE` CLI flag (highest priority among explicit paths).
    pub explicit_path: Option<&'a Path>,
    /// `--project <FILE>` CLI flag (highest priority among project-tier sources).
    pub explicit_project_path: Option<&'a Path>,
    /// CWD for the project-tier walk (#33). Pass `None` to disable the walk.
    pub cwd: Option<&'a Path>,
}

/// Configuration loader. Stateless namespace for the discovery and loading
/// pipeline.
pub struct ConfigLoader;

impl ConfigLoader {
    /// Top-level entry: build the ordered path list, load, and merge.
    ///
    /// Layering (lowest → highest precedence):
    /// 1. system / user / `$OCX_HOME` tiers — skipped when `OCX_NO_CONFIG=1`
    /// 2. `OCX_CONFIG` — if set and non-empty
    /// 3. `--config FILE` (via [`ConfigInputs::explicit_path`])
    ///
    /// Explicit paths (both env-var and CLI) always load if set; they layer
    /// on top of the discovered chain (or on top of nothing, if
    /// `OCX_NO_CONFIG=1` pruned it). Empty `OCX_CONFIG=""` is treated
    /// as unset — an escape hatch so users can disable ambient env-var
    /// config without unsetting it.
    ///
    /// All filesystem I/O uses [`tokio::fs`] so the loader can run inside
    /// the async runtime without blocking a worker thread.
    ///
    /// # Errors
    /// Returns an error on missing explicit files, I/O failure, or TOML
    /// parse failure.
    pub async fn load(inputs: ConfigInputs<'_>) -> Result<Config> {
        let no_config = crate::env::flag("OCX_NO_CONFIG", false);
        let raw_env_config_file = crate::env::var("OCX_CONFIG");
        if raw_env_config_file.as_deref() == Some("") {
            log::debug!("OCX_CONFIG is set to empty string — skipped via escape hatch");
        }
        let env_config_file = raw_env_config_file.filter(|s| !s.is_empty());

        // Resolve the project-tier path first so a missing `--project` or
        // `OCX_PROJECT` surfaces as `FileNotFound` (exit 79) before we
        // read anything else. Phase 1 only wires error propagation — the
        // returned path itself is consumed in later phases once the
        // project-config schema lands.
        let _project_path = Self::project_path(inputs.cwd, inputs.explicit_project_path).await?;

        let mut paths: Vec<PathBuf> = if no_config {
            Vec::new()
        } else {
            Self::discover_paths().await
        };
        if let Some(env_path) = env_config_file {
            paths.push(PathBuf::from(env_path));
        }
        if let Some(explicit) = inputs.explicit_path {
            paths.push(explicit.to_path_buf());
        }
        Self::load_and_merge(&paths).await
    }

    /// Discover the ordered list of config files to load (lowest precedence
    /// first).
    ///
    /// Returns `[system_path, user_path, home_path]` filtering out `None`
    /// and nonexistent files. The project-tier path is resolved separately
    /// by [`Self::project_path`] because it returns a single
    /// `Option<PathBuf>` rather than joining the tier chain.
    ///
    /// Async because it calls [`tokio::fs::symlink_metadata`] per candidate
    /// path. `NotFound` candidates are silently skipped. Symlinked candidates
    /// are rejected with a warning — an attacker who can write to a
    /// discovered-tier location (`/etc/ocx/config.toml`,
    /// `~/.config/ocx/config.toml`, `$OCX_HOME/config.toml`) could otherwise
    /// point the link at any readable file and surface its contents via a
    /// parse-error message or provoke unexpected side effects on load.
    /// Explicit paths (`--config`, `OCX_CONFIG`) are trusted caller
    /// input and are not subject to this check. Other I/O errors (permission
    /// denied, stale NFS handle, EIO) are logged as warnings and the
    /// candidate is still skipped — discovery never fails the whole process,
    /// but an unreadable `~/.ocx/config.toml` should at least be
    /// *diagnosable*. A race between discovery and read surfaces later as
    /// [`Error::Io`] during [`Self::load_and_merge`].
    pub async fn discover_paths() -> Vec<PathBuf> {
        let mut candidates: Vec<PathBuf> = Vec::new();
        candidates.push(Self::system_path());
        if let Some(user) = Self::user_path() {
            candidates.push(user);
        }
        if let Some(home) = Self::home_path() {
            candidates.push(home);
        }
        // join_all preserves input order, so the precedence semantics of the
        // candidate list (system → user → $OCX_HOME) are unchanged. Running
        // the symlink_metadata calls concurrently shaves two sequential
        // filesystem round-trips on startup. We use `symlink_metadata` rather
        // than `try_exists` (which follows symlinks) so the symlink-rejection
        // branch can observe the link itself without dereferencing it.
        let checks = join_all(candidates.iter().map(tokio::fs::symlink_metadata)).await;
        candidates
            .into_iter()
            .zip(checks)
            .filter_map(|(path, result)| match result {
                Ok(meta) if meta.file_type().is_symlink() => {
                    log::warn!(
                        "skipping symlinked config candidate {} (discovered-tier config files must not be symlinks)",
                        path.display()
                    );
                    None
                }
                Ok(_) => Some(path),
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => None,
                Err(source) => {
                    log::warn!("skipping unreadable config candidate {}: {source}", path.display());
                    None
                }
            })
            .collect()
    }

    /// Resolve the project-tier `ocx.toml` path.
    ///
    /// Precedence: `explicit` (from `--project`) > `OCX_PROJECT` env
    /// var > CWD walk. `OCX_NO_PROJECT=1` prunes the walk and the env var
    /// but does NOT prune the explicit flag (trusted caller intent, per
    /// ADR `adr_project_toolchain_config.md` Amendment G3). Empty
    /// `OCX_PROJECT=""` is treated as unset (escape hatch, matches
    /// `OCX_CONFIG=""`).
    ///
    /// The CWD walk stops at the first `ocx.toml`, any `.git/` boundary,
    /// or `OCX_CEILING_PATH`. Discovered (walked) paths reject symlinks;
    /// explicit paths (flag / env) follow symlinks (trusted caller).
    ///
    /// # Errors
    /// Returns [`Error::FileNotFound`] when an explicit source (`--project`
    /// or `OCX_PROJECT`) names a path that does not exist (exit 79).
    /// CWD-walk misses return `Ok(None)` — the walk treats absence as a
    /// non-event, unlike explicit caller intent.
    pub async fn project_path(
        cwd: Option<&Path>,
        explicit: Option<&Path>,
    ) -> std::result::Result<Option<PathBuf>, crate::config::error::Error> {
        // Tier 1: explicit `--project` flag — highest priority. Follows
        // symlinks (trusted caller intent). Missing file → FileNotFound.
        // Not gated by `OCX_NO_PROJECT` (Amendment G3).
        if let Some(path) = explicit {
            return Self::resolve_explicit_project_path(path).await;
        }

        // `OCX_NO_PROJECT=1` prunes BOTH env-var and CWD-walk lookups
        // (Amendment G3). This diverges from `OCX_NO_CONFIG` + explicit
        // env-var tier-config behavior deliberately: the project file is
        // a single, unique source, not a composed tier chain — so "turn
        // project discovery off entirely" is the useful kill switch.
        let no_project = crate::env::flag("OCX_NO_PROJECT", false);
        if no_project {
            return Ok(None);
        }

        // Tier 2: `OCX_PROJECT` env var. Empty string is the escape
        // hatch (matches `OCX_CONFIG=""` pattern in `load()` above).
        let raw_env = crate::env::var("OCX_PROJECT");
        if raw_env.as_deref() == Some("") {
            log::debug!("OCX_PROJECT is set to empty string — skipped via escape hatch");
        }
        if let Some(env_path) = raw_env.filter(|s| !s.is_empty()) {
            return Self::resolve_explicit_project_path(Path::new(&env_path)).await;
        }

        // Tier 3: CWD walk.
        let walk_result = match cwd {
            Some(start) => {
                let ceiling = crate::env::var("OCX_CEILING_PATH").map(PathBuf::from);
                Self::walk_for_project_file(start, ceiling.as_deref()).await
            }
            None => None,
        };
        if walk_result.is_some() {
            return Ok(walk_result);
        }
        // Tier 4: `$OCX_HOME/ocx.toml` fallback. Amendment C: mutual
        // exclusion — never reached when the CWD walk hit, so the home
        // tier wholesale-replaces project tier rather than composing.
        // See plan_project_toolchain.md Phase 9.
        Ok(Self::home_project_path().await)
    }

    /// Resolve an explicit project-tier path (from `--project` or
    /// `OCX_PROJECT`). Explicit paths follow symlinks and must resolve
    /// to a regular file.
    async fn resolve_explicit_project_path(
        path: &Path,
    ) -> std::result::Result<Option<PathBuf>, crate::config::error::Error> {
        // `tokio::fs::metadata` follows symlinks (trusted caller intent, G5).
        match tokio::fs::metadata(path).await {
            Ok(meta) if meta.file_type().is_file() => Ok(Some(path.to_path_buf())),
            Ok(_) => {
                // Path exists but is not a regular file (directory, device,
                // FIFO). Phase 1 discards the resolved path, so if we
                // returned `Ok(Some(path))` here the defect would silently
                // slip through discovery and surface only at parse time.
                // Surface now as `Error::Io` (exit 74, ADR G9).
                Err(Error::Io {
                    path: path.to_path_buf(),
                    tier: ConfigSource::Project,
                    source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a regular file"),
                })
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Err(Error::FileNotFound {
                path: path.to_path_buf(),
                tier: ConfigSource::Project,
            }),
            // Other I/O errors (permission denied, stale NFS handle, EIO).
            // Phase 1 discards the resolved path, so if we returned
            // `Ok(Some(path))` here the error would silently vanish. Surface
            // as `Error::Io` (exit 74) so an explicit caller gets a
            // diagnosable failure instead of a phantom success (ADR G9).
            Err(source) => Err(Error::Io {
                path: path.to_path_buf(),
                tier: ConfigSource::Project,
                source,
            }),
        }
    }

    /// Walk up from `start`, looking for `ocx.toml`. Stops at the first hit,
    /// any `.git/` boundary, or `ceiling` (if set). Returns `None` if the
    /// walk exhausts the filesystem root without finding a project file.
    ///
    /// Symlinks discovered at the walk step are rejected (G5): the candidate
    /// is skipped and the walk continues upward. `.git/` detection uses
    /// `symlink_metadata` as well so a `.git` symlink does not silently
    /// weaken the boundary check. A `.git` file (git worktree linkfile) also
    /// counts as a boundary — we match git's own "any `.git` entry" rule.
    async fn walk_for_project_file(start: &Path, ceiling: Option<&Path>) -> Option<PathBuf> {
        let mut current = start;
        loop {
            // Probe `.git/` and `ocx.toml` concurrently; `tokio::join!` just
            // overlaps the two stat round-trips. Precedence at each level
            // (Amendment F): a valid `ocx.toml` at the current level wins
            // over the `.git/` boundary — the boundary only prevents walking
            // UP past the repo root, not accepting a project file AT the
            // repo root. The `.git/` gate therefore fires AFTER the
            // candidate-hit check but before ascending.
            let candidate = current.join("ocx.toml");
            let (git_present, candidate_meta) =
                tokio::join!(Self::has_git_dir(current), tokio::fs::symlink_metadata(&candidate),);

            match &candidate_meta {
                Ok(meta) if meta.file_type().is_file() => {
                    return Some(candidate);
                }
                Ok(meta) if meta.file_type().is_symlink() => {
                    log::warn!(
                        "skipping symlinked project candidate {} (CWD walk rejects symlinks; use --project or OCX_PROJECT to opt in)",
                        candidate.display()
                    );
                }
                Ok(_) => {
                    // Directory or other non-regular file named `ocx.toml` —
                    // treat as absent. A weird mount point shouldn't derail
                    // discovery or cause silent success.
                }
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => {
                    log::warn!(
                        "skipping unreadable project candidate {}: {source}",
                        candidate.display()
                    );
                }
            }

            // No valid hit at this level — respect the repo boundary before
            // ascending so we don't walk into a parent repository.
            if git_present {
                return None;
            }

            // Ceiling check runs AFTER probing the current directory so a
            // ceiling that points exactly at a workspace containing
            // `ocx.toml` still discovers it. The ceiling bounds the walk
            // from going ABOVE it, not from reading AT it.
            if let Some(ceiling) = ceiling
                && current == ceiling
            {
                return None;
            }

            match current.parent() {
                Some(parent) => current = parent,
                // Reached the filesystem root without a hit.
                None => return None,
            }
        }
    }

    /// Returns `true` if `dir/.git` exists as any filesystem entry (directory,
    /// file — the git worktree "linkfile" case — or symlink).
    ///
    /// Fail-closed on non-`NotFound` I/O errors: if the filesystem reports
    /// `PermissionDenied` or `EIO` for `.git`, we cannot *disprove* the
    /// presence of a repository boundary, and silently letting the walk
    /// cross into the parent would weaken the boundary check. The safer
    /// default is to assume the boundary exists.
    async fn has_git_dir(dir: &Path) -> bool {
        match tokio::fs::symlink_metadata(dir.join(".git")).await {
            Ok(_) => true,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => false,
            Err(source) => {
                log::warn!(
                    "treating {}/.git as a repository boundary due to I/O error: {source}",
                    dir.display()
                );
                true
            }
        }
    }

    /// Load and merge an ordered list of config files (lowest precedence
    /// first). Missing files at this stage are an error — discovery should
    /// have filtered them. Async I/O via [`tokio::fs`] + parse + merge.
    ///
    /// Accepts any slice of `AsRef<Path>` so callers can pass `&[PathBuf]`,
    /// `&[&Path]`, or a borrowed single path without forced allocation.
    ///
    /// # Errors
    /// Returns an error if any file is missing, unreadable, exceeds
    /// [`MAX_CONFIG_SIZE`], or contains invalid TOML.
    pub async fn load_and_merge<P: AsRef<Path>>(paths: &[P]) -> Result<Config> {
        let mut config = Config::default();
        for path in paths {
            let path = path.as_ref();
            let file = match tokio::fs::File::open(path).await {
                Ok(f) => f,
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                    return Err(Error::FileNotFound {
                        path: path.to_path_buf(),
                        tier: ConfigSource::Config,
                    }
                    .into());
                }
                Err(source) => {
                    return Err(Error::Io {
                        path: path.to_path_buf(),
                        tier: ConfigSource::Config,
                        source,
                    }
                    .into());
                }
            };
            let metadata = file.metadata().await.map_err(|source| Error::Io {
                path: path.to_path_buf(),
                tier: ConfigSource::Config,
                source,
            })?;
            if !metadata.file_type().is_file() {
                return Err(Error::Io {
                    path: path.to_path_buf(),
                    tier: ConfigSource::Config,
                    source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "config path is not a regular file"),
                }
                .into());
            }
            if metadata.len() > MAX_CONFIG_SIZE {
                return Err(Error::FileTooLarge {
                    path: path.to_path_buf(),
                    size: metadata.len(),
                    limit: MAX_CONFIG_SIZE,
                }
                .into());
            }
            // Bounded read so synthetic files whose `metadata.len()` is 0 but whose read is
            // unbounded (e.g. /proc/self/mem, /proc/self/maps on Linux) can't bypass the size
            // cap and hang/exhaust memory. The `metadata.len()` pre-check above still fast-paths
            // normal oversized files without reading any bytes.
            let mut contents = String::new();
            let mut taken = file.take(MAX_CONFIG_SIZE + 1);
            taken.read_to_string(&mut contents).await.map_err(|source| Error::Io {
                path: path.to_path_buf(),
                tier: ConfigSource::Config,
                source,
            })?;
            if contents.len() as u64 > MAX_CONFIG_SIZE {
                return Err(Error::FileTooLarge {
                    path: path.to_path_buf(),
                    size: contents.len() as u64,
                    limit: MAX_CONFIG_SIZE,
                }
                .into());
            }
            let parsed: Config = toml::from_str(&contents).map_err(|source| Error::Parse {
                path: path.to_path_buf(),
                source,
            })?;
            config.merge(parsed);
        }
        Ok(config)
    }

    /// System config: `/etc/ocx/config.toml`.
    pub fn system_path() -> PathBuf {
        PathBuf::from("/etc/ocx/config.toml")
    }

    /// User config: `$XDG_CONFIG_HOME/ocx/config.toml` or
    /// `~/.config/ocx/config.toml` (via `dirs::config_dir`).
    pub fn user_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("ocx").join("config.toml"))
    }

    /// `$OCX_HOME` directory, falling back to `~/.ocx`.
    ///
    /// Single resolver shared by [`Self::home_path`] (returns
    /// `<dir>/config.toml`), [`Self::home_project_path`] (returns
    /// `<dir>/ocx.toml`), and [`Self::home_init_path`] (returns the
    /// per-shell init file). Returns `None` when `OCX_HOME` is unset and
    /// `dirs::home_dir()` cannot resolve a home directory (e.g., a service
    /// account with no `$HOME`).
    ///
    /// Kept private: callers compose well-known children through one of
    /// the named `home_*_path()` accessors above so `$OCX_HOME` path math
    /// stays in this module. External code that needs a `$OCX_HOME`-rooted
    /// path should add (or extend) such an accessor rather than reach for
    /// the bare directory.
    fn home_dir() -> Option<PathBuf> {
        if let Some(ocx_home) = crate::env::var("OCX_HOME") {
            return Some(PathBuf::from(ocx_home));
        }
        dirs::home_dir().map(|h| h.join(".ocx"))
    }

    /// `$OCX_HOME/config.toml`, falling back to `~/.ocx/config.toml`.
    pub fn home_path() -> Option<PathBuf> {
        Self::home_dir().map(|d| d.join("config.toml"))
    }

    /// `$OCX_HOME/init.<shell>` — per-shell init file landing path.
    ///
    /// Composes [`Self::home_dir`] (kept private) with
    /// [`crate::shell::Shell::default_init_filename`] so the per-shell
    /// basename table lives on `Shell` itself.
    ///
    /// Returns `None` when `$OCX_HOME` cannot be resolved and there is no
    /// home directory to fall back to.
    pub fn home_init_path(shell: crate::shell::Shell) -> Option<PathBuf> {
        Self::home_dir().map(|d| d.join(shell.default_init_filename()))
    }

    /// `$OCX_HOME/ocx.toml` fallback for project-tier discovery (Amendment C).
    ///
    /// Returns `Some(path)` only when the file exists as a regular file.
    /// Symlinks are followed (G5: explicit-trust location, matching how the
    /// ambient loader treats `$OCX_HOME/config.toml`). Used only when the
    /// CWD walk returned `None` — never composes with a project-tier hit
    /// (Amendment C: wholesale replacement).
    async fn home_project_path() -> Option<PathBuf> {
        let home = Self::home_dir()?;
        let candidate = home.join("ocx.toml");
        // G5-style trust: $OCX_HOME is caller-chosen, so follow symlinks
        // here (matches how home_path() treats $OCX_HOME/config.toml).
        // Distinct from walk_for_project_file which rejects symlinks
        // (untrusted ancestor dirs). Amendment C: this fallback is
        // wholesale-replacement only — never composes with a project-tier hit.
        match tokio::fs::metadata(&candidate).await {
            Ok(meta) if meta.file_type().is_file() => Some(candidate),
            Ok(_) => {
                // Path exists but isn't a regular file (directory, device,
                // FIFO). Skip rather than treat the home-tier as available.
                log::warn!(
                    "skipping home-tier project candidate {} (not a regular file)",
                    candidate.display()
                );
                None
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => None,
            Err(source) => {
                // Permission denied, EIO, stale NFS handle, etc. We cannot
                // load the file, but discovery shouldn't abort the whole
                // process. Mirror discover_paths behavior: warn so it's at
                // least diagnosable, then skip.
                log::warn!(
                    "skipping unreadable home-tier project candidate {}: {source}",
                    candidate.display()
                );
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::*;

    // ── Helper ───────────────────────────────────────────────────────────────

    fn write_config(dir: &TempDir, filename: &str, content: &str) -> PathBuf {
        let path = dir.path().join(filename);
        std::fs::write(&path, content).expect("write test config");
        path
    }

    // ── load_and_merge tests (Step 3.3) ──────────────────────────────────────

    #[tokio::test]
    async fn load_and_merge_empty_list_returns_default() {
        // Plan: Step 3.3 — empty list → Config::default()
        let result = ConfigLoader::load_and_merge::<PathBuf>(&[]).await;
        let config = result.expect("empty merge should succeed");
        assert!(config.registry.is_none());
    }

    #[tokio::test]
    async fn load_and_merge_single_file() {
        // Plan: Step 3.3 — single file is loaded and parsed
        let dir = TempDir::new().unwrap();
        let path = write_config(&dir, "single.toml", "[registry]\ndefault = \"single.example\"");
        let config = ConfigLoader::load_and_merge(&[path])
            .await
            .expect("single file merge should succeed");
        assert_eq!(
            config.registry.as_ref().and_then(|r| r.default.as_deref()),
            Some("single.example")
        );
    }

    #[tokio::test]
    async fn load_and_merge_two_files_second_wins_on_conflict() {
        // Plan: Step 3.3 — two files both setting [registry] default → second wins
        let dir = TempDir::new().unwrap();
        let first = write_config(&dir, "first.toml", "[registry]\ndefault = \"first.example\"");
        let second = write_config(&dir, "second.toml", "[registry]\ndefault = \"second.example\"");
        let config = ConfigLoader::load_and_merge(&[first, second])
            .await
            .expect("two-file merge should succeed");
        assert_eq!(
            config.registry.as_ref().and_then(|r| r.default.as_deref()),
            Some("second.example"),
            "second (higher precedence) should win"
        );
    }

    #[tokio::test]
    async fn load_and_merge_two_files_only_first_sets_default() {
        // Plan: Step 3.3 — two files, only first sets [registry] default → preserved
        let dir = TempDir::new().unwrap();
        let first = write_config(&dir, "first.toml", "[registry]\ndefault = \"first.example\"");
        let second = write_config(&dir, "second.toml", "");
        let config = ConfigLoader::load_and_merge(&[first, second])
            .await
            .expect("merge should succeed");
        assert_eq!(
            config.registry.as_ref().and_then(|r| r.default.as_deref()),
            Some("first.example"),
            "first file's value should be preserved when second doesn't override"
        );
    }

    #[tokio::test]
    async fn load_and_merge_missing_file_returns_error() {
        // Plan: Step 3.3 — missing file in list → error (discovery should have filtered)
        let nonexistent = PathBuf::from("/tmp/this-file-does-not-exist-ocx-test-12345.toml");
        let result = ConfigLoader::load_and_merge(&[nonexistent]).await;
        assert!(result.is_err(), "missing file in load_and_merge should be an error");
    }

    #[tokio::test]
    async fn load_and_merge_invalid_toml_returns_parse_error() {
        // Plan: Step 3.3 — file with invalid TOML → Parse error with file path
        let dir = TempDir::new().unwrap();
        let path = write_config(&dir, "broken.toml", "this is not valid toml =[[[");
        let result = ConfigLoader::load_and_merge(std::slice::from_ref(&path)).await;
        assert!(result.is_err(), "invalid TOML should produce an error");
        let err = result.unwrap_err();
        let err_str = err.to_string();
        assert!(
            err_str.contains("broken.toml"),
            "error message should contain the file path, got: {err_str}"
        );
    }

    #[tokio::test]
    async fn load_and_merge_file_too_large_returns_error() {
        // A file exceeding MAX_CONFIG_SIZE returns FileTooLarge. The bounded
        // read inside load_and_merge also defends against files whose
        // `metadata.len()` is 0 but whose read is unbounded — use a file with
        // real bytes here; the synthetic case is infeasible to simulate in a
        // portable test.
        let dir = TempDir::new().unwrap();
        let content = "x".repeat(MAX_CONFIG_SIZE as usize + 1);
        let path = write_config(&dir, "huge.toml", &content);
        let result = ConfigLoader::load_and_merge(std::slice::from_ref(&path)).await;
        let err = result.expect_err("oversized file should be rejected");
        let err_str = err.to_string();
        assert!(
            err_str.contains("exceeds maximum allowed size"),
            "error should mention size cap, got: {err_str}"
        );
        assert!(
            err_str.contains("huge.toml"),
            "error should contain the file path, got: {err_str}"
        );
    }

    #[tokio::test]
    async fn load_and_merge_rejects_non_regular_file() {
        // Pointing load_and_merge at a directory triggers the is_file() guard.
        let dir = TempDir::new().unwrap();
        let dir_path = dir.path().to_path_buf();
        let result = ConfigLoader::load_and_merge(&[dir_path]).await;
        let err = result.expect_err("directory path should be rejected");
        // Plan Test 3.1.3: assert the exact substring injected at loader.rs:148.
        // The string "config path is not a regular file" lives in the inner
        // std::io::Error source, so walk the full source chain manually via
        // `std::error::Error::source()` and concatenate each Display.
        use std::error::Error as _;
        let mut err_str = format!("{err}");
        let mut cause: Option<&dyn std::error::Error> = err.source();
        while let Some(c) = cause {
            err_str.push_str(": ");
            err_str.push_str(&c.to_string());
            cause = c.source();
        }
        assert!(
            err_str.contains("config path is not a regular file"),
            "error should contain exact substring 'config path is not a regular file', got: {err_str}"
        );
    }

    #[tokio::test]
    async fn load_and_merge_three_files_precedence_order() {
        // Three tiers each setting `[registry] default`: highest wins, the
        // lower two values are fully replaced.
        let dir = TempDir::new().unwrap();
        let low = write_config(&dir, "low.toml", "[registry]\ndefault = \"low.example\"");
        let mid = write_config(&dir, "mid.toml", "[registry]\ndefault = \"mid.example\"");
        let high = write_config(&dir, "high.toml", "[registry]\ndefault = \"high.example\"");
        let config = ConfigLoader::load_and_merge(&[low, mid, high])
            .await
            .expect("three-file merge should succeed");
        assert_eq!(
            config.registry.as_ref().and_then(|r| r.default.as_deref()),
            Some("high.example"),
            "highest-precedence tier (third file) should win"
        );
    }

    #[tokio::test]
    async fn load_and_merge_registries_merge_across_tiers() {
        // Two tiers with overlapping [registries.shared] entries + one unique
        // entry each. Higher tier wins on the shared key; both unique entries
        // survive.
        let dir = TempDir::new().unwrap();
        let low = write_config(
            &dir,
            "low.toml",
            "[registries.shared]\nurl = \"old.example\"\n\n[registries.only_low]\nurl = \"low.example\"",
        );
        let high = write_config(
            &dir,
            "high.toml",
            "[registries.shared]\nurl = \"new.example\"\n\n[registries.only_high]\nurl = \"high.example\"",
        );
        let config = ConfigLoader::load_and_merge(&[low, high])
            .await
            .expect("two-file registries merge should succeed");
        let registries = config.registries.expect("registries should be present");
        assert_eq!(registries.len(), 3);
        assert_eq!(
            registries["shared"].url.as_deref(),
            Some("new.example"),
            "higher tier should win on conflicting key"
        );
        assert_eq!(registries["only_low"].url.as_deref(), Some("low.example"));
        assert_eq!(registries["only_high"].url.as_deref(), Some("high.example"));
    }

    // ── load() orchestration tests ───────────────────────────────────────────
    //
    // Env-touching tests acquire `crate::test::env::lock()` — a process-wide
    // mutex whose Drop clears all overrides. Overrides route through
    // `crate::env::var`'s `#[cfg(test)]` branch; no `std::env::set_var`, no
    // `unsafe`.

    #[tokio::test]
    async fn load_with_no_config_returns_default() {
        let env = crate::test::env::lock();
        env.set("OCX_NO_CONFIG", "1");
        env.remove("OCX_CONFIG");
        let inputs = ConfigInputs {
            explicit_path: None,
            explicit_project_path: None,
            cwd: None,
        };
        let config = ConfigLoader::load(inputs)
            .await
            .expect("OCX_NO_CONFIG=1 should succeed");
        assert!(
            config.registry.is_none(),
            "OCX_NO_CONFIG=1 with no explicit path should return default config"
        );
    }

    #[tokio::test]
    async fn load_with_no_config_and_explicit_path_loads_only_explicit() {
        // OCX_NO_CONFIG=1 with --config → explicit file still loads.
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        let path = write_config(&dir, "hermetic.toml", "[registry]\ndefault = \"hermetic.example\"");
        env.set("OCX_NO_CONFIG", "1");
        env.remove("OCX_CONFIG");
        let inputs = ConfigInputs {
            explicit_path: Some(&path),
            explicit_project_path: None,
            cwd: None,
        };
        let config = ConfigLoader::load(inputs)
            .await
            .expect("OCX_NO_CONFIG=1 with --config should load the explicit file");
        assert_eq!(
            config.registry.as_ref().and_then(|r| r.default.as_deref()),
            Some("hermetic.example"),
            "the explicit file must load even when OCX_NO_CONFIG=1"
        );
    }

    #[tokio::test]
    async fn load_with_no_config_and_env_path_still_loads_env_path() {
        // OCX_NO_CONFIG=1 with OCX_CONFIG → env-var path still loads.
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        let path = write_config(
            &dir,
            "env-hermetic.toml",
            "[registry]\ndefault = \"env-hermetic.example\"",
        );
        env.set("OCX_NO_CONFIG", "1");
        env.set("OCX_CONFIG", path.to_str().unwrap());
        let inputs = ConfigInputs {
            explicit_path: None,
            explicit_project_path: None,
            cwd: None,
        };
        let config = ConfigLoader::load(inputs)
            .await
            .expect("OCX_NO_CONFIG=1 with OCX_CONFIG should load the env file");
        assert_eq!(
            config.registry.as_ref().and_then(|r| r.default.as_deref()),
            Some("env-hermetic.example"),
            "the env-var path must load even when OCX_NO_CONFIG=1"
        );
    }

    #[tokio::test]
    async fn load_with_empty_ocx_config_file_treats_as_unset() {
        // OCX_CONFIG="" is the escape hatch — treated as unset, not an error.
        let env = crate::test::env::lock();
        env.set("OCX_NO_CONFIG", "1");
        env.set("OCX_CONFIG", "");
        let inputs = ConfigInputs {
            explicit_path: None,
            explicit_project_path: None,
            cwd: None,
        };
        let config = ConfigLoader::load(inputs)
            .await
            .expect("empty OCX_CONFIG should be treated as unset");
        assert!(config.registry.is_none(), "empty OCX_CONFIG must not load anything");
    }

    #[tokio::test]
    async fn load_with_nonexistent_explicit_path_errors() {
        let env = crate::test::env::lock();
        env.set("OCX_NO_CONFIG", "1");
        env.remove("OCX_CONFIG");
        let nonexistent = PathBuf::from("/tmp/ocx-test-nonexistent-config-99999.toml");
        let inputs = ConfigInputs {
            explicit_path: Some(&nonexistent),
            explicit_project_path: None,
            cwd: None,
        };
        let result = ConfigLoader::load(inputs).await;
        assert!(result.is_err(), "explicit path to missing file should error");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("ocx-test-nonexistent-config-99999.toml"),
            "error should contain the path, got: {err}"
        );
    }

    #[tokio::test]
    async fn load_with_ocx_config_file_env_loads_that_file() {
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        let path = write_config(&dir, "ci.toml", "[registry]\ndefault = \"ci.example\"");
        env.set("OCX_NO_CONFIG", "1");
        env.set("OCX_CONFIG", path.to_str().unwrap());
        let inputs = ConfigInputs {
            explicit_path: None,
            explicit_project_path: None,
            cwd: None,
        };
        let config = ConfigLoader::load(inputs).await.expect("OCX_CONFIG should succeed");
        assert_eq!(
            config.registry.as_ref().and_then(|r| r.default.as_deref()),
            Some("ci.example")
        );
    }

    #[tokio::test]
    async fn load_with_explicit_path_layers_on_top_of_env_path() {
        // Both OCX_CONFIG and --config set → both load; --config (highest
        // file-tier precedence) wins on conflicting scalars.
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        let env_file = write_config(&dir, "env.toml", "[registry]\ndefault = \"env.example\"");
        let explicit_file = write_config(&dir, "explicit.toml", "[registry]\ndefault = \"explicit.example\"");
        env.set("OCX_NO_CONFIG", "1");
        env.set("OCX_CONFIG", env_file.to_str().unwrap());
        let inputs = ConfigInputs {
            explicit_path: Some(&explicit_file),
            explicit_project_path: None,
            cwd: None,
        };
        let config = ConfigLoader::load(inputs)
            .await
            .expect("both explicit sources should load");
        assert_eq!(
            config.registry.as_ref().and_then(|r| r.default.as_deref()),
            Some("explicit.example"),
            "--config should layer on top of OCX_CONFIG and win on conflict"
        );
    }

    // ── Path resolver tests ──────────────────────────────────────────────────

    #[cfg(unix)]
    #[test]
    fn system_path_is_etc_ocx_config_toml() {
        let path = ConfigLoader::system_path();
        assert_eq!(path, PathBuf::from("/etc/ocx/config.toml"));
    }

    #[test]
    fn user_path_ends_with_ocx_config_toml() {
        if dirs::config_dir().is_none() {
            // Cannot test without a config dir — skip (don't panic)
            return;
        }
        let path = ConfigLoader::user_path();
        assert!(
            path.is_some(),
            "user_path() should return Some when config_dir() is available"
        );
        let path = path.unwrap();
        assert!(
            path.ends_with("ocx/config.toml"),
            "user_path should end with ocx/config.toml, got: {}",
            path.display()
        );
    }

    #[test]
    fn home_path_uses_ocx_home_env_var() {
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        env.set("OCX_HOME", dir.path().to_str().unwrap());
        let path = ConfigLoader::home_path();
        assert!(path.is_some(), "home_path() should return Some when OCX_HOME is set");
        let expected = dir.path().join("config.toml");
        assert_eq!(path.unwrap(), expected);
    }

    // ── discover_paths error handling ────────────────────────────────────────

    #[cfg(unix)]
    #[tokio::test]
    async fn discover_paths_skips_unreadable_candidate() {
        // A candidate whose parent directory lacks search permission causes
        // `try_exists` to return `Err(PermissionDenied)`. The new filter_map
        // branch must log a warning and drop the candidate rather than either
        // including it or failing the whole discovery pass.
        use std::os::unix::fs::PermissionsExt;

        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        let locked_home = dir.path().join("locked-home");
        std::fs::create_dir(&locked_home).unwrap();
        let locked_config = locked_home.join("config.toml");
        std::fs::write(&locked_config, "").unwrap();
        // Mode 0o000 strips search permission; stat() on the child fails with
        // PermissionDenied, which is the error kind discover_paths should now
        // log+skip rather than silently collapse.
        std::fs::set_permissions(&locked_home, std::fs::Permissions::from_mode(0o000)).unwrap();

        env.set("OCX_HOME", locked_home.to_str().unwrap());
        env.remove("OCX_CONFIG");
        env.remove("OCX_NO_CONFIG");

        let paths = ConfigLoader::discover_paths().await;

        // Restore permissions so TempDir::drop can clean up even if the
        // assertion below panics.
        std::fs::set_permissions(&locked_home, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(
            !paths.contains(&locked_config),
            "unreadable candidate must be skipped, got: {paths:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn discover_paths_rejects_symlinked_candidate() {
        // Security: a discovered-tier `config.toml` that is a symlink is
        // rejected to prevent a writer with control over one of the
        // tier directories from aiming the link at an arbitrary readable
        // file. Explicit paths (--config, OCX_CONFIG) are out of scope
        // for this check — those are trusted caller input.
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("ocx-home");
        std::fs::create_dir(&home).unwrap();
        // Create a real target file, then symlink `config.toml` → target.
        let target = dir.path().join("target.toml");
        std::fs::write(&target, "[registry]\ndefault = \"symlinked.example\"").unwrap();
        let symlink_path = home.join("config.toml");
        std::os::unix::fs::symlink(&target, &symlink_path).expect("create symlink");

        env.set("OCX_HOME", home.to_str().unwrap());
        env.remove("OCX_CONFIG");
        env.remove("OCX_NO_CONFIG");

        let paths = ConfigLoader::discover_paths().await;

        assert!(
            !paths.contains(&symlink_path),
            "symlinked candidate must be skipped, got: {paths:?}"
        );
    }

    #[test]
    fn home_path_fallback_when_ocx_home_unset() {
        // With OCX_HOME removed, home_path() falls back to `dirs::home_dir()`.
        // The result is platform-dependent: Some(path ending in .ocx/config.toml)
        // when HOME is set, None otherwise. Both are valid outcomes.
        let env = crate::test::env::lock();
        env.remove("OCX_HOME");
        let path = ConfigLoader::home_path();
        if let Some(path) = path {
            assert!(
                path.ends_with(".ocx/config.toml"),
                "fallback path should end with .ocx/config.toml, got: {}",
                path.display()
            );
        }
    }

    // ── project_path tests ──────────────────────────────────────────────────
    //
    // Plan Phase 1 (plan_project_toolchain.md, lines 77–93) defines the
    // resolver contract:
    //
    //   Precedence: --project > OCX_PROJECT > CWD walk
    //   OCX_NO_PROJECT=1 prunes CWD walk + env var, NOT the explicit flag
    //   OCX_PROJECT="" treated as unset (escape hatch)
    //   Explicit paths follow symlinks; CWD-walk paths reject symlinks
    //   Any basename accepted via flag/env; CWD walk looks for literal `ocx.toml`
    //   CWD walk stops at first ocx.toml, .git/ boundary, or OCX_CEILING_PATH
    //   CWD walk does NOT stop at filesystem root if .git/ is absent
    //   Explicit path escapes OCX_CEILING_PATH
    //   --project <missing> / OCX_PROJECT=<missing> → NotFound (79)
    //
    // Every test below names one plan bullet or one `max`-tier edge case. In
    // Phase 1 (stub), every test fails with the `unimplemented!()` panic on
    // `project_path`; Phase 4 impl flips them to pass.
    //
    // All env-touching tests acquire `crate::test::env::lock()` — the same
    // process-wide mutex used by the `load()` tests above.

    /// Helper: write a file at `path` with the given content.
    fn write_file(path: &std::path::Path, content: &str) {
        std::fs::write(path, content).expect("write test fixture");
    }

    /// Plan bullet: `--project <valid>` → loads the file.
    #[tokio::test]
    async fn project_path_explicit_flag_loads_valid_file() {
        let env = crate::test::env::lock();
        env.remove("OCX_PROJECT");
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_CEILING_PATH");
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("ocx.toml");
        write_file(&path, "");
        let resolved = ConfigLoader::project_path(None, Some(&path))
            .await
            .expect("valid explicit path should resolve");
        assert_eq!(resolved, Some(path));
    }

    /// Plan bullet: `--project <missing>` → `NotFound` (79).
    #[tokio::test]
    async fn project_path_explicit_flag_missing_returns_not_found() {
        let env = crate::test::env::lock();
        env.remove("OCX_PROJECT");
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_CEILING_PATH");
        let missing = PathBuf::from("/tmp/ocx-project-path-test-missing-explicit.toml");
        let err = ConfigLoader::project_path(None, Some(&missing))
            .await
            .expect_err("missing explicit path should be FileNotFound");
        assert!(
            matches!(
                err,
                crate::config::error::Error::FileNotFound {
                    ref path,
                    tier: crate::config::error::ConfigSource::Project,
                } if path == &missing,
            ),
            "expected FileNotFound(Project) for missing --project path, got: {err:?}"
        );
    }

    /// Non-`NotFound` I/O error on an explicit path surfaces as `Error::Io`
    /// (exit 74) rather than silently succeeding. `/dev/null/...` returns
    /// `ENOTDIR` on Unix because `/dev/null` is a character device, not a
    /// directory — a stable way to provoke a non-`NotFound` kind without
    /// platform-specific permission gymnastics.
    #[cfg(unix)]
    #[tokio::test]
    async fn project_path_explicit_io_error_surfaces_as_io() {
        let env = crate::test::env::lock();
        env.remove("OCX_PROJECT");
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_CEILING_PATH");
        let bad = PathBuf::from("/dev/null/not-a-real-file.toml");
        let err = ConfigLoader::project_path(None, Some(&bad))
            .await
            .expect_err("non-NotFound I/O error on explicit path must surface");
        assert!(
            matches!(
                err,
                crate::config::error::Error::Io {
                    ref path,
                    tier: crate::config::error::ConfigSource::Project,
                    ..
                } if path == &bad,
            ),
            "expected Error::Io(Project) for ENOTDIR on explicit --project path, got: {err:?}"
        );
    }

    /// Plan bullet: `OCX_PROJECT=<valid>` → loads the file.
    #[tokio::test]
    async fn project_path_env_var_loads_valid_file() {
        let env = crate::test::env::lock();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_CEILING_PATH");
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("custom-name.toml");
        write_file(&path, "");
        env.set("OCX_PROJECT", path.to_str().unwrap());
        let resolved = ConfigLoader::project_path(None, None)
            .await
            .expect("env-var path should resolve");
        assert_eq!(resolved, Some(path));
    }

    /// Plan bullet: `OCX_PROJECT=""` → treated as unset.
    ///
    /// With env var treated as unset, and no explicit flag, and a cwd that
    /// has no `ocx.toml` above it up to the ceiling → returns `None`.
    #[tokio::test]
    async fn project_path_empty_env_var_treated_as_unset() {
        let env = crate::test::env::lock();
        env.set("OCX_PROJECT", "");
        env.remove("OCX_NO_PROJECT");
        let dir = TempDir::new().unwrap();
        env.set("OCX_CEILING_PATH", dir.path().to_str().unwrap());
        let resolved = ConfigLoader::project_path(Some(dir.path()), None)
            .await
            .expect("empty env var should be treated as unset, not an error");
        assert_eq!(
            resolved, None,
            "empty OCX_PROJECT must fall through; with no cwd hit, result should be None"
        );
    }

    /// Plan bullet: `OCX_NO_PROJECT=1` → skips CWD walk + env-var path; returns `None`.
    #[tokio::test]
    async fn project_path_no_project_returns_none() {
        let env = crate::test::env::lock();
        env.set("OCX_NO_PROJECT", "1");
        let dir = TempDir::new().unwrap();
        // Even placing a valid ocx.toml at cwd must not be discovered.
        let cwd_project = dir.path().join("ocx.toml");
        write_file(&cwd_project, "");
        // And a valid env-var path must also be ignored.
        let env_path = dir.path().join("env.toml");
        write_file(&env_path, "");
        env.set("OCX_PROJECT", env_path.to_str().unwrap());
        let resolved = ConfigLoader::project_path(Some(dir.path()), None)
            .await
            .expect("OCX_NO_PROJECT=1 with no explicit flag must return Ok(None)");
        assert_eq!(
            resolved, None,
            "OCX_NO_PROJECT=1 must prune both CWD walk and env-var path"
        );
    }

    /// Plan bullet: `OCX_NO_PROJECT=1` + `--project <valid>` → still loads.
    #[tokio::test]
    async fn project_path_no_project_does_not_block_explicit_flag() {
        let env = crate::test::env::lock();
        env.set("OCX_NO_PROJECT", "1");
        env.remove("OCX_PROJECT");
        env.remove("OCX_CEILING_PATH");
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("flag.toml");
        write_file(&path, "");
        let resolved = ConfigLoader::project_path(None, Some(&path))
            .await
            .expect("OCX_NO_PROJECT=1 must not block --project");
        assert_eq!(resolved, Some(path));
    }

    /// ADR G3: `OCX_NO_PROJECT=1` prunes `OCX_PROJECT` (stricter than
    /// `OCX_NO_CONFIG`, which leaves `OCX_CONFIG` intact). Only `--project`
    /// escapes the kill switch — the env var does not.
    #[tokio::test]
    async fn project_path_no_project_prunes_env_var() {
        let env = crate::test::env::lock();
        env.set("OCX_NO_PROJECT", "1");
        env.remove("OCX_CEILING_PATH");
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("env-hermetic.toml");
        write_file(&path, "");
        env.set("OCX_PROJECT", path.to_str().unwrap());
        let resolved = ConfigLoader::project_path(None, None)
            .await
            .expect("OCX_NO_PROJECT=1 must prune the env-var path per ADR G3");
        assert_eq!(resolved, None, "OCX_NO_PROJECT=1 must prune OCX_PROJECT (ADR G3)");
    }

    /// Plan bullet: Precedence `--project` > `OCX_PROJECT` > CWD walk.
    ///
    /// Three files are materialized, one per tier. A single resolver call
    /// that sees all three must return the highest-precedence one.
    #[tokio::test]
    async fn project_path_flag_beats_env_beats_walk_precedence() {
        let env = crate::test::env::lock();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_CEILING_PATH");
        let dir = TempDir::new().unwrap();

        // CWD-walk candidate at the root of a throw-away workspace.
        let walk_dir = dir.path().join("workspace");
        std::fs::create_dir(&walk_dir).unwrap();
        let walk_path = walk_dir.join("ocx.toml");
        write_file(&walk_path, "");

        // Env-var candidate.
        let env_path = dir.path().join("env.toml");
        write_file(&env_path, "");
        env.set("OCX_PROJECT", env_path.to_str().unwrap());

        // Explicit-flag candidate (highest).
        let flag_path = dir.path().join("flag.toml");
        write_file(&flag_path, "");

        let resolved = ConfigLoader::project_path(Some(&walk_dir), Some(&flag_path))
            .await
            .expect("all three tiers present should resolve");
        assert_eq!(
            resolved,
            Some(flag_path),
            "--project must beat OCX_PROJECT and CWD walk"
        );
    }

    /// Max-tier edge case: `--project` takes precedence over `OCX_PROJECT`
    /// when both are set (focused assertion separate from the three-tier test).
    #[tokio::test]
    async fn project_path_explicit_takes_precedence_over_env_when_both_set() {
        let env = crate::test::env::lock();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_CEILING_PATH");
        let dir = TempDir::new().unwrap();
        let file_a = dir.path().join("a.toml");
        let file_b = dir.path().join("b.toml");
        write_file(&file_a, "");
        write_file(&file_b, "");
        env.set("OCX_PROJECT", file_b.to_str().unwrap());
        let resolved = ConfigLoader::project_path(None, Some(&file_a))
            .await
            .expect("both explicit sources should resolve");
        assert_eq!(resolved, Some(file_a), "--project must beat OCX_PROJECT");
    }

    /// Plan bullet: Explicit path escapes `OCX_CEILING_PATH`.
    ///
    /// Ceiling is set above the target file; the explicit path must still
    /// resolve because the ceiling only bounds the CWD walk.
    #[cfg(unix)]
    #[tokio::test]
    async fn project_path_explicit_escapes_ceiling() {
        let env = crate::test::env::lock();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_PROJECT");
        let dir = TempDir::new().unwrap();
        let ceiling = dir.path().join("ceiling");
        std::fs::create_dir(&ceiling).unwrap();
        env.set("OCX_CEILING_PATH", ceiling.to_str().unwrap());
        // File lives OUTSIDE the ceiling — must still resolve via --project.
        let outside = dir.path().join("outside.toml");
        write_file(&outside, "");
        let resolved = ConfigLoader::project_path(None, Some(&outside))
            .await
            .expect("explicit path must escape ceiling");
        assert_eq!(resolved, Some(outside));
    }

    /// Plan bullet: Symlink via `--project` → accepted.
    ///
    /// Explicit paths follow symlinks — trusted caller intent.
    #[cfg(unix)]
    #[tokio::test]
    async fn project_path_explicit_follows_symlink() {
        let env = crate::test::env::lock();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_PROJECT");
        env.remove("OCX_CEILING_PATH");
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("real.toml");
        write_file(&target, "");
        let link = dir.path().join("link.toml");
        std::os::unix::fs::symlink(&target, &link).expect("create symlink");
        let resolved = ConfigLoader::project_path(None, Some(&link))
            .await
            .expect("symlink via --project must be accepted");
        // Either the link path itself or the resolved target is acceptable —
        // the spec says "accepted" without pinning which is returned. Assert
        // that we got a Some and it points at a real file.
        let returned = resolved.expect("should be Some");
        assert!(
            returned == link || returned == target,
            "returned path should be the link or its target, got: {}",
            returned.display()
        );
    }

    /// Plan bullet: Symlink discovered via CWD walk → rejected.
    ///
    /// CWD walk rejects symlinks (matches tier-config discovery symmetry).
    #[cfg(unix)]
    #[tokio::test]
    async fn project_path_walk_rejects_symlink() {
        let env = crate::test::env::lock();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_PROJECT");
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        // .git/ absent, ceiling bounds the walk to the temp dir.
        env.set("OCX_CEILING_PATH", dir.path().to_str().unwrap());

        // Put the real ocx.toml outside the walk, then symlink into workspace.
        let target = dir.path().join("real.toml");
        write_file(&target, "");
        let link = workspace.join("ocx.toml");
        std::os::unix::fs::symlink(&target, &link).expect("create symlink");

        let resolved = ConfigLoader::project_path(Some(&workspace), None)
            .await
            .expect("symlinked walk hit should be skipped, not error");
        assert_eq!(
            resolved, None,
            "CWD walk must reject symlinks and return None when only the symlink is found"
        );
    }

    /// Plan bullet: Non-`ocx.toml` basename via `--project` → accepted.
    ///
    /// Matches Cargo `--manifest-path` semantics.
    #[tokio::test]
    async fn project_path_explicit_accepts_any_basename() {
        let env = crate::test::env::lock();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_PROJECT");
        env.remove("OCX_CEILING_PATH");
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("fixture-manifest.project");
        write_file(&path, "");
        let resolved = ConfigLoader::project_path(None, Some(&path))
            .await
            .expect("any basename should be accepted via --project");
        assert_eq!(resolved, Some(path));
    }

    /// Plan bullet: CWD walk with `ocx.toml` at repo root + nested cwd → finds root file.
    #[cfg(unix)]
    #[tokio::test]
    async fn project_path_walk_finds_root_ocx_toml_from_nested_cwd() {
        let env = crate::test::env::lock();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_PROJECT");
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        let nested = root.join("src").join("deep");
        std::fs::create_dir_all(&nested).unwrap();
        let project = root.join("ocx.toml");
        write_file(&project, "");
        env.set("OCX_CEILING_PATH", dir.path().to_str().unwrap());

        let resolved = ConfigLoader::project_path(Some(&nested), None)
            .await
            .expect("walk from nested cwd should succeed");
        assert_eq!(resolved, Some(project), "walk should locate root ocx.toml");
    }

    /// Plan bullet: CWD walk stops at `.git/` boundary.
    ///
    /// A parent `ocx.toml` exists above a `.git/` directory; the walk must
    /// stop at the `.git/` boundary and NOT cross into the parent.
    #[cfg(unix)]
    #[tokio::test]
    async fn project_path_walk_stops_at_git_boundary() {
        let env = crate::test::env::lock();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_PROJECT");
        let dir = TempDir::new().unwrap();
        // Parent workspace holds an ocx.toml — this must NOT be returned.
        let parent_project = dir.path().join("ocx.toml");
        write_file(&parent_project, "");
        // Inner workspace with a .git/ boundary and a nested cwd.
        let inner = dir.path().join("inner");
        std::fs::create_dir(&inner).unwrap();
        std::fs::create_dir(inner.join(".git")).unwrap();
        let nested = inner.join("src");
        std::fs::create_dir(&nested).unwrap();
        env.set("OCX_CEILING_PATH", dir.path().to_str().unwrap());

        let resolved = ConfigLoader::project_path(Some(&nested), None)
            .await
            .expect("walk must stop at .git/ boundary");
        assert_eq!(
            resolved, None,
            ".git/ boundary must prevent discovery of parent ocx.toml"
        );
    }

    /// Git worktree `.git` is a *file* (linkfile) pointing at the real git
    /// directory under `worktrees/<name>/`, not a directory. The walk must
    /// still treat the worktree root as a repository boundary — matching
    /// git's own `git-check-ref-format` rule of "any `.git` entry counts".
    #[cfg(unix)]
    #[tokio::test]
    async fn project_path_walk_stops_at_git_worktree_linkfile() {
        let env = crate::test::env::lock();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_PROJECT");
        let dir = TempDir::new().unwrap();
        // Parent workspace holds an ocx.toml — this must NOT be returned.
        let parent_project = dir.path().join("ocx.toml");
        write_file(&parent_project, "");
        // Worktree layout: `.git` is a regular file, not a directory.
        let worktree = dir.path().join("worktree");
        std::fs::create_dir(&worktree).unwrap();
        std::fs::write(worktree.join(".git"), "gitdir: /some/path/.git/worktrees/wt\n").unwrap();
        let nested = worktree.join("src");
        std::fs::create_dir(&nested).unwrap();
        env.set("OCX_CEILING_PATH", dir.path().to_str().unwrap());

        let resolved = ConfigLoader::project_path(Some(&nested), None)
            .await
            .expect("walk must stop at .git linkfile boundary");
        assert_eq!(
            resolved, None,
            ".git file (worktree linkfile) must also act as a repo boundary"
        );
    }

    /// Amendment F precedence: `ocx.toml` at the repo root (alongside
    /// `.git/`) must be returned — the `.git/` boundary only prevents
    /// walking UP past the repo root, it does not disqualify a project
    /// file AT that level. Regression guard for the common case.
    #[cfg(unix)]
    #[tokio::test]
    async fn project_path_walk_finds_ocx_toml_at_git_root_level() {
        let env = crate::test::env::lock();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_PROJECT");
        let dir = TempDir::new().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        std::fs::create_dir(repo.join(".git")).unwrap();
        let project = repo.join("ocx.toml");
        write_file(&project, "");
        let nested = repo.join("src").join("deep");
        std::fs::create_dir_all(&nested).unwrap();
        env.set("OCX_CEILING_PATH", dir.path().to_str().unwrap());

        let resolved = ConfigLoader::project_path(Some(&nested), None)
            .await
            .expect("walk from nested cwd must find ocx.toml at repo root");
        assert_eq!(
            resolved,
            Some(project),
            "a repo root with both .git/ and ocx.toml must resolve to the ocx.toml"
        );
    }

    /// Explicit `--project <dir>` must not silently succeed — non-file
    /// targets surface as `Error::Io` (exit 74, ADR G9) rather than being
    /// accepted as a valid project-file path.
    #[cfg(unix)]
    #[tokio::test]
    async fn project_path_explicit_directory_rejected_as_io() {
        let env = crate::test::env::lock();
        env.remove("OCX_PROJECT");
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_CEILING_PATH");
        let dir = TempDir::new().unwrap();
        let target = dir.path().to_path_buf();
        let err = ConfigLoader::project_path(None, Some(&target))
            .await
            .expect_err("explicit --project pointing at a directory must error");
        assert!(
            matches!(
                err,
                crate::config::error::Error::Io {
                    ref path,
                    tier: crate::config::error::ConfigSource::Project,
                    ..
                } if path == &target,
            ),
            "expected Error::Io(Project) for directory on explicit --project path, got: {err:?}"
        );
    }

    /// Plan bullet: No `ocx.toml`, no explicit path → returns `None`.
    #[tokio::test]
    async fn project_path_returns_none_when_no_source() {
        let env = crate::test::env::lock();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_PROJECT");
        let dir = TempDir::new().unwrap();
        env.set("OCX_CEILING_PATH", dir.path().to_str().unwrap());
        let resolved = ConfigLoader::project_path(Some(dir.path()), None)
            .await
            .expect("no sources should resolve to None, not error");
        assert_eq!(resolved, None);
    }

    /// Max-tier edge case: `OCX_PROJECT=<missing>` → `NotFound`.
    ///
    /// Plan line 69 + Amendment G7 — symmetry with `--project <missing>`.
    /// Plan bullets only test the valid env-var path explicitly.
    #[tokio::test]
    async fn project_path_env_var_missing_file_returns_not_found() {
        let env = crate::test::env::lock();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_CEILING_PATH");
        let missing = PathBuf::from("/tmp/ocx-project-path-test-missing-env.toml");
        env.set("OCX_PROJECT", missing.to_str().unwrap());
        let err = ConfigLoader::project_path(None, None)
            .await
            .expect_err("missing env-var path should be FileNotFound");
        assert!(
            matches!(
                err,
                crate::config::error::Error::FileNotFound {
                    ref path,
                    tier: crate::config::error::ConfigSource::Project,
                } if path == &missing,
            ),
            "expected FileNotFound(Project) for missing OCX_PROJECT path, got: {err:?}"
        );
    }

    /// Max-tier edge case: CWD walk with no `.git/`, no `OCX_CEILING_PATH`, no
    /// `ocx.toml` → returns `None` without hanging or erroring.
    ///
    /// Plan line 40 ("Does NOT stop at filesystem root if .git/ is absent")
    /// requires the walk to continue past the common stopping conditions;
    /// this test guards against an infinite loop.
    #[tokio::test]
    async fn project_path_walk_without_git_or_ceiling_returns_none() {
        let env = crate::test::env::lock();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_PROJECT");
        env.remove("OCX_CEILING_PATH");
        // Use a temp dir far inside /tmp; no ocx.toml anywhere on the path
        // to `/`. The resolver must terminate at the filesystem root and
        // return None — never hang, never error.
        let dir = TempDir::new().unwrap();
        let nested = dir.path().join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        let resolved = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            ConfigLoader::project_path(Some(&nested), None),
        )
        .await
        .expect("walk must terminate within 5s when no .git/ and no ceiling")
        .expect("walk should resolve to None when nothing is found, not error");
        assert_eq!(resolved, None);
    }

    /// Max-tier edge case: `OCX_CEILING_PATH` set above the `ocx.toml` → returns `None`.
    ///
    /// Amendment F — ceiling is the paired bound for the walk. When the
    /// ceiling sits between cwd and the `ocx.toml`, discovery must stop.
    #[cfg(unix)]
    #[tokio::test]
    async fn project_path_walk_stops_at_ceiling() {
        let env = crate::test::env::lock();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_PROJECT");
        let dir = TempDir::new().unwrap();
        // ocx.toml at the OUTERMOST level (above the ceiling).
        let outer_project = dir.path().join("ocx.toml");
        write_file(&outer_project, "");
        // Ceiling sits inside the tempdir; walk starts below the ceiling.
        let ceiling = dir.path().join("ceiling");
        std::fs::create_dir(&ceiling).unwrap();
        env.set("OCX_CEILING_PATH", ceiling.to_str().unwrap());
        let cwd = ceiling.join("project");
        std::fs::create_dir(&cwd).unwrap();

        let resolved = ConfigLoader::project_path(Some(&cwd), None)
            .await
            .expect("ceiling-bounded walk should resolve, not error");
        assert_eq!(
            resolved, None,
            "OCX_CEILING_PATH must bound the walk before reaching outer ocx.toml"
        );
    }

    /// Max-tier edge case: no ceiling, no `.git/`, deeply nested cwd, `ocx.toml`
    /// many levels up → found.
    ///
    /// Stresses the loop-termination logic: we expect the walk to actually
    /// traverse several levels rather than short-circuiting at one or two.
    #[cfg(unix)]
    #[tokio::test]
    async fn project_path_walk_no_ceiling_no_git_finds_ocx_toml_many_levels_up() {
        let env = crate::test::env::lock();
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_PROJECT");
        let dir = TempDir::new().unwrap();
        // Set the ceiling at the tempdir so the test cannot accidentally walk
        // past it into the real filesystem; the `ocx.toml` is placed just
        // below the ceiling so the walk has work to do without leaving the
        // sandbox. This still stresses five levels of parent traversal, which
        // is the point of the test.
        env.set("OCX_CEILING_PATH", dir.path().to_str().unwrap());
        let root = dir.path().join("r");
        std::fs::create_dir(&root).unwrap();
        let project = root.join("ocx.toml");
        write_file(&project, "");
        let deep = root.join("a").join("b").join("c").join("d").join("e");
        std::fs::create_dir_all(&deep).unwrap();

        let resolved = ConfigLoader::project_path(Some(&deep), None)
            .await
            .expect("deep walk should resolve");
        assert_eq!(
            resolved,
            Some(project),
            "walk must traverse multiple parent levels to find ocx.toml"
        );
    }

    // ── Phase 9: home-tier $OCX_HOME/ocx.toml fallback ──────────────────────
    //
    // Plan plan_project_toolchain.md Phase 9 (lines 830–855). Activates only
    // when the explicit flag, OCX_PROJECT_FILE, and the CWD walk all return
    // None and OCX_NO_PROJECT is unset. Amendment C: the home tier is a
    // wholesale replacement — never composed when a project-tier hit exists.
    //
    // Bodies depend on the `home_project_path` helper that today calls
    // `unimplemented!()`; every test below should panic at the stub until
    // Phase 5 fills the body in.

    /// Plan bullet (Phase 9): empty CWD + `$OCX_HOME/ocx.toml` exists →
    /// returns the home file. Use a sibling tmp dir as cwd so the walk
    /// cannot accidentally rediscover the home file from above.
    #[tokio::test]
    async fn project_path_home_tier_fallback_when_walk_returns_none() {
        let env = crate::test::env::lock();
        env.remove("OCX_PROJECT_FILE");
        env.remove("OCX_NO_PROJECT");
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let home_toml = home.join("ocx.toml");
        write_file(&home_toml, "");
        // Sibling dir for cwd so the upward walk never sees `home`.
        let cwd = dir.path().join("sibling");
        std::fs::create_dir(&cwd).unwrap();
        env.set("OCX_HOME", home.to_str().unwrap());
        // Bound the walk so it terminates inside the tempdir without hitting
        // the real filesystem.
        env.set("OCX_CEILING_PATH", cwd.to_str().unwrap());

        let resolved = ConfigLoader::project_path(Some(&cwd), None)
            .await
            .expect("home-tier fallback should resolve to Some");
        assert_eq!(
            resolved,
            Some(home_toml),
            "empty walk + $OCX_HOME/ocx.toml present must resolve to the home file"
        );
    }

    /// Plan bullet (Phase 9, Amendment C): project ocx.toml at cwd AND home
    /// ocx.toml both exist → walk wins; home tier is wholesale replacement,
    /// never composed.
    #[tokio::test]
    async fn project_path_home_tier_skipped_when_walk_finds_project() {
        let env = crate::test::env::lock();
        env.remove("OCX_PROJECT_FILE");
        env.remove("OCX_NO_PROJECT");
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let home_toml = home.join("ocx.toml");
        write_file(&home_toml, "");
        let cwd = dir.path().join("project");
        std::fs::create_dir(&cwd).unwrap();
        let project_toml = cwd.join("ocx.toml");
        write_file(&project_toml, "");
        env.set("OCX_HOME", home.to_str().unwrap());
        env.set("OCX_CEILING_PATH", cwd.to_str().unwrap());

        let resolved = ConfigLoader::project_path(Some(&cwd), None)
            .await
            .expect("walk + home both present should resolve");
        assert_eq!(
            resolved,
            Some(project_toml),
            "Amendment C: project file at cwd must beat $OCX_HOME/ocx.toml"
        );
    }

    /// Plan bullet (Phase 9): `OCX_NO_PROJECT=1` prunes the home tier too —
    /// the kill switch is total, not just walk + env-var.
    #[tokio::test]
    async fn project_path_home_tier_skipped_when_no_project_set() {
        let env = crate::test::env::lock();
        env.remove("OCX_PROJECT_FILE");
        env.set("OCX_NO_PROJECT", "1");
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let home_toml = home.join("ocx.toml");
        write_file(&home_toml, "");
        let cwd = dir.path().join("sibling");
        std::fs::create_dir(&cwd).unwrap();
        env.set("OCX_HOME", home.to_str().unwrap());
        env.set("OCX_CEILING_PATH", cwd.to_str().unwrap());

        let resolved = ConfigLoader::project_path(Some(&cwd), None)
            .await
            .expect("OCX_NO_PROJECT=1 with home file must succeed with None");
        assert_eq!(
            resolved, None,
            "OCX_NO_PROJECT=1 must prune the home tier as well as walk + env"
        );
    }

    /// Plan bullet (Phase 9): explicit `--project <path>` always beats the
    /// home tier — explicit wins regardless of the home file.
    #[tokio::test]
    async fn project_path_home_tier_skipped_when_explicit_flag_used() {
        let env = crate::test::env::lock();
        env.remove("OCX_PROJECT_FILE");
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_CEILING_PATH");
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let home_toml = home.join("ocx.toml");
        write_file(&home_toml, "");
        let explicit = dir.path().join("explicit.toml");
        write_file(&explicit, "");
        env.set("OCX_HOME", home.to_str().unwrap());

        let resolved = ConfigLoader::project_path(None, Some(&explicit))
            .await
            .expect("explicit + home should resolve to explicit");
        assert_eq!(
            resolved,
            Some(explicit),
            "--project must always beat the home-tier fallback"
        );
    }

    /// Plan bullet (Phase 9): `OCX_HOME` directory present but no `ocx.toml`
    /// inside → returns `Ok(None)`, not an error.
    #[tokio::test]
    async fn project_path_home_tier_returns_none_when_home_file_absent() {
        let env = crate::test::env::lock();
        env.remove("OCX_PROJECT_FILE");
        env.remove("OCX_NO_PROJECT");
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir(&home).unwrap();
        // Note: no ocx.toml inside home/.
        let cwd = dir.path().join("sibling");
        std::fs::create_dir(&cwd).unwrap();
        env.set("OCX_HOME", home.to_str().unwrap());
        env.set("OCX_CEILING_PATH", cwd.to_str().unwrap());

        let resolved = ConfigLoader::project_path(Some(&cwd), None)
            .await
            .expect("absent home ocx.toml should resolve to None, not error");
        assert_eq!(resolved, None, "$OCX_HOME without an ocx.toml must produce Ok(None)");
    }

    /// Plan bullet (Phase 9): home-tier symlinks are followed (matches the
    /// `home_path()` policy for `$OCX_HOME/config.toml` — explicit-trust
    /// location). The CWD-walk symlink-rejection rule does NOT apply here.
    #[cfg(unix)]
    #[tokio::test]
    async fn project_path_home_tier_follows_symlink() {
        let env = crate::test::env::lock();
        env.remove("OCX_PROJECT_FILE");
        env.remove("OCX_NO_PROJECT");
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let target = dir.path().join("real.toml");
        write_file(&target, "");
        let link = home.join("ocx.toml");
        std::os::unix::fs::symlink(&target, &link).expect("create symlink");
        let cwd = dir.path().join("sibling");
        std::fs::create_dir(&cwd).unwrap();
        env.set("OCX_HOME", home.to_str().unwrap());
        env.set("OCX_CEILING_PATH", cwd.to_str().unwrap());

        let resolved = ConfigLoader::project_path(Some(&cwd), None)
            .await
            .expect("home-tier symlink must resolve, not error");
        let returned = resolved.expect("home symlink must produce Some");
        assert!(
            returned == link || returned == target,
            "home-tier symlink should resolve to link or target, got: {}",
            returned.display()
        );
    }

    /// Codex follow-up: a permission-denied home-tier file must skip cleanly
    /// (warn-and-skip), mirroring `discover_paths_skips_unreadable_candidate`.
    /// Previously all `tokio::fs::metadata` failures collapsed to `None`,
    /// making EIO / EACCES on `$OCX_HOME/ocx.toml` non-diagnosable.
    #[cfg(unix)]
    #[tokio::test]
    async fn project_path_home_tier_skips_unreadable_with_warning() {
        use std::os::unix::fs::PermissionsExt;
        let env = crate::test::env::lock();
        env.remove("OCX_PROJECT_FILE");
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_CEILING_PATH");
        let dir = TempDir::new().unwrap();
        let locked_home = dir.path().join("locked-home");
        std::fs::create_dir(&locked_home).unwrap();
        let candidate = locked_home.join("ocx.toml");
        std::fs::write(&candidate, "").unwrap();
        // Strip search permission on parent dir → metadata() returns
        // PermissionDenied. The home-tier fallback must skip cleanly.
        std::fs::set_permissions(&locked_home, std::fs::Permissions::from_mode(0o000)).unwrap();
        env.set("OCX_HOME", locked_home.to_str().unwrap());
        let cwd_dir = TempDir::new().unwrap();
        let result = ConfigLoader::project_path(Some(cwd_dir.path()), None).await;
        // Restore permissions so TempDir cleanup works.
        std::fs::set_permissions(&locked_home, std::fs::Permissions::from_mode(0o755)).unwrap();
        let resolved = result.expect("home-tier failure should not abort discovery");
        assert!(
            resolved.is_none(),
            "unreadable home-tier candidate must be skipped, got: {resolved:?}"
        );
    }

    /// Plan bullet (Phase 9): `OCX_HOME` unset → fall back to
    /// `dirs::home_dir()/.ocx/ocx.toml` (mirrors the `home_path()` policy).
    /// The behavior is platform-dependent; without a real file at
    /// `~/.ocx/ocx.toml` we assert the resolver returns `None` cleanly
    /// rather than panicking or erroring.
    #[tokio::test]
    async fn project_path_home_tier_when_ocx_home_unset_uses_home_dir_fallback() {
        let env = crate::test::env::lock();
        env.remove("OCX_PROJECT_FILE");
        env.remove("OCX_NO_PROJECT");
        env.remove("OCX_HOME");
        // Bound the walk inside a sandbox so we never accidentally hit a
        // real `ocx.toml` somewhere on disk while exercising this branch.
        let dir = TempDir::new().unwrap();
        let cwd = dir.path().join("sibling");
        std::fs::create_dir(&cwd).unwrap();
        env.set("OCX_CEILING_PATH", cwd.to_str().unwrap());

        // The result depends on whether `~/.ocx/ocx.toml` happens to exist
        // on the host. Both `Ok(None)` and `Ok(Some(_))` are acceptable —
        // what we want to lock in is that the resolver tolerates an unset
        // `OCX_HOME` without panicking or returning an error.
        let resolved = ConfigLoader::project_path(Some(&cwd), None)
            .await
            .expect("unset OCX_HOME must not produce an error");
        if let Some(path) = resolved {
            assert!(
                path.ends_with(".ocx/ocx.toml"),
                "fallback path should end with .ocx/ocx.toml, got: {}",
                path.display()
            );
        }
    }
}
