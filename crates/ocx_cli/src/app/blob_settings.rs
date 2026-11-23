// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Where this invocation looks for blobs and where it keeps the ones it fetches.
//!
//! The winning source replaces the others whole: `--blob-dir` flags, else `OCX_BLOB_DIR`, else the
//! merged `[blobs] dirs`. The cache is flag, then `OCX_BLOB_CACHE_DIR`, then `[blobs] cache_dir`.

use std::path::{Path, PathBuf};

use ocx_config::Config;
use ocx_config::exec_state::PolicyLayer;
use ocx_config::exec_state::policy::check_blob_dir_count;
// The split `OCX_BLOB_DIR` goes through lives beside the policy slot that carries it.
#[cfg(test)]
use ocx_config::exec_state::policy::split_blob_dirs as split_list;

use super::ContextOptions;

/// The most blob directories one invocation reads.
pub const MAX_BLOB_DIRS: usize = ocx_config::exec_state::policy::MAX_BLOB_DIRS;

/// Which source named a blob directory; it decides whether a missing one is an error or a warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingOrigin {
    /// `--blob-dir`: a missing directory refuses the command.
    Flag,
    /// `OCX_BLOB_DIR`: a missing directory warns once on first use.
    Env,
    /// `[blobs] dirs`: a missing directory warns once on first use.
    Config,
}

/// One blob directory and the source that named it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobDir {
    pub path: PathBuf,
    pub origin: SettingOrigin,
}

/// The resolved blob directories (read-only) and blob cache (writable).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlobSettings {
    pub dirs: Vec<BlobDir>,
    pub cache_dir: Option<PathBuf>,
}

/// Why the blob settings cannot be used.
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
#[non_exhaustive]
pub enum BlobSettingsError {
    /// A `--blob-dir` flag named a path that is missing or not a directory.
    #[error("blob directory '{}' does not exist or is not a directory (--blob-dir)", path.display())]
    #[exit(
        NotFound,
        slug = "blob_dir_not_found",
        summary = "A --blob-dir path does not exist or is not a directory"
    )]
    BlobDirNotFound { path: PathBuf },

    /// A `[blobs]` path is relative where only an absolute one is accepted.
    #[error(transparent)]
    #[exit(delegate)]
    Config(#[from] ocx_config::error::Error),
}

/// Resolves the settings for this invocation.
///
/// `env` is the environment slot, the policy layer holding the value each seam reads where it reads
/// the variable; `cwd` anchors a relative flag or environment path.
///
/// # Errors
///
/// [`BlobSettingsError::BlobDirNotFound`] when a flag-given directory is missing,
/// [`BlobSettingsError::Config`] when the winning source lists more than [`MAX_BLOB_DIRS`] entries, and
/// [`BlobSettingsError::Config`] when a config path reaches here relative: the loader anchors or
/// refuses every one, so the join onto the working directory would be a guess.
pub fn resolve(
    flags: &ContextOptions,
    env: &PolicyLayer,
    config: &Config,
    cwd: &Path,
) -> Result<BlobSettings, BlobSettingsError> {
    let blobs = config.blobs.as_ref();
    let dirs = if !flags.blob_dir.is_empty() {
        check_blob_dir_count("--blob-dir", flags.blob_dir.len())?;
        let mut dirs = Vec::new();
        for path in &flags.blob_dir {
            let path = cwd.join(path);
            if !path.is_dir() {
                return Err(BlobSettingsError::BlobDirNotFound { path });
            }
            dirs.push(BlobDir {
                path,
                origin: SettingOrigin::Flag,
            });
        }
        dirs
    } else if let Some(list) = &env.policy.blob_dir {
        check_blob_dir_count("OCX_BLOB_DIR", list.len())?;
        list.iter()
            .map(|path| BlobDir {
                path: cwd.join(path),
                origin: SettingOrigin::Env,
            })
            .collect()
    } else if let Some(list) = blobs.and_then(|blobs| blobs.dirs.as_ref()) {
        check_blob_dir_count("[blobs] dirs", list.len())?;
        let mut dirs = Vec::new();
        for path in list {
            dirs.push(BlobDir {
                path: absolute_config_path(path)?,
                origin: SettingOrigin::Config,
            });
        }
        dirs
    } else {
        Vec::new()
    };

    let cache_dir = if let Some(path) = &flags.blob_cache_dir {
        Some(cwd.join(path))
    } else if let Some(path) = &env.policy.blob_cache_dir {
        Some(cwd.join(path))
    } else if let Some(path) = blobs.and_then(|blobs| blobs.cache_dir.as_ref()) {
        Some(absolute_config_path(path)?)
    } else {
        None
    };
    Ok(BlobSettings { dirs, cache_dir })
}

fn absolute_config_path(path: &Path) -> Result<PathBuf, BlobSettingsError> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Err(ocx_config::error::Error::BlobPathRelative {
            path: path.to_path_buf(),
        }
        .into())
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use ocx_config::blobs::BlobsConfig;
    use ocx_config::exec_state::Policy;

    use super::*;

    fn options(args: &[&str]) -> ContextOptions {
        ContextOptions::try_parse_from(std::iter::once("ocx").chain(args.iter().copied())).expect("flags parse")
    }

    fn env_with(dirs: Vec<PathBuf>) -> PolicyLayer {
        PolicyLayer {
            policy: Policy {
                blob_dir: Some(dirs),
                ..Policy::default()
            },
            ..PolicyLayer::default()
        }
    }

    fn config_with(dirs: Vec<PathBuf>) -> Config {
        Config {
            blobs: Some(BlobsConfig {
                dirs: Some(dirs),
                cache_dir: None,
            }),
            ..Config::default()
        }
    }

    fn existing(name: &str) -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().expect("temp dir");
        let dir = root.path().join(name);
        std::fs::create_dir(&dir).expect("create dir");
        (root, dir)
    }

    #[test]
    fn flag_replaces_env() {
        let (_a, flag_dir) = existing("flag");
        let (_b, env_dir) = existing("env");
        let flags = options(&["--blob-dir", flag_dir.to_str().expect("utf-8 path")]);
        let settings = resolve(&flags, &env_with(vec![env_dir]), &Config::default(), Path::new("/")).expect("resolves");
        assert_eq!(
            settings.dirs,
            vec![BlobDir {
                path: flag_dir,
                origin: SettingOrigin::Flag
            }]
        );
    }

    #[test]
    fn flag_value_wins_over_env_value() {
        let (_a, flag_dir) = existing("flag");
        let flags = options(&["--blob-dir", flag_dir.to_str().expect("utf-8 path")]);
        let env = env_with(
            (0..=MAX_BLOB_DIRS)
                .map(|i| PathBuf::from(format!("/env/{i}")))
                .collect(),
        );
        let settings = resolve(&flags, &env, &Config::default(), Path::new("/")).expect("flag wins, env never read");
        assert_eq!(settings.dirs.len(), 1);
        assert_eq!(settings.dirs[0].origin, SettingOrigin::Flag);
    }

    #[test]
    fn more_than_the_cap_is_refused_from_every_source() {
        let over = |tag: &str| -> Vec<PathBuf> {
            (0..=MAX_BLOB_DIRS)
                .map(|i| PathBuf::from(format!("/{tag}/{i}")))
                .collect()
        };
        let flag_args: Vec<String> = over("flag")
            .iter()
            .flat_map(|p| ["--blob-dir".to_string(), p.display().to_string()])
            .collect();
        let flag_args: Vec<&str> = flag_args.iter().map(String::as_str).collect();
        let results = [
            resolve(
                &options(&flag_args),
                &PolicyLayer::default(),
                &Config::default(),
                Path::new("/"),
            ),
            resolve(
                &options(&[]),
                &env_with(over("env")),
                &Config::default(),
                Path::new("/"),
            ),
            resolve(
                &options(&[]),
                &PolicyLayer::default(),
                &config_with(over("cfg")),
                Path::new("/"),
            ),
        ];
        for result in results {
            let err = result.expect_err("one entry past the cap is refused, not dropped");
            assert!(
                matches!(
                    err,
                    BlobSettingsError::Config(ocx_config::error::Error::ConfigListTooLong { .. })
                ),
                "got: {err:?}"
            );
        }
    }

    #[test]
    fn exactly_the_cap_is_kept() {
        let env = env_with((0..MAX_BLOB_DIRS).map(|i| PathBuf::from(format!("/env/{i}"))).collect());
        let settings = resolve(&options(&[]), &env, &Config::default(), Path::new("/")).expect("at the cap resolves");
        assert_eq!(settings.dirs.len(), MAX_BLOB_DIRS);
    }

    #[test]
    fn config_anchored() {
        let (_a, config_dir) = existing("cfg");
        let settings = resolve(
            &options(&[]),
            &PolicyLayer::default(),
            &config_with(vec![config_dir.clone()]),
            Path::new("/"),
        )
        .expect("resolves");
        assert_eq!(
            settings.dirs,
            vec![BlobDir {
                path: config_dir,
                origin: SettingOrigin::Config
            }]
        );
    }

    #[test]
    fn missing_flag_dir_is_79() {
        let (_a, parent) = existing("parent");
        let missing = parent.join("absent");
        let flags = options(&["--blob-dir", missing.to_str().expect("utf-8 path")]);
        let err = resolve(&flags, &PolicyLayer::default(), &Config::default(), Path::new("/"))
            .expect_err("a missing --blob-dir is refused");
        assert!(matches!(err, BlobSettingsError::BlobDirNotFound { .. }), "got: {err:?}");
    }

    #[test]
    fn missing_env_dir_passes() {
        let (_a, parent) = existing("parent");
        let missing = parent.join("absent");
        let settings = resolve(
            &options(&[]),
            &env_with(vec![missing.clone()]),
            &Config::default(),
            Path::new("/"),
        )
        .expect("a missing env directory passes resolve");
        assert_eq!(
            settings.dirs,
            vec![BlobDir {
                path: missing,
                origin: SettingOrigin::Env
            }]
        );
    }

    /// The loader refuses a relative `[managed]` path (exit 78) and anchors every other tier's, so a
    /// relative entry that still reaches `resolve` is refused rather than joined onto the working directory.
    #[test]
    fn managed_relative_refused() {
        let err = resolve(
            &options(&[]),
            &PolicyLayer::default(),
            &config_with(vec![PathBuf::from("relative/blobs")]),
            Path::new("/"),
        );
        assert!(err.is_err(), "a relative config path must not resolve");
    }

    #[test]
    fn split_list_skips_empty_entries() {
        let joined = std::env::join_paths([PathBuf::from("a"), PathBuf::new(), PathBuf::from("b")])
            .expect("paths without the separator");
        assert_eq!(split_list(&joined), vec![PathBuf::from("a"), PathBuf::from("b")]);
    }

    #[cfg(windows)]
    #[test]
    fn windows_quoted_entry() {
        let raw = std::ffi::OsStr::new(r#""C:\blobs;one";D:\two"#);
        assert_eq!(
            split_list(raw),
            vec![PathBuf::from(r"C:\blobs;one"), PathBuf::from(r"D:\two")]
        );
    }
}
