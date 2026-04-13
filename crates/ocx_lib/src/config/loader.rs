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
use crate::config::{Config, error::Error};
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
    /// CWD for the future project-tier walk (#33). Pass `None` in v1.
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
    /// 2. `OCX_CONFIG_FILE` — if set and non-empty
    /// 3. `--config FILE` (via [`ConfigInputs::explicit_path`])
    ///
    /// Explicit paths (both env-var and CLI) always load if set; they layer
    /// on top of the discovered chain (or on top of nothing, if
    /// `OCX_NO_CONFIG=1` pruned it). Empty `OCX_CONFIG_FILE=""` is treated
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
        let raw_env_config_file = crate::env::var("OCX_CONFIG_FILE");
        if raw_env_config_file.as_deref() == Some("") {
            log::debug!("OCX_CONFIG_FILE is set to empty string — skipped via escape hatch");
        }
        let env_config_file = raw_env_config_file.filter(|s| !s.is_empty());

        let mut paths: Vec<PathBuf> = if no_config {
            Vec::new()
        } else {
            Self::discover_paths(&inputs).await
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
    /// v1 returns: `[system_path, user_path, home_path]` filtering out
    /// `None` and nonexistent files. Future #33 will append the project
    /// path (CWD walk result) before any explicit override.
    ///
    /// Async because it calls [`tokio::fs::try_exists`] per candidate path.
    /// `NotFound` candidates are silently skipped. Other I/O errors
    /// (permission denied, stale NFS handle, EIO) are logged as warnings
    /// and the candidate is still skipped — discovery never fails the
    /// whole process, but an unreadable `~/.ocx/config.toml` should at
    /// least be *diagnosable*. A race between discovery and read surfaces
    /// later as [`Error::Io`] during [`Self::load_and_merge`].
    pub async fn discover_paths(_inputs: &ConfigInputs<'_>) -> Vec<PathBuf> {
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
        // the try_exists calls concurrently shaves two sequential filesystem
        // round-trips on startup.
        let checks = join_all(candidates.iter().map(tokio::fs::try_exists)).await;
        candidates
            .into_iter()
            .zip(checks)
            .filter_map(|(path, result)| match result {
                Ok(true) => Some(path),
                Ok(false) => None,
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => None,
                Err(source) => {
                    log::warn!("skipping unreadable config candidate {}: {source}", path.display());
                    None
                }
            })
            .collect()
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
                    }
                    .into());
                }
                Err(source) => {
                    return Err(Error::Io {
                        path: path.to_path_buf(),
                        source,
                    }
                    .into());
                }
            };
            let metadata = file.metadata().await.map_err(|source| Error::Io {
                path: path.to_path_buf(),
                source,
            })?;
            if !metadata.file_type().is_file() {
                return Err(Error::Io {
                    path: path.to_path_buf(),
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

    /// `$OCX_HOME/config.toml`, falling back to `~/.ocx/config.toml`.
    pub fn home_path() -> Option<PathBuf> {
        if let Some(ocx_home) = crate::env::var("OCX_HOME") {
            return Some(PathBuf::from(ocx_home).join("config.toml"));
        }
        dirs::home_dir().map(|h| h.join(".ocx").join("config.toml"))
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
        env.remove("OCX_CONFIG_FILE");
        let inputs = ConfigInputs {
            explicit_path: None,
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
        env.remove("OCX_CONFIG_FILE");
        let inputs = ConfigInputs {
            explicit_path: Some(&path),
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
        // OCX_NO_CONFIG=1 with OCX_CONFIG_FILE → env-var path still loads.
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        let path = write_config(
            &dir,
            "env-hermetic.toml",
            "[registry]\ndefault = \"env-hermetic.example\"",
        );
        env.set("OCX_NO_CONFIG", "1");
        env.set("OCX_CONFIG_FILE", path.to_str().unwrap());
        let inputs = ConfigInputs {
            explicit_path: None,
            cwd: None,
        };
        let config = ConfigLoader::load(inputs)
            .await
            .expect("OCX_NO_CONFIG=1 with OCX_CONFIG_FILE should load the env file");
        assert_eq!(
            config.registry.as_ref().and_then(|r| r.default.as_deref()),
            Some("env-hermetic.example"),
            "the env-var path must load even when OCX_NO_CONFIG=1"
        );
    }

    #[tokio::test]
    async fn load_with_empty_ocx_config_file_treats_as_unset() {
        // OCX_CONFIG_FILE="" is the escape hatch — treated as unset, not an error.
        let env = crate::test::env::lock();
        env.set("OCX_NO_CONFIG", "1");
        env.set("OCX_CONFIG_FILE", "");
        let inputs = ConfigInputs {
            explicit_path: None,
            cwd: None,
        };
        let config = ConfigLoader::load(inputs)
            .await
            .expect("empty OCX_CONFIG_FILE should be treated as unset");
        assert!(
            config.registry.is_none(),
            "empty OCX_CONFIG_FILE must not load anything"
        );
    }

    #[tokio::test]
    async fn load_with_nonexistent_explicit_path_errors() {
        let env = crate::test::env::lock();
        env.set("OCX_NO_CONFIG", "1");
        env.remove("OCX_CONFIG_FILE");
        let nonexistent = PathBuf::from("/tmp/ocx-test-nonexistent-config-99999.toml");
        let inputs = ConfigInputs {
            explicit_path: Some(&nonexistent),
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
        env.set("OCX_CONFIG_FILE", path.to_str().unwrap());
        let inputs = ConfigInputs {
            explicit_path: None,
            cwd: None,
        };
        let config = ConfigLoader::load(inputs)
            .await
            .expect("OCX_CONFIG_FILE should succeed");
        assert_eq!(
            config.registry.as_ref().and_then(|r| r.default.as_deref()),
            Some("ci.example")
        );
    }

    #[tokio::test]
    async fn load_with_explicit_path_layers_on_top_of_env_path() {
        // Both OCX_CONFIG_FILE and --config set → both load; --config (highest
        // file-tier precedence) wins on conflicting scalars.
        let env = crate::test::env::lock();
        let dir = TempDir::new().unwrap();
        let env_file = write_config(&dir, "env.toml", "[registry]\ndefault = \"env.example\"");
        let explicit_file = write_config(&dir, "explicit.toml", "[registry]\ndefault = \"explicit.example\"");
        env.set("OCX_NO_CONFIG", "1");
        env.set("OCX_CONFIG_FILE", env_file.to_str().unwrap());
        let inputs = ConfigInputs {
            explicit_path: Some(&explicit_file),
            cwd: None,
        };
        let config = ConfigLoader::load(inputs)
            .await
            .expect("both explicit sources should load");
        assert_eq!(
            config.registry.as_ref().and_then(|r| r.default.as_deref()),
            Some("explicit.example"),
            "--config should layer on top of OCX_CONFIG_FILE and win on conflict"
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
        env.remove("OCX_CONFIG_FILE");
        env.remove("OCX_NO_CONFIG");

        let inputs = ConfigInputs {
            explicit_path: None,
            cwd: None,
        };
        let paths = ConfigLoader::discover_paths(&inputs).await;

        // Restore permissions so TempDir::drop can clean up even if the
        // assertion below panics.
        std::fs::set_permissions(&locked_home, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(
            !paths.contains(&locked_config),
            "unreadable candidate must be skipped, got: {paths:?}"
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
}
