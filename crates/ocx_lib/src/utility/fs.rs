// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

mod assemble;
mod dir_walker;
mod drop_file;
pub mod path;

pub use assemble::{AssemblyError, AssemblyStats, assemble_from_layer, assemble_from_layers};
pub use dir_walker::{DirWalker, WalkDecision};
pub use drop_file::DropFile;

/// Returns whether `path` exists, swallowing any I/O error as `false`.
///
/// Wraps [`tokio::fs::try_exists`] and emits a `debug!` log whenever
/// the probe fails (permission denied, transient I/O, etc.) so the
/// swallow is still observable in diagnostic output. Use when the
/// caller is tolerant of a missing path — either because a follow-up
/// fallible operation will naturally surface the same error with
/// better context, or because absence and I/O failure are handled
/// identically at the call site.
pub async fn path_exists_lossy(path: &std::path::Path) -> bool {
    match tokio::fs::try_exists(path).await {
        Ok(exists) => exists,
        Err(e) => {
            crate::log::debug!("path_exists_lossy probe failed for {}: {}", path.display(), e);
            false
        }
    }
}

/// Moves `src` directory to `dst` via same-filesystem rename.
///
/// Creates parent directories of `dst` if needed. If `dst` already exists
/// (e.g., from a crashed previous attempt), it is removed first.
///
/// Uses `tokio::fs::rename` which requires `src` and `dst` to reside on
/// the same filesystem. Cross-device moves return an OS error.
pub async fn move_dir(src: &std::path::Path, dst: &std::path::Path) -> Result<(), crate::Error> {
    if let Some(parent) = dst.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| crate::error::file_error(parent, e))?;
    }
    if dst.exists() {
        tokio::fs::remove_dir_all(dst)
            .await
            .map_err(|e| crate::error::file_error(dst, e))?;
    }
    tokio::fs::rename(src, dst)
        .await
        .map_err(|e| crate::error::file_error(src, e))?;
    Ok(())
}
