// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::io::{Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use zip::write::SimpleFileOptions;

use crate::{Result, compression};

use tracing_indicatif::span_ext::IndicatifSpanExt;

use super::backend::Backend;
use super::error::Error;

use crate::cli::progress::LOG_INTERVAL;

/// Object-safe trait combining `Write`, `Seek`, and `Send` (required by `ZipWriter`).
pub(super) trait WriteSeek: Write + Seek + Send {}
impl<T: Write + Seek + Send> WriteSeek for T {}

pub(super) struct ZipBackend {
    inner: Arc<Mutex<zip::ZipWriter<Box<dyn WriteSeek>>>>,
    options: SimpleFileOptions,
    output_path: PathBuf,
}

impl ZipBackend {
    pub fn new(output: &Path, level: compression::CompressionLevel) -> Result<Self> {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(output)
            .map_err(|e| Error::Io(output.to_path_buf(), e))?;
        let options = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .compression_level(Some(match level {
                compression::CompressionLevel::Fast => 1,
                compression::CompressionLevel::Best => 9,
                compression::CompressionLevel::Default => 6,
            }));
        let writer = zip::ZipWriter::new(Box::new(file) as Box<dyn WriteSeek>);
        Ok(Self {
            inner: Arc::new(Mutex::new(writer)),
            options,
            output_path: output.to_path_buf(),
        })
    }

    /// Locks the writer on a blocking thread, runs `f`, and releases the lock.
    async fn run_blocking<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&mut zip::ZipWriter<Box<dyn WriteSeek>>) -> Result<R> + Send + 'static,
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
impl Backend for ZipBackend {
    async fn add_file(&mut self, archive_path: PathBuf, file: PathBuf) -> Result<()> {
        let options = self.options;
        self.run_blocking(move |writer| {
            let opts = file_options_with_permissions(options, &file);
            let mut source = std::fs::File::open(&file).map_err(|e| Error::Io(file, e))?;
            let name = path_to_zip_name(&archive_path);
            writer.start_file(name, opts).map_err(Error::Zip)?;
            std::io::copy(&mut source, writer).map_err(|e| Error::Io(archive_path, e))?;
            Ok(())
        })
        .await
    }

    async fn add_dir(&mut self, archive_path: PathBuf, _dir: PathBuf) -> Result<()> {
        let options = self.options;
        self.run_blocking(move |writer| {
            let name = path_to_zip_name(&archive_path);
            if !name.is_empty() {
                let dir_name = if name.ends_with('/') { name } else { format!("{name}/") };
                writer.add_directory(dir_name, options).map_err(Error::Zip)?;
            }
            Ok(())
        })
        .await
    }

    async fn add_dir_all(&mut self, archive_path: PathBuf, dir: PathBuf) -> Result<()> {
        let options = self.options;
        let span = tracing::Span::current();
        self.run_blocking(move |writer| {
            let _guard = span.entered();
            let mut count = 0u64;
            add_dir_recursive(writer, options, &archive_path, &dir, &mut count)?;
            tracing::debug!("Bundled {count} entries total");
            Ok(())
        })
        .await
    }

    async fn finish(self: Box<Self>) -> Result<()> {
        let Ok(mutex) = Arc::try_unwrap(self.inner) else {
            panic!("backend has outstanding references");
        };
        let writer = mutex.into_inner().unwrap_or_else(|e| e.into_inner());
        let output_path = self.output_path;
        tokio::task::spawn_blocking(move || {
            let mut inner = writer.finish().map_err(Error::Zip)?;
            inner.flush().map_err(|e| Error::Io(output_path, e))?;
            Ok(())
        })
        .await
        .map_err(|e| Error::Internal(e.to_string()))?
    }
}

fn add_dir_recursive(
    writer: &mut zip::ZipWriter<Box<dyn WriteSeek>>,
    options: SimpleFileOptions,
    base_path: &Path,
    dir: &Path,
    count: &mut u64,
) -> Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| Error::Io(dir.to_path_buf(), e))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| Error::Io(dir.to_path_buf(), e))?;
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let file_type = entry.file_type().map_err(|e| Error::Io(entry.path(), e))?;
        let name = entry.file_name();
        let archive_path = if base_path.as_os_str().is_empty() {
            PathBuf::from(&name)
        } else {
            base_path.join(&name)
        };

        if file_type.is_symlink() {
            let entry_path = entry.path();
            let target = std::fs::read_link(&entry_path).map_err(|e| Error::Io(entry_path, e))?;
            let link_name = path_to_zip_name(&archive_path);
            let target_name = target.to_string_lossy();
            writer
                .add_symlink(link_name, &*target_name, options)
                .map_err(Error::Zip)?;
        } else if file_type.is_dir() {
            let dir_name = path_to_zip_name(&archive_path);
            let dir_name = if dir_name.ends_with('/') {
                dir_name
            } else {
                format!("{dir_name}/")
            };
            writer.add_directory(dir_name, options).map_err(Error::Zip)?;
            add_dir_recursive(writer, options, &archive_path, &entry.path(), count)?;
        } else {
            let file_path = entry.path();
            let file_name = path_to_zip_name(&archive_path);
            let opts = file_options_with_permissions(options, &file_path);
            writer.start_file(file_name, opts).map_err(Error::Zip)?;
            let mut source = std::fs::File::open(&file_path).map_err(|e| Error::Io(file_path.clone(), e))?;
            std::io::copy(&mut source, writer).map_err(|e| Error::Io(file_path, e))?;
        }

        *count += 1;
        tracing::trace!("Adding {}", archive_path.display());
        if (*count).is_multiple_of(LOG_INTERVAL) {
            tracing::debug!("Bundled {} entries", *count);
        }

        tracing::Span::current().pb_inc(1);
    }
    Ok(())
}

/// Converts a `Path` to a forward-slash ZIP entry name.
fn path_to_zip_name(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Returns `options` with Unix permissions copied from `file`, if available.
#[cfg(unix)]
fn file_options_with_permissions(options: SimpleFileOptions, file: &Path) -> SimpleFileOptions {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(metadata) = file.metadata() {
        options.unix_permissions(metadata.permissions().mode())
    } else {
        options
    }
}

#[cfg(not(unix))]
fn file_options_with_permissions(options: SimpleFileOptions, _file: &Path) -> SimpleFileOptions {
    options
}

pub(super) fn extract(archive: &Path, output: &Path, strip_components: usize) -> Result<()> {
    let file = std::fs::File::open(archive).map_err(|e| Error::Io(archive.to_path_buf(), e))?;
    let mut zip = zip::ZipArchive::new(file).map_err(Error::Zip)?;

    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(Error::Zip)?;
        let Some(enclosed_name) = entry.enclosed_name().map(|p| p.to_path_buf()) else {
            continue;
        };

        let stripped: PathBuf = enclosed_name.iter().skip(strip_components).collect();
        if stripped.as_os_str().is_empty() {
            continue;
        }

        let output_path = output.join(&stripped);

        if entry.is_dir() {
            std::fs::create_dir_all(&output_path).map_err(|e| Error::Io(output_path.clone(), e))?;
        } else if entry.is_symlink() {
            if let Some(parent) = output_path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| Error::Io(parent.to_path_buf(), e))?;
            }
            let mut target = String::new();
            std::io::Read::read_to_string(&mut entry, &mut target).map_err(|e| Error::Io(output_path.clone(), e))?;
            super::validate_symlink_target(output, &output_path, Path::new(&target))?;
            #[cfg(unix)]
            {
                std::os::unix::fs::symlink(&target, &output_path).map_err(|e| Error::Io(output_path.clone(), e))?;
            }
            #[cfg(windows)]
            {
                // On Windows, try directory symlink first since we can't easily distinguish
                let target_path = output_path.parent().unwrap_or(Path::new(".")).join(&target);
                if target_path.is_dir() {
                    std::os::windows::fs::symlink_dir(&target, &output_path)
                        .map_err(|e| Error::Io(output_path.clone(), e))?;
                } else {
                    std::os::windows::fs::symlink_file(&target, &output_path)
                        .map_err(|e| Error::Io(output_path.clone(), e))?;
                }
            }
        } else {
            if let Some(parent) = output_path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| Error::Io(parent.to_path_buf(), e))?;
            }
            let mut outfile = std::fs::File::create(&output_path).map_err(|e| Error::Io(output_path.clone(), e))?;
            std::io::copy(&mut entry, &mut outfile).map_err(|e| Error::Io(output_path.clone(), e))?;

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Some(mode) = entry.unix_mode() {
                    std::fs::set_permissions(&output_path, std::fs::Permissions::from_mode(mode))
                        .map_err(|e| Error::Io(output_path.clone(), e))?;
                }
            }
        }
    }

    Ok(())
}
