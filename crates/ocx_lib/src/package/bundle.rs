// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::{Path, PathBuf};

use crate::{Result, archive, compression, utility};

/// Builds a compressed tar archive from a file or directory tree.
///
/// When the source is a directory, all files and subdirectories are added to
/// the archive root.  When the source is a single file (e.g. an executable),
/// it is archived under its filename.
///
/// The compression algorithm is determined by the file extension of the output
/// path passed to [`BundleBuilder::create`]: `.tar.xz` selects LZMA (the
/// default when the filename is inferred), `.tar.gz` / `.tgz` selects Gzip.
/// The compression level can be overridden with [`BundleBuilder::with_compression`].
pub struct BundleBuilder {
    source: PathBuf,
    compression: compression::CompressionOptions,
}

impl BundleBuilder {
    /// Creates a new `BundleBuilder` for the given source path.
    ///
    /// The path may point to a directory or a single file.  It is stored as-is
    /// and not validated until [`BundleBuilder::create`] is called.
    pub fn from_path(path: impl AsRef<std::path::Path>) -> Self {
        Self {
            source: path.as_ref().to_path_buf(),
            compression: Default::default(),
        }
    }

    /// Overrides the compression options (algorithm and level).
    ///
    /// When `algorithm` is `None` inside the options, the algorithm is inferred
    /// from the output file extension at creation time.
    pub fn with_compression(mut self, compression: compression::CompressionOptions) -> Self {
        self.compression = compression;
        self
    }

    /// Creates the archive at `output`.
    ///
    /// The compression algorithm is inferred from the output file extension if
    /// not already set via [`BundleBuilder::with_compression`].
    ///
    /// If the source is a directory, all files and subdirectories are added to
    /// the archive root (no extra top-level directory is inserted).  If the
    /// source is a single file, it is added under its filename.
    ///
    /// The archive is first written to a temporary file alongside the output
    /// path and atomically renamed on success.  If creation fails, the
    /// temporary file is automatically cleaned up.
    pub async fn create(self, output: impl AsRef<std::path::Path>) -> Result<()> {
        let output = output.as_ref();
        let temp_path = temp_path_for(output);
        let mut temp_guard = utility::drop_file::DropFile::new(&temp_path);

        let mut archive = archive::Archive::create_with_compression(&temp_path, self.compression).await?;
        if self.source.is_dir() {
            let bar = match count_entries(&self.source) {
                Ok(count) => crate::cli::progress::ProgressBar::files(tracing::info_span!("Bundling"), count),
                Err(e) => {
                    tracing::debug!("Could not count entries: {e}");
                    crate::cli::progress::ProgressBar::from(tracing::info_span!("Bundling"))
                }
            };

            let _guard = bar.enter();
            archive.add_dir_all("", &self.source).await?;
        } else {
            let name = self.source.file_name().unwrap_or(self.source.as_os_str());
            archive.add_file(name, &self.source).await?;
        }

        {
            let _span = tracing::info_span!("Finishing").entered();
            archive.finish().await?;
        }

        std::fs::rename(&temp_path, output).map_err(|e| crate::error::file_error(&temp_path, e))?;
        temp_guard.retain();
        Ok(())
    }
}

fn count_entries(dir: &Path) -> std::io::Result<u64> {
    let mut count = 0u64;
    count_entries_recursive(dir, &mut count)?;
    Ok(count)
}

fn count_entries_recursive(dir: &Path, count: &mut u64) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        *count += 1;
        let ft = entry.file_type()?;
        if ft.is_dir() {
            count_entries_recursive(&entry.path(), count)?;
        }
    }
    Ok(())
}

/// Returns a temporary path in the same directory as `output` with a `._tmp_`
/// prefix on the filename. This preserves the original file extension so that
/// archive format detection works unchanged.
fn temp_path_for(output: &Path) -> PathBuf {
    let name = output.file_name().unwrap_or_default();
    let mut tmp_name = std::ffi::OsString::from("._tmp_");
    tmp_name.push(name);
    match output.parent() {
        Some(parent) => parent.join(tmp_name),
        None => PathBuf::from(tmp_name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::Archive;

    /// Creates a temporary source directory with a known file layout, bundles
    /// it, then extracts the bundle and asserts the layout is preserved.
    async fn round_trip(extension: &str) {
        let src = tempfile::tempdir().unwrap();
        let bin_dir = src.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        std::fs::write(bin_dir.join("tool"), b"#!/bin/sh\necho hello").unwrap();
        std::fs::write(src.path().join("README"), b"test package").unwrap();

        let out_dir = tempfile::tempdir().unwrap();
        let archive_path = out_dir.path().join(format!("pkg.{extension}"));

        BundleBuilder::from_path(src.path())
            .create(&archive_path)
            .await
            .expect("bundle creation failed");

        assert!(archive_path.exists(), "archive file was not created");

        let extract_dir = tempfile::tempdir().unwrap();
        Archive::extract(&archive_path, extract_dir.path())
            .await
            .expect("extraction failed");

        assert!(
            extract_dir.path().join("bin/tool").exists(),
            "bin/tool missing after extraction"
        );
        assert!(
            extract_dir.path().join("README").exists(),
            "README missing after extraction"
        );
        assert_eq!(
            std::fs::read(extract_dir.path().join("README")).unwrap(),
            b"test package",
        );
    }

    #[tokio::test]
    async fn test_round_trip_xz() {
        round_trip("tar.xz").await;
    }

    #[tokio::test]
    async fn test_round_trip_gz() {
        round_trip("tar.gz").await;
    }

    #[tokio::test]
    async fn test_round_trip_zip() {
        round_trip("zip").await;
    }

    #[tokio::test]
    async fn test_single_file_round_trip_zip() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("my-tool");
        std::fs::write(&file, b"#!/bin/sh\necho hello").unwrap();

        let out = dir.path().join("out.zip");
        BundleBuilder::from_path(&file)
            .create(&out)
            .await
            .expect("bundle creation failed");

        let extract_dir = tempfile::tempdir().unwrap();
        Archive::extract(&out, extract_dir.path())
            .await
            .expect("extraction failed");

        let extracted = extract_dir.path().join("my-tool");
        assert!(extracted.exists(), "file missing after extraction");
        assert_eq!(std::fs::read(&extracted).unwrap(), b"#!/bin/sh\necho hello");
    }

    /// Regression test for symlink preservation (inspired by issue #6).
    ///
    /// Symlinks inside a directory must be preserved as symlinks in tar archives,
    /// not expanded to full file copies.
    #[cfg(unix)]
    #[tokio::test]
    async fn test_symlinks_preserved_tar() {
        use std::os::unix::fs::symlink;

        let src = tempfile::tempdir().unwrap();
        let real_file = src.path().join("libfoo.dylib");
        std::fs::write(&real_file, vec![0u8; 1024]).unwrap();
        let link = src.path().join("libfoo_link.dylib");
        symlink("libfoo.dylib", &link).unwrap();

        let out_dir = tempfile::tempdir().unwrap();
        let archive_path = out_dir.path().join("pkg.tar.xz");

        BundleBuilder::from_path(src.path())
            .create(&archive_path)
            .await
            .expect("bundle creation failed");

        let extract_dir = tempfile::tempdir().unwrap();
        Archive::extract(&archive_path, extract_dir.path())
            .await
            .expect("extraction failed");

        let extracted_link = extract_dir.path().join("libfoo_link.dylib");
        assert!(
            extracted_link.symlink_metadata().unwrap().file_type().is_symlink(),
            "expected symlink to be preserved, but it was stored as a regular file"
        );
        assert_eq!(
            std::fs::read_link(&extracted_link).unwrap().to_str().unwrap(),
            "libfoo.dylib"
        );
        assert!(extract_dir.path().join("libfoo.dylib").exists());
    }

    /// Symlinks must survive a zip round-trip.
    #[cfg(unix)]
    #[tokio::test]
    async fn test_symlinks_preserved_zip() {
        use std::os::unix::fs::symlink;

        let src = tempfile::tempdir().unwrap();
        let real_file = src.path().join("libfoo.dylib");
        std::fs::write(&real_file, vec![0u8; 1024]).unwrap();
        let link = src.path().join("libfoo_link.dylib");
        symlink("libfoo.dylib", &link).unwrap();

        let out_dir = tempfile::tempdir().unwrap();
        let archive_path = out_dir.path().join("pkg.zip");

        BundleBuilder::from_path(src.path())
            .create(&archive_path)
            .await
            .expect("bundle creation failed");

        let extract_dir = tempfile::tempdir().unwrap();
        Archive::extract(&archive_path, extract_dir.path())
            .await
            .expect("extraction failed");

        let extracted_link = extract_dir.path().join("libfoo_link.dylib");
        assert!(
            extracted_link.symlink_metadata().unwrap().file_type().is_symlink(),
            "expected symlink to be preserved, but it was stored as a regular file"
        );
        assert_eq!(
            std::fs::read_link(&extracted_link).unwrap().to_str().unwrap(),
            "libfoo.dylib"
        );
        assert!(extract_dir.path().join("libfoo.dylib").exists());
    }

    /// Unix file permissions must survive a zip round-trip.
    #[cfg(unix)]
    #[tokio::test]
    async fn test_permissions_preserved_zip() {
        use std::os::unix::fs::PermissionsExt;

        let src = tempfile::tempdir().unwrap();
        let bin = src.path().join("tool");
        std::fs::write(&bin, b"#!/bin/sh\necho hello").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();

        let out_dir = tempfile::tempdir().unwrap();
        let archive_path = out_dir.path().join("pkg.zip");

        BundleBuilder::from_path(src.path())
            .create(&archive_path)
            .await
            .expect("bundle creation failed");

        let extract_dir = tempfile::tempdir().unwrap();
        Archive::extract(&archive_path, extract_dir.path())
            .await
            .expect("extraction failed");

        let extracted = extract_dir.path().join("tool");
        let mode = extracted.metadata().unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "executable bits not preserved: {mode:#o}");
    }

    #[tokio::test]
    async fn test_single_file_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("my-tool");
        std::fs::write(&file, b"#!/bin/sh\necho hello").unwrap();

        let out = dir.path().join("out.tar.xz");
        BundleBuilder::from_path(&file)
            .create(&out)
            .await
            .expect("bundle creation failed");

        let extract_dir = tempfile::tempdir().unwrap();
        Archive::extract(&out, extract_dir.path())
            .await
            .expect("extraction failed");

        let extracted = extract_dir.path().join("my-tool");
        assert!(extracted.exists(), "file missing after extraction");
        assert_eq!(std::fs::read(&extracted).unwrap(), b"#!/bin/sh\necho hello");
    }
}
