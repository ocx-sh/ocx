// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

type Result<T> = std::result::Result<T, ocx_util::error::FileError>;

pub struct TempDir {
    pub dir: PathBuf,
}

impl TempDir {
    pub(super) fn has_artifacts(&self) -> Result<bool> {
        if !self.dir.exists() {
            return Ok(false);
        }
        let entries = std::fs::read_dir(&self.dir).map_err(|e| ocx_util::error::FileError::new(self.dir.clone(), e))?;
        Ok(entries.flatten().next().is_some())
    }

    /// Removes the contents, keeping the directory.
    pub(super) fn clear(&self) -> Result<()> {
        if !self.dir.exists() {
            return Ok(());
        }
        let entries = std::fs::read_dir(&self.dir).map_err(|e| ocx_util::error::FileError::new(self.dir.clone(), e))?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                std::fs::remove_dir_all(&path).map_err(|e| ocx_util::error::FileError::new(path, e))?;
            } else {
                std::fs::remove_file(&path).map_err(|e| ocx_util::error::FileError::new(path, e))?;
            }
        }
        Ok(())
    }
}
