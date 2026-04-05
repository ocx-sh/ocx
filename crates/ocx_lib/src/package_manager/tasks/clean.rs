// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

use crate::file_structure::StaleEntry;
use crate::log;

use super::super::PackageManager;
use super::garbage_collection::GarbageCollector;

pub struct CleanResult {
    pub objects: Vec<PathBuf>,
    pub temp: Vec<PathBuf>,
}

impl PackageManager {
    pub async fn clean(&self, dry_run: bool) -> crate::Result<CleanResult> {
        let profile = self.profile.snapshot();

        let garbage_collector = GarbageCollector::build(self.file_structure(), &profile).await?;
        let targets = garbage_collector.unreachable_objects();

        log::debug!(
            "Scanning for unreferenced entries{}: {} candidate(s).",
            if dry_run { " (dry run)" } else { "" },
            targets.len(),
        );

        let objects = garbage_collector.delete_objects(&targets, dry_run).await?;
        let temp = clean_temp(self.file_structure(), dry_run).await?;
        Ok(CleanResult { objects, temp })
    }
}

/// Removes stale temp directories and orphan lock files.
///
/// Uses [`TempStore::stale_entries`] which discovers entries from both
/// `.lock` files and directories, acquiring locks where possible to
/// prevent races with concurrent installs.
async fn clean_temp(fs: &crate::file_structure::FileStructure, dry_run: bool) -> crate::Result<Vec<PathBuf>> {
    let stale = fs.temp.stale_entries()?;

    log::debug!(
        "Found {} stale temp entry/entries{}.",
        stale.len(),
        if dry_run { " (dry run)" } else { "" },
    );

    let mut removed = Vec::new();

    for entry in stale {
        match entry {
            StaleEntry::Locked(acquired) => {
                let dir_path = acquired.dir.dir.clone();
                remove_stale_dir(&dir_path, dry_run, "stale").await?;
                // Drop releases the OS lock and auto-deletes the .lock file.
                drop(acquired);
                removed.push(dir_path);
            }
            StaleEntry::Orphan(dir_path) => {
                remove_stale_dir(&dir_path, dry_run, "orphan").await?;
                removed.push(dir_path);
            }
        }
    }

    log::debug!(
        "{} {} stale temp entry/entries.",
        if dry_run { "Would remove" } else { "Removed" },
        removed.len(),
    );

    Ok(removed)
}

async fn remove_stale_dir(dir_path: &std::path::Path, dry_run: bool, label: &str) -> crate::Result<()> {
    log::info!(
        "{} {} temp dir: {}",
        if dry_run { "Would remove" } else { "Removing" },
        label,
        dir_path.display(),
    );
    if !dry_run && dir_path.exists() {
        tokio::fs::remove_dir_all(dir_path)
            .await
            .map_err(|e| crate::Error::InternalFile(dir_path.to_path_buf(), e))?;
    }
    Ok(())
}
