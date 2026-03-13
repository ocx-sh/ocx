// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

use crate::Result;

/// Trait for archive format backends (tar, zip).
#[async_trait::async_trait]
pub(super) trait Backend: Send {
    async fn add_file(&mut self, archive_path: PathBuf, file: PathBuf) -> Result<()>;
    async fn add_dir(&mut self, archive_path: PathBuf, dir: PathBuf) -> Result<()>;
    async fn add_dir_all(&mut self, archive_path: PathBuf, dir: PathBuf) -> Result<()>;
    async fn finish(self: Box<Self>) -> Result<()>;
}
