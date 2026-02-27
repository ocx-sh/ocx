use crate::{log, prelude::*};

pub fn update(target_path: impl AsRef<std::path::Path>, link_path: impl AsRef<std::path::Path>) -> Result<()> {
    let link_path = link_path.as_ref();
    let target_path = target_path.as_ref();

    if link_path.exists() {
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

pub fn create(target: impl AsRef<std::path::Path>, link_path: impl AsRef<std::path::Path>) -> Result<()> {
    let target = target.as_ref();
    let link_path = link_path.as_ref();
    if let Some(parent) = link_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| Error::InternalFile(parent.to_path_buf(), error))?;
    }
    symlink::symlink_auto(target, link_path).map_err(|error| Error::InternalFile(link_path.to_path_buf(), error))?;
    Ok(())
}

pub fn remove(link_path: impl AsRef<std::path::Path>) -> Result<()> {
    let link_path = link_path.as_ref();
    if link_path.exists() {
        symlink::remove_symlink_auto(link_path)
            .map_err(|error| Error::InternalFile(link_path.to_path_buf(), error))?;
    }
    Ok(())
}
