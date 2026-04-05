// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

use crate::{Error, Result};

/// Represents a single temp directory.
pub struct TempDir {
    pub dir: PathBuf,
}

impl TempDir {
    /// Returns `true` if the directory contains any files or subdirectories.
    pub(super) fn has_artifacts(&self) -> Result<bool> {
        if !self.dir.exists() {
            return Ok(false);
        }
        let entries = std::fs::read_dir(&self.dir).map_err(|e| Error::InternalFile(self.dir.clone(), e))?;
        Ok(entries.flatten().next().is_some())
    }

    /// Removes all files and subdirectories.
    pub(super) fn clear(&self) -> Result<()> {
        if !self.dir.exists() {
            return Ok(());
        }
        let entries = std::fs::read_dir(&self.dir).map_err(|e| Error::InternalFile(self.dir.clone(), e))?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                std::fs::remove_dir_all(&path).map_err(|e| Error::InternalFile(path, e))?;
            } else {
                std::fs::remove_file(&path).map_err(|e| Error::InternalFile(path, e))?;
            }
        }
        Ok(())
    }
}
