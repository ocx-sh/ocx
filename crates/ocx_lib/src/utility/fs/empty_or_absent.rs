// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Validate that a path is absent or an empty directory.
//!
//! Used by destination-path preconditions: `ocx package test --output DIR`
//! requires `DIR` to be absent (it will be created) or empty (it will be
//! reused).

use std::path::{Path, PathBuf};

use crate::cli::{ClassifyExitCode, ExitCode};

/// Failure modes of [`ensure_empty_or_absent`].
#[derive(Debug)]
pub enum EmptyOrAbsentError {
    /// `path` exists and is not a directory.
    NotADirectory { path: PathBuf },
    /// `path` exists, is a directory, and contains entries.
    NonEmpty { path: PathBuf },
    /// I/O failure during the existence/metadata/listing probe.
    Io { path: PathBuf, source: std::io::Error },
}

impl std::fmt::Display for EmptyOrAbsentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotADirectory { path } => {
                write!(f, "path '{}' exists and is not a directory", path.display())
            }
            Self::NonEmpty { path } => write!(
                f,
                "directory '{}' is not empty; remove its contents or choose a different path",
                path.display(),
            ),
            Self::Io { path, source } => {
                write!(f, "I/O error checking directory '{}': {source}", path.display())
            }
        }
    }
}

impl std::error::Error for EmptyOrAbsentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl ClassifyExitCode for EmptyOrAbsentError {
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            Self::NotADirectory { .. } | Self::NonEmpty { .. } => ExitCode::UsageError,
            Self::Io { .. } => ExitCode::IoError,
        })
    }
}

/// Verify that `path` is either absent or an empty directory.
///
/// Returns `Ok(())` on success. Errors with structured variants distinguish
/// "exists as file", "non-empty directory", and "I/O failure".
pub async fn ensure_empty_or_absent(path: &Path) -> Result<(), EmptyOrAbsentError> {
    let exists = tokio::fs::try_exists(path).await.map_err(|e| EmptyOrAbsentError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    if !exists {
        return Ok(());
    }
    let meta = tokio::fs::metadata(path).await.map_err(|e| EmptyOrAbsentError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    if !meta.is_dir() {
        return Err(EmptyOrAbsentError::NotADirectory {
            path: path.to_path_buf(),
        });
    }
    let mut entries = tokio::fs::read_dir(path).await.map_err(|e| EmptyOrAbsentError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    let next = entries.next_entry().await.map_err(|e| EmptyOrAbsentError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    if next.is_some() {
        return Err(EmptyOrAbsentError::NonEmpty {
            path: path.to_path_buf(),
        });
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
        let target = td.path().join("does-not-exist");
        ensure_empty_or_absent(&target).await.unwrap();
    }

    #[tokio::test]
    async fn empty_dir_passes() {
        let td = TempDir::new().unwrap();
        ensure_empty_or_absent(td.path()).await.unwrap();
    }

    #[tokio::test]
    async fn non_empty_dir_rejected() {
        let td = TempDir::new().unwrap();
        tokio::fs::write(td.path().join("file"), b"x").await.unwrap();
        match ensure_empty_or_absent(td.path()).await.unwrap_err() {
            EmptyOrAbsentError::NonEmpty { .. } => {}
            other => panic!("expected NonEmpty, got {other}"),
        }
    }

    #[tokio::test]
    async fn file_path_rejected() {
        let td = TempDir::new().unwrap();
        let f = td.path().join("file");
        tokio::fs::write(&f, b"x").await.unwrap();
        match ensure_empty_or_absent(&f).await.unwrap_err() {
            EmptyOrAbsentError::NotADirectory { .. } => {}
            other => panic!("expected NotADirectory, got {other}"),
        }
    }
}
