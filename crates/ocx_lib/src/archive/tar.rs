// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::Result;

use super::backend::Backend;
use super::error::Error;

use crate::cli::progress::LOG_INTERVAL;

pub(super) struct TarBackend {
    inner: Arc<Mutex<tar::Builder<Box<dyn Write + Send>>>>,
}

impl TarBackend {
    pub fn new(writer: Box<dyn Write + Send>) -> Self {
        let mut builder = tar::Builder::new(writer);
        builder.follow_symlinks(false);
        // Deterministic headers: zero uid/gid/mtime/uname/gname. Without this, every
        // archive embeds the build user's uid and the current mtime, breaking byte-for-byte
        // reproducibility and producing files owned by a stale uid after extraction.
        builder.mode(tar::HeaderMode::Deterministic);
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
        .map_err(Error::internal)?
    }
}

#[async_trait::async_trait]
impl Backend for TarBackend {
    async fn add_file(&mut self, archive_path: PathBuf, file: PathBuf) -> Result<()> {
        self.run_blocking(move |builder| {
            let mut f = std::fs::File::open(&file).map_err(|e| Error::Io { path: file, source: e })?;
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
        self.run_blocking(move |builder| {
            let mut count = 0u64;
            add_dir_recursive(builder, &archive_path, &dir, &mut count)?;
            tracing::debug!("Bundled {count} entries total");
            Ok(())
        })
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
        .map_err(Error::internal)?
    }
}

fn add_dir_recursive(
    builder: &mut tar::Builder<Box<dyn Write + Send>>,
    base_path: &Path,
    dir: &Path,
    count: &mut u64,
) -> Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| Error::Io {
            path: dir.to_path_buf(),
            source: e,
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| Error::Io {
            path: dir.to_path_buf(),
            source: e,
        })?;
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let path = entry.path();
        let name = entry.file_name();
        let archive_name = if base_path.as_os_str().is_empty() {
            PathBuf::from(&name)
        } else {
            base_path.join(&name)
        };

        builder
            .append_path_with_name(&path, &archive_name)
            .map_err(Error::Tar)?;

        let ft = entry.file_type().map_err(|e| Error::Io {
            path: path.clone(),
            source: e,
        })?;
        if ft.is_dir() {
            add_dir_recursive(builder, &archive_name, &path, count)?;
        }

        *count += 1;
        tracing::trace!("Adding {}", archive_name.display());
        if (*count).is_multiple_of(LOG_INTERVAL) {
            tracing::debug!("Bundled {} entries", *count);
        }
    }
    Ok(())
}

pub(super) fn extract(reader: impl std::io::Read, output: &std::path::Path, strip_components: usize) -> Result<()> {
    let mut archive = tar::Archive::new(reader);
    archive.set_preserve_permissions(true);

    let mut count = 0u64;
    for entry in archive.entries().map_err(Error::Tar)? {
        let mut entry = entry.map_err(Error::Tar)?;
        let path = entry.path().map_err(Error::Tar)?.to_path_buf();
        let stripped: std::path::PathBuf = path.iter().skip(strip_components).collect();
        if stripped.as_os_str().is_empty() {
            continue;
        }

        // Reject entries whose path escapes the output root.
        if stripped.is_absolute() || crate::utility::fs::path::escapes_root(&stripped) {
            return Err(Error::EntryEscape(path).into());
        }

        let output_path = output.join(&stripped);

        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| Error::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }

        if entry.header().entry_type() == tar::EntryType::Symlink {
            if let Some(target) = entry.link_name().map_err(Error::Tar)? {
                crate::symlink::validate_target(output, &output_path, target.as_ref())?;
                crate::symlink::create(target.as_ref(), &output_path)?;
            }
        } else {
            entry.unpack(&output_path).map_err(Error::Tar)?;
        }

        count += 1;
        tracing::trace!("Extracted {}", stripped.display());
        if count.is_multiple_of(LOG_INTERVAL) {
            tracing::debug!("Extracted {count} entries");
        }
    }
    tracing::debug!("Extracted {count} entries total");

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::archive::Archive;

    /// Regression: tar archives must not embed the build host's ownership or per-file
    /// mtimes. Without `HeaderMode::Deterministic` every entry carries the build user's
    /// uid/gid and the source file's mtime, breaking byte-reproducibility and producing
    /// files owned by a stale uid after extraction on a different machine. The tar crate
    /// uses a fixed non-zero constant for mtime to work around tools that mishandle a
    /// zero timestamp (see rust-lang/cargo#9512), so we assert mtime is uniform across
    /// entries — not derived from the source filesystem.
    #[tokio::test]
    async fn test_headers_have_zero_ownership_and_constant_mtime() {
        let src = tempfile::tempdir().unwrap();
        let nested = src.path().join("sub");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(src.path().join("top.txt"), b"top").unwrap();
        std::fs::write(nested.join("inner.txt"), b"inner").unwrap();

        let out_dir = tempfile::tempdir().unwrap();
        let archive_path = out_dir.path().join("pkg.tar");

        let mut archive = Archive::create(&archive_path).await.unwrap();
        archive.add_dir_all("", src.path()).await.unwrap();
        archive.finish().await.unwrap();

        let file = std::fs::File::open(&archive_path).unwrap();
        let mut tar = ::tar::Archive::new(file);
        let mut entry_count = 0;
        let mut first_mtime: Option<u64> = None;
        for entry in tar.entries().unwrap() {
            let entry = entry.unwrap();
            let header = entry.header();
            let path = entry.path().unwrap().to_path_buf();
            assert_eq!(header.uid().unwrap(), 0, "uid not zeroed on {path:?}");
            assert_eq!(header.gid().unwrap(), 0, "gid not zeroed on {path:?}");
            assert_eq!(
                header.username().unwrap().unwrap_or(""),
                "",
                "uname not cleared on {path:?}"
            );
            assert_eq!(
                header.groupname().unwrap().unwrap_or(""),
                "",
                "gname not cleared on {path:?}"
            );
            let mtime = header.mtime().unwrap();
            match first_mtime {
                None => first_mtime = Some(mtime),
                Some(expected) => assert_eq!(
                    mtime, expected,
                    "mtime varies across entries (source mtime leaked) on {path:?}"
                ),
            }
            entry_count += 1;
        }
        assert!(entry_count >= 2, "expected at least 2 entries, got {entry_count}");
    }

    /// Regression: `HeaderMode::Deterministic` normalizes mode bits but must still
    /// propagate the user-execute bit so distributed binaries remain runnable after
    /// extraction. Regular files land at 0o644, executables at 0o755.
    #[cfg(unix)]
    #[tokio::test]
    async fn test_executable_bit_preserved_through_round_trip() {
        use std::os::unix::fs::PermissionsExt;

        let src = tempfile::tempdir().unwrap();
        let bin = src.path().join("tool");
        let data = src.path().join("data.txt");
        std::fs::write(&bin, b"#!/bin/sh\necho hi").unwrap();
        std::fs::write(&data, b"plain").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o644)).unwrap();

        let out_dir = tempfile::tempdir().unwrap();
        let archive_path = out_dir.path().join("pkg.tar");

        let mut archive = Archive::create(&archive_path).await.unwrap();
        archive.add_dir_all("", src.path()).await.unwrap();
        archive.finish().await.unwrap();

        let extract_dir = tempfile::tempdir().unwrap();
        Archive::extract(&archive_path, extract_dir.path()).await.unwrap();

        let bin_mode = extract_dir.path().join("tool").metadata().unwrap().permissions().mode() & 0o777;
        let data_mode = extract_dir
            .path()
            .join("data.txt")
            .metadata()
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(bin_mode, 0o755, "executable bit lost through round-trip");
        assert_eq!(data_mode, 0o644, "regular file mode not normalized to 0o644");
    }

    /// Regression: identical source trees produce byte-identical tar archives across
    /// invocations. Confirms determinism end-to-end.
    #[tokio::test]
    async fn test_archive_bytes_are_reproducible() {
        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("a.txt"), b"alpha").unwrap();
        std::fs::write(src.path().join("b.txt"), b"beta").unwrap();

        let out_dir = tempfile::tempdir().unwrap();
        let first = out_dir.path().join("first.tar");
        let second = out_dir.path().join("second.tar");

        for path in [&first, &second] {
            let mut archive = Archive::create(path).await.unwrap();
            archive.add_dir_all("", src.path()).await.unwrap();
            archive.finish().await.unwrap();
        }

        let bytes_first = std::fs::read(&first).unwrap();
        let bytes_second = std::fs::read(&second).unwrap();
        assert_eq!(
            bytes_first, bytes_second,
            "two runs over the same source tree produced different archive bytes"
        );
    }
}
