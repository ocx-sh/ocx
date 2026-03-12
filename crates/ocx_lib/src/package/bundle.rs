// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

use crate::{Result, archive, compression};

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
    pub async fn create(self, output: impl AsRef<std::path::Path>) -> Result<()> {
        let mut archive = archive::Archive::create_with_compression(output, self.compression).await?;
        if self.source.is_dir() {
            archive.add_dir_all("", &self.source).await?;
        } else {
            let name = self.source.file_name().unwrap_or(self.source.as_os_str());
            archive.add_file(name, &self.source).await?;
        }
        archive.finish().await?;
        Ok(())
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

    /// Regression test for https://github.com/ocx-sh/ocx/issues/6
    ///
    /// Symlinks inside a directory (e.g. inside a macOS .app bundle) must be
    /// preserved as symlinks in the archive — not expanded to full file copies.
    /// Previously `tar::Builder::follow_symlinks` defaulted to `true`, causing
    /// every symlink target to be stored as a separate copy of its contents,
    /// bloating the archive by the size of each symlink target.
    #[cfg(unix)]
    #[tokio::test]
    async fn test_symlinks_preserved_not_expanded() {
        use std::os::unix::fs::symlink;

        let src = tempfile::tempdir().unwrap();

        // Create a real file (simulates a framework binary, e.g. QtCore)
        let real_file = src.path().join("libfoo.dylib");
        let content = vec![0u8; 1024]; // 1 KiB of zeros
        std::fs::write(&real_file, &content).unwrap();

        // Create a symlink pointing to the real file (simulates .app framework symlinks)
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

        // The symlink must be preserved as a symlink — not a regular file copy.
        assert!(
            extracted_link.symlink_metadata().unwrap().file_type().is_symlink(),
            "expected symlink to be preserved as a symlink after bundling, but it was stored as a regular file"
        );

        // The symlink target must be correct.
        let target = std::fs::read_link(&extracted_link).unwrap();
        assert_eq!(
            target.to_str().unwrap(),
            "libfoo.dylib",
            "symlink target was not preserved"
        );

        // Both the real file and the symlink exist.
        assert!(extract_dir.path().join("libfoo.dylib").exists());
    }
}
