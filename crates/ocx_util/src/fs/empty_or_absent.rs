// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Validate that a path is absent or an empty directory.

use std::path::{Path, PathBuf};

/// Failure modes of [`ensure_empty_or_absent`].
#[derive(Debug, ocx_exit::Classify)]
pub enum EmptyOrAbsentError {
    #[exit(
        UsageError,
        slug = "destination_not_a_directory",
        summary = "The destination exists and is not a directory"
    )]
    NotADirectory { path: PathBuf },
    #[exit(
        UsageError,
        slug = "destination_not_empty",
        summary = "The destination directory is not empty"
    )]
    NonEmpty { path: PathBuf },
    #[exit(
        IoError,
        slug = "destination_check_io",
        summary = "Inspecting the destination directory failed"
    )]
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

/// Verify that `path` is either absent or an empty directory.
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
