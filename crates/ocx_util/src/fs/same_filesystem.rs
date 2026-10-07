// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Detect whether two paths reside on the same filesystem.

use std::path::{Path, PathBuf};

/// Failure modes of [`same_filesystem`].
#[derive(Debug, ocx_exit::Classify)]
pub enum SameFilesystemError {
    /// I/O failure resolving an existing ancestor of one of the inputs.
    #[exit(
        IoError,
        slug = "same_filesystem_check_failed",
        summary = "Probing whether two paths share a filesystem failed"
    )]
    Io { path: PathBuf, source: std::io::Error },
}

impl std::fmt::Display for SameFilesystemError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "I/O error checking filesystem for '{}': {source}", path.display(),),
        }
    }
}

impl std::error::Error for SameFilesystemError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
        }
    }
}

/// Whether `a` and `b` share a filesystem; a path that does not exist yet is keyed off its nearest existing ancestor.
pub async fn same_filesystem(a: &Path, b: &Path) -> Result<bool, SameFilesystemError> {
    let a_anchor = anchor(a).await?;
    let b_anchor = anchor(b).await?;
    compare_anchors(&a_anchor, &b_anchor).await
}

/// Resolve a path to the nearest existing ancestor that we can `stat`.
async fn anchor(path: &Path) -> Result<PathBuf, SameFilesystemError> {
    if try_exists(path).await? {
        return Ok(path.to_path_buf());
    }
    let mut current: Option<&Path> = path.parent();
    loop {
        match current {
            Some(p) => {
                let probe = if p.as_os_str().is_empty() { Path::new(".") } else { p };
                if try_exists(probe).await? {
                    return Ok(probe.to_path_buf());
                }
                current = p.parent();
            }
            None => return Ok(path.to_path_buf()),
        }
    }
}

async fn try_exists(path: &Path) -> Result<bool, SameFilesystemError> {
    tokio::fs::try_exists(path).await.map_err(|e| SameFilesystemError::Io {
        path: path.to_path_buf(),
        source: e,
    })
}

#[cfg(unix)]
async fn compare_anchors(a: &Path, b: &Path) -> Result<bool, SameFilesystemError> {
    use std::os::unix::fs::MetadataExt;
    let am = tokio::fs::metadata(a).await.map_err(|e| SameFilesystemError::Io {
        path: a.to_path_buf(),
        source: e,
    })?;
    let bm = tokio::fs::metadata(b).await.map_err(|e| SameFilesystemError::Io {
        path: b.to_path_buf(),
        source: e,
    })?;
    Ok(am.dev() == bm.dev())
}

#[cfg(windows)]
async fn compare_anchors(a: &Path, b: &Path) -> Result<bool, SameFilesystemError> {
    let av = volume_mount_point(a)?;
    let bv = volume_mount_point(b)?;
    Ok(av.eq_ignore_ascii_case(&bv))
}

/// The volume mount point for `path` (e.g. `C:\`), with symlinks and junctions resolved.
#[cfg(windows)]
fn volume_mount_point(path: &Path) -> Result<String, SameFilesystemError> {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows_sys::Win32::Storage::FileSystem::GetVolumePathNameW;

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut buf = vec![0u16; 261];
    // SAFETY: `wide` is NUL-terminated and `buf` is writable for `buf.len()` u16s; both outlive the call.
    let ok = unsafe { GetVolumePathNameW(wide.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) };
    if ok == 0 {
        return Err(SameFilesystemError::Io {
            path: path.to_path_buf(),
            source: std::io::Error::last_os_error(),
        });
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Ok(OsString::from_wide(&buf[..len]).to_string_lossy().into_owned())
}

#[cfg(not(any(unix, windows)))]
async fn compare_anchors(_a: &Path, _b: &Path) -> Result<bool, SameFilesystemError> {
    // No reliable cross-platform check; assume same filesystem.
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn same_dir_is_same_fs() {
        let td = TempDir::new().unwrap();
        assert!(same_filesystem(td.path(), td.path()).await.unwrap());
    }

    #[tokio::test]
    async fn absent_target_uses_parent() {
        let td = TempDir::new().unwrap();
        let absent = td.path().join("not-yet-created");
        assert!(same_filesystem(&absent, td.path()).await.unwrap());
    }

    #[tokio::test]
    async fn bare_relative_name_does_not_io_error() {
        // A bare relative name like "build" has an empty parent; the helper
        // must fall back to "." rather than ENOENT on metadata("").
        // Acceptance test covers the cross-fs case under cwd swap; here we
        // only assert that the bare relative anchor resolves without error.
        let td = TempDir::new().unwrap();
        let _ = same_filesystem(Path::new("build"), td.path()).await.unwrap();
    }
}
