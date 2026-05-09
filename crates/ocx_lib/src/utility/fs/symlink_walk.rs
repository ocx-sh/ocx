// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Refuse a path whose ancestor chain contains any symlink.
//!
//! Used by destination-path validation to guard against symlink-traversal
//! attacks where an adversary places a symlink along the path to redirect
//! writes to attacker-controlled locations.

use std::path::{Path, PathBuf};

use crate::cli::{ClassifyExitCode, ExitCode};

/// Failure mode of [`refuse_if_symlink_in_path`].
#[derive(Debug)]
pub enum SymlinkWalkError {
    /// `ancestor` (an existing component of `path`) is a symlink.
    Ancestor { path: PathBuf, ancestor: PathBuf },
    /// I/O failure while walking the ancestor chain.
    Io { path: PathBuf, source: std::io::Error },
}

impl std::fmt::Display for SymlinkWalkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ancestor { path, ancestor } => write!(
                f,
                "destination '{}' must not resolve through a symlink: '{}' is a symlink",
                path.display(),
                ancestor.display(),
            ),
            Self::Io { path, source } => {
                write!(f, "I/O error checking path component '{}': {source}", path.display())
            }
        }
    }
}

impl std::error::Error for SymlinkWalkError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Ancestor { .. } => None,
            Self::Io { source, .. } => Some(source),
        }
    }
}

impl ClassifyExitCode for SymlinkWalkError {
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            Self::Ancestor { .. } => ExitCode::UsageError,
            Self::Io { .. } => ExitCode::IoError,
        })
    }
}

/// Refuses when `path` itself or any existing ancestor is a symlink.
///
/// Walks ancestors from `path` upward (most specific first) and calls
/// [`tokio::fs::symlink_metadata`] on each that exists. Missing ancestors
/// are tolerated — only existing symlinks fail the check.
///
/// # Security note
///
/// TOCTOU residual: an ancestor swap between this check and the subsequent
/// directory create can still redirect writes. Single-user use cases are
/// unaffected; CI automation should validate the exact path passed in,
/// not derive it from untrusted input.
pub async fn refuse_if_symlink_in_path(path: &Path) -> Result<(), SymlinkWalkError> {
    let mut current: Option<&Path> = Some(path);
    while let Some(p) = current {
        match tokio::fs::symlink_metadata(p).await {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(SymlinkWalkError::Ancestor {
                    path: path.to_path_buf(),
                    ancestor: p.to_path_buf(),
                });
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Not yet created — keep walking.
            }
            Err(e) => {
                return Err(SymlinkWalkError::Io {
                    path: p.to_path_buf(),
                    source: e,
                });
            }
        }
        current = p.parent();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn absent_path_passes() {
        let td = TempDir::new().unwrap();
        let target = td.path().join("does/not/exist");
        refuse_if_symlink_in_path(&target).await.unwrap();
    }

    #[tokio::test]
    async fn regular_dir_passes() {
        let td = TempDir::new().unwrap();
        refuse_if_symlink_in_path(td.path()).await.unwrap();
    }

    #[tokio::test]
    async fn regular_file_passes() {
        let td = TempDir::new().unwrap();
        let f = td.path().join("file");
        tokio::fs::write(&f, b"x").await.unwrap();
        refuse_if_symlink_in_path(&f).await.unwrap();
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn leaf_symlink_rejected() {
        let td = TempDir::new().unwrap();
        let real = td.path().join("real");
        tokio::fs::create_dir(&real).await.unwrap();
        let link = td.path().join("link");
        tokio::fs::symlink(&real, &link).await.unwrap();
        match refuse_if_symlink_in_path(&link).await.unwrap_err() {
            SymlinkWalkError::Ancestor { ancestor, .. } => assert_eq!(ancestor, link),
            other => panic!("expected Ancestor, got {other}"),
        }
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn intermediate_symlink_rejected() {
        let td = TempDir::new().unwrap();
        let real = td.path().join("real");
        tokio::fs::create_dir(&real).await.unwrap();
        let link = td.path().join("link");
        tokio::fs::symlink(&real, &link).await.unwrap();
        let target = link.join("nested");
        match refuse_if_symlink_in_path(&target).await.unwrap_err() {
            SymlinkWalkError::Ancestor { ancestor, .. } => assert_eq!(ancestor, link),
            other => panic!("expected Ancestor, got {other}"),
        }
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn broken_symlink_rejected() {
        let td = TempDir::new().unwrap();
        let link = td.path().join("dangling");
        tokio::fs::symlink(td.path().join("missing"), &link).await.unwrap();
        match refuse_if_symlink_in_path(&link).await.unwrap_err() {
            SymlinkWalkError::Ancestor { .. } => {}
            other => panic!("expected Ancestor, got {other}"),
        }
    }
}
