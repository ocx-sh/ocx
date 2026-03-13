// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::Result;

use super::backend::Backend;
use super::error::Error;

pub(super) struct TarBackend {
    inner: Arc<Mutex<tar::Builder<Box<dyn Write + Send>>>>,
}

impl TarBackend {
    pub fn new(writer: Box<dyn Write + Send>) -> Self {
        let mut builder = tar::Builder::new(writer);
        builder.follow_symlinks(false);
        Self {
            inner: Arc::new(Mutex::new(builder)),
        }
    }

    /// Locks the builder on a blocking thread, runs `f`, and releases the lock.
    async fn run_blocking<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&mut tar::Builder<Box<dyn Write + Send>>) -> Result<R> + Send + 'static,
        R: Send + 'static,
    {
        let inner = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let mut guard = inner.lock().unwrap_or_else(|e| e.into_inner());
            f(&mut guard)
        })
        .await
        .map_err(|e| Error::Internal(e.to_string()))?
    }
}

#[async_trait::async_trait]
impl Backend for TarBackend {
    async fn add_file(&mut self, archive_path: PathBuf, file: PathBuf) -> Result<()> {
        self.run_blocking(move |builder| {
            let mut f = std::fs::File::open(&file).map_err(|e| Error::Io(file, e))?;
            builder.append_file(&archive_path, &mut f).map_err(Error::Tar)?;
            Ok(())
        })
        .await
    }

    async fn add_dir(&mut self, archive_path: PathBuf, dir: PathBuf) -> Result<()> {
        self.run_blocking(move |builder| Ok(builder.append_dir(&archive_path, &dir).map_err(Error::Tar)?))
            .await
    }

    async fn add_dir_all(&mut self, archive_path: PathBuf, dir: PathBuf) -> Result<()> {
        self.run_blocking(move |builder| Ok(builder.append_dir_all(&archive_path, &dir).map_err(Error::Tar)?))
            .await
    }

    async fn finish(self: Box<Self>) -> Result<()> {
        let Ok(mutex) = Arc::try_unwrap(self.inner) else {
            panic!("backend has outstanding references");
        };
        let mut builder = mutex.into_inner().unwrap_or_else(|e| e.into_inner());
        tokio::task::spawn_blocking(move || {
            builder.finish().map_err(Error::Tar)?;
            builder.into_inner().map_err(Error::Tar)?.flush().map_err(Error::Tar)?;
            Ok(())
        })
        .await
        .map_err(|e| Error::Internal(e.to_string()))?
    }
}

pub(super) fn extract(reader: impl std::io::Read, output: &std::path::Path, strip_components: usize) -> Result<()> {
    let mut archive = tar::Archive::new(reader);
    archive.set_preserve_permissions(true);

    for entry in archive.entries().map_err(Error::Tar)? {
        let mut entry = entry.map_err(Error::Tar)?;
        let path = entry.path().map_err(Error::Tar)?.to_path_buf();
        let stripped: std::path::PathBuf = path.iter().skip(strip_components).collect();
        if stripped.as_os_str().is_empty() {
            continue;
        }

        // Reject entries whose path escapes the output root.
        if stripped.is_absolute() || super::escapes_root(&stripped) {
            return Err(Error::EntryEscape(path).into());
        }

        let output_path = output.join(&stripped);

        // Validate symlink targets resolve within the extraction root.
        if entry.header().entry_type() == tar::EntryType::Symlink
            && let Some(target) = entry.link_name().map_err(Error::Tar)?
        {
            super::validate_symlink_target(output, &output_path, target.as_ref())?;
        }

        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| Error::Io(parent.to_path_buf(), e))?;
        }
        entry.unpack(&output_path).map_err(Error::Tar)?;
    }

    Ok(())
}
