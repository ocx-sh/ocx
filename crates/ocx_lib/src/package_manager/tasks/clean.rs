use std::path::PathBuf;

use crate::log;

use super::super::PackageManager;

pub struct CleanResult {
    pub objects: Vec<PathBuf>,
    pub temp: Vec<PathBuf>,
}

impl PackageManager {
    pub fn clean(&self, dry_run: bool) -> crate::Result<CleanResult> {
        let objects = self.clean_unreferenced_objects(dry_run)?;
        let temp = self.clean_stale_temp(dry_run)?;
        Ok(CleanResult { objects, temp })
    }

    /// Removes objects whose `refs/` directory is empty or absent.
    fn clean_unreferenced_objects(&self, dry_run: bool) -> crate::Result<Vec<PathBuf>> {
        let object_dirs = self.file_structure().objects.list_all()?;

        log::debug!(
            "Scanning {} object(s) for unreferenced entries{}.",
            object_dirs.len(),
            if dry_run { " (dry run)" } else { "" },
        );

        let mut removed = Vec::new();

        for obj in object_dirs {
            let refs_dir = obj.refs_dir();
            let is_empty = !refs_dir.is_dir()
                || std::fs::read_dir(&refs_dir)
                    .map(|mut d| d.next().is_none())
                    .unwrap_or(true);

            log::trace!(
                "Object '{}': refs/ {}.",
                obj.dir.display(),
                if is_empty { "is empty" } else { "has references" },
            );

            if is_empty {
                log::info!(
                    "{} unreferenced object: {}",
                    if dry_run { "Would remove" } else { "Removing" },
                    obj.dir.display(),
                );
                if !dry_run {
                    std::fs::remove_dir_all(&obj.dir)
                        .map_err(|e| crate::Error::InternalFile(obj.dir.clone(), e))?;
                }
                removed.push(obj.dir.clone());
            }
        }

        log::debug!(
            "{} {} unreferenced object(s).",
            if dry_run { "Would remove" } else { "Removed" },
            removed.len(),
        );

        Ok(removed)
    }

    /// Removes temp directories whose `install.lock` is not held by any process.
    ///
    /// Uses [`TempStore::stale_dirs`] which acquires locks, preventing races
    /// with concurrent installs.
    fn clean_stale_temp(&self, dry_run: bool) -> crate::Result<Vec<PathBuf>> {
        let stale = self.file_structure().temp.stale_dirs()?;

        log::debug!(
            "Found {} stale temp dir(s){}.",
            stale.len(),
            if dry_run { " (dry run)" } else { "" },
        );

        let mut removed = Vec::new();

        for acquired in stale {
            let dir_path = acquired.dir.dir.clone();
            log::info!(
                "{} stale temp dir: {}",
                if dry_run { "Would remove" } else { "Removing" },
                dir_path.display(),
            );
            if !dry_run {
                // Remove while holding the lock — prevents races with
                // concurrent installs. The fd-based advisory lock remains
                // valid even after the file is unlinked (POSIX semantics).
                std::fs::remove_dir_all(&dir_path)
                    .map_err(|e| crate::Error::InternalFile(dir_path.clone(), e))?;
            }
            drop(acquired);
            removed.push(dir_path);
        }

        log::debug!(
            "{} {} stale temp dir(s).",
            if dry_run { "Would remove" } else { "Removed" },
            removed.len(),
        );

        Ok(removed)
    }
}
