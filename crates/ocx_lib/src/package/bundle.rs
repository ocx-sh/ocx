use std::path::PathBuf;

use crate::{Result, archive, compression};

/// Builds a compressed tar archive from a directory tree.
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
    /// The path is stored as-is; it is not validated until [`BundleBuilder::create`]
    /// is called.  Passing a file rather than a directory will result in an error
    /// at that point.
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
    /// All files and directories under the source path are added to the archive
    /// root (no extra top-level directory is inserted).
    pub async fn create(self, output: impl AsRef<std::path::Path>) -> Result<()> {
        let mut archive = archive::Archive::create_with_compression(output, self.compression).await?;
        archive.add_dir_all("", self.source).await?;
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

        assert!(extract_dir.path().join("bin/tool").exists(), "bin/tool missing after extraction");
        assert!(extract_dir.path().join("README").exists(), "README missing after extraction");
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
    async fn test_file_path_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("not-a-dir");
        std::fs::write(&file, b"data").unwrap();

        let out = dir.path().join("out.tar.xz");
        let result = BundleBuilder::from_path(&file).create(&out).await;
        assert!(result.is_err(), "expected error when source is a file");
    }
}
