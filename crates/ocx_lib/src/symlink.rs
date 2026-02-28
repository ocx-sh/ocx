//! Low-level symlink primitives (create, update, remove).
//!
//! These functions operate on a single symlink without any bookkeeping.
//! For install symlinks (candidates and current under `installs/`), use
//! [`crate::reference_manager::ReferenceManager`] instead — it keeps the
//! `refs/` back-references in sync, which is required for garbage collection.

use crate::{log, prelude::*};

/// Creates or updates a symlink at `link_path` pointing to `target_path`.
///
/// No-op if `link_path` already resolves to `target_path`.
/// Removes any existing symlink (including dangling ones) before creating anew.
///
/// For install symlinks, use [`crate::reference_manager::ReferenceManager::link`].
pub fn update(target_path: impl AsRef<std::path::Path>, link_path: impl AsRef<std::path::Path>) -> Result<()> {
    let link_path = link_path.as_ref();
    let target_path = target_path.as_ref();

    if link_path.exists() || link_path.is_symlink() {
        let link_resolved =
            std::fs::read_link(link_path).map_err(|error| Error::InternalFile(link_path.to_path_buf(), error))?;
        if link_resolved == target_path {
            log::debug!("Symlink at '{}' already points to '{}', skipping update.", link_path.display(), target_path.display());
            return Ok(());
        }
        log::debug!("Symlink at '{}' points to '{}', updating to point to '{}'.", link_path.display(), link_resolved.display(), target_path.display());
        remove(link_path)?;
    }
    create(target_path, link_path)
}

/// Creates a new symlink at `link_path` pointing to `target`.
///
/// Creates any missing parent directories. Fails if `link_path` already exists.
///
/// For install symlinks, use [`crate::reference_manager::ReferenceManager::link`].
pub fn create(target: impl AsRef<std::path::Path>, link_path: impl AsRef<std::path::Path>) -> Result<()> {
    let target = target.as_ref();
    let link_path = link_path.as_ref();
    if let Some(parent) = link_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| Error::InternalFile(parent.to_path_buf(), error))?;
    }
    symlink::symlink_auto(target, link_path).map_err(|error| Error::InternalFile(link_path.to_path_buf(), error))?;
    Ok(())
}

/// Removes the symlink at `link_path`.
///
/// No-op if `link_path` does not exist and is not a dangling symlink.
///
/// For install symlinks, use [`crate::reference_manager::ReferenceManager::unlink`].
pub fn remove(link_path: impl AsRef<std::path::Path>) -> Result<()> {
    let link_path = link_path.as_ref();
    if link_path.exists() || link_path.is_symlink() {
        symlink::remove_symlink_auto(link_path)
            .map_err(|error| Error::InternalFile(link_path.to_path_buf(), error))?;
    }
    Ok(())
}
