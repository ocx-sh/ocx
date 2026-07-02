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

/// Refuses when `path` itself or any existing ancestor below `boundary` is a
/// symlink.
///
/// Walks ancestors from `path` upward (most specific first) and calls
/// [`tokio::fs::symlink_metadata`] on each that exists. Missing ancestors
/// are tolerated — only existing symlinks fail the check.
///
/// `boundary` marks a **trusted** root: the walk stops when it reaches that
/// path, so `boundary` and everything above it are never checked. Pass
/// `Some(dest_content)` when only the untrusted portion strictly *below* a
/// package's content root should be validated — OCX's own store path (e.g. a
/// `~/.ocx` that is itself a symlink onto a larger disk) is trusted and must not
/// fail-close every prefix-using install. Pass `None` to walk the whole
/// ancestor chain up to the filesystem root (for a fully untrusted path such as
/// a user-supplied `--output` directory).
///
/// # Security note
///
/// TOCTOU residual: an ancestor swap between this check and the subsequent
/// directory create can still redirect writes. Single-user use cases are
/// unaffected; CI automation should validate the exact path passed in,
/// not derive it from untrusted input.
pub async fn refuse_if_symlink_in_path(path: &Path, boundary: Option<&Path>) -> Result<(), SymlinkWalkError> {
    let mut current: Option<&Path> = Some(path);
    while let Some(p) = current {
        // Stop at the trusted boundary: `boundary` and its ancestors are OCX's
        // own store path, which may legitimately be a symlink. Only the
        // untrusted portion strictly below it is in scope.
        if boundary == Some(p) {
            break;
        }
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

    /// Canonicalize the tempdir path: on macOS `TempDir::new()` lands under
    /// `/var/folders/...`, but `/var` is a symlink to `/private/var`, which
    /// would trip [`refuse_if_symlink_in_path`] on every ancestor walk.
    fn temp_root() -> (TempDir, PathBuf) {
        let td = TempDir::new().unwrap();
        let canonical = td.path().canonicalize().unwrap();
        (td, canonical)
    }

    #[tokio::test]
    async fn absent_path_passes() {
        let (_td, root) = temp_root();
        let target = root.join("does/not/exist");
        refuse_if_symlink_in_path(&target, None).await.unwrap();
    }

    #[tokio::test]
    async fn regular_dir_passes() {
        let (_td, root) = temp_root();
        refuse_if_symlink_in_path(&root, None).await.unwrap();
    }

    #[tokio::test]
    async fn regular_file_passes() {
        let (_td, root) = temp_root();
        let f = root.join("file");
        tokio::fs::write(&f, b"x").await.unwrap();
        refuse_if_symlink_in_path(&f, None).await.unwrap();
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn leaf_symlink_rejected() {
        let td = TempDir::new().unwrap();
        let real = td.path().join("real");
        tokio::fs::create_dir(&real).await.unwrap();
        let link = td.path().join("link");
        tokio::fs::symlink(&real, &link).await.unwrap();
        match refuse_if_symlink_in_path(&link, None).await.unwrap_err() {
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
        match refuse_if_symlink_in_path(&target, None).await.unwrap_err() {
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
        match refuse_if_symlink_in_path(&link, None).await.unwrap_err() {
            SymlinkWalkError::Ancestor { .. } => {}
            other => panic!("expected Ancestor, got {other}"),
        }
    }

    /// A symlinked ancestor ABOVE the trusted boundary (simulating a symlinked
    /// `$OCX_HOME`, e.g. `~/.ocx` pointing at a larger disk) must NOT fail the
    /// check: only the untrusted portion strictly below `boundary` is in scope.
    /// Without the boundary the same symlinked ancestor is (correctly) rejected.
    #[tokio::test]
    #[cfg(unix)]
    async fn symlinked_ancestor_above_boundary_passes() {
        let td = TempDir::new().unwrap();
        // `store` is the real on-disk directory; `home_link -> store` simulates a
        // symlinked $OCX_HOME. The package content root lives under it.
        let store = td.path().join("store");
        tokio::fs::create_dir_all(store.join("content/prefix")).await.unwrap();
        let home_link = td.path().join("home_link");
        tokio::fs::symlink(&store, &home_link).await.unwrap();

        // dest_content and the prefix are addressed THROUGH the symlink, exactly
        // as an install under a symlinked $OCX_HOME would be.
        let dest_content = home_link.join("content");
        let prefix = dest_content.join("prefix");

        // Bounded at dest_content: the symlinked `home_link` ancestor is above
        // the boundary and never checked → passes.
        refuse_if_symlink_in_path(&prefix, Some(&dest_content))
            .await
            .expect("a symlinked ancestor above the boundary must not fail");

        // Unbounded: the same symlinked ancestor is now in scope → rejected.
        match refuse_if_symlink_in_path(&prefix, None).await.unwrap_err() {
            SymlinkWalkError::Ancestor { ancestor, .. } => assert_eq!(ancestor, home_link),
            other => panic!("expected Ancestor for the unbounded walk, got {other}"),
        }
    }
}
