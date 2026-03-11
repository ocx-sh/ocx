// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::fs::OpenOptions;

use crate::{ErrorExt, Result, compression};

mod extract_options;
pub use extract_options::ExtractOptions;

pub struct Archive {
    tar: tar::Builder<Box<dyn std::io::Write>>,
}

impl Archive {
    /// Creates a new archive at the given path.
    /// Any existing file at the path will be overwritten.
    /// If the path has a known compression extension, the corresponding compression algorithm will be used.
    /// Otherwise, a plain tar archive will be created.
    /// If you want to enforce compression, use `create_with_compression` instead.
    pub async fn create(output: impl AsRef<std::path::Path>) -> Result<Self> {
        let output = output.as_ref();
        if let Some(compression) = compression::CompressionAlgorithm::from_file(output) {
            Self::create_with_compression(output, compression::CompressionOptions::new(compression)).await
        } else {
            let file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(output)
                .map_to_undefined_error()?;
            let tar = tar::Builder::new(Box::new(file) as Box<dyn std::io::Write>);
            Ok(Self { tar })
        }
    }

    /// Creates a new archive at the given path with the given compression options.
    /// If the algorithm is not specified, it will be tried to infer it from the file extension of the output path.
    /// If the algorithm cannot be inferred, an error will be returned.
    pub async fn create_with_compression(
        output: impl AsRef<std::path::Path>,
        compression: compression::CompressionOptions,
    ) -> Result<Self> {
        let writer = compression::write_file(output, compression.algorithm, Some(compression.level)).await?;
        let tar = tar::Builder::new(writer);
        Ok(Self { tar })
    }

    /// Extracts the given archive to the given output path.
    /// If the archive has a known compression extension, the corresponding compression algorithm will be used.
    /// Otherwise, a plain tar archive will be assumed.
    pub async fn extract(archive: impl AsRef<std::path::Path>, output: impl AsRef<std::path::Path>) -> Result<()> {
        Self::extract_with_options(archive, output, None).await
    }

    /// Extracts the given archive to the given output path with the given compression algorithm.
    /// If the algorithm is not specified, it will be tried to infer it from the file extension of the archive path.
    /// If the algorithm cannot be inferred, a plain tar archive will be assumed.
    pub async fn extract_with_options(
        archive: impl AsRef<std::path::Path>,
        output: impl AsRef<std::path::Path>,
        options: Option<ExtractOptions>,
    ) -> Result<()> {
        /// Map a tar/io error to our error type, walking the full error chain
        /// so inner causes (e.g. xz decompression errors) are not lost.
        fn tar_error(e: std::io::Error) -> crate::Error {
            let mut msg = e.to_string();
            let mut source = std::error::Error::source(&e);
            while let Some(cause) = source {
                msg.push_str(": ");
                msg.push_str(&cause.to_string());
                source = cause.source();
            }
            crate::Error::UndefinedWithMessage(msg)
        }

        fn extract_impl(
            archive: impl std::io::Read,
            output: impl AsRef<std::path::Path>,
            options: &ExtractOptions,
        ) -> Result<()> {
            let mut archive = tar::Archive::new(archive);
            archive.set_preserve_permissions(true);
            if options.strip_components == 0 {
                archive.unpack(output).map_err(tar_error)?;
                return Ok(());
            }

            for entry in archive.entries().map_err(tar_error)? {
                let mut entry = entry.map_err(tar_error)?;
                let path = entry.path().map_err(tar_error)?;
                let stripped_path = path
                    .iter()
                    .skip(options.strip_components)
                    .collect::<std::path::PathBuf>();
                if stripped_path.as_os_str().is_empty() {
                    continue;
                }
                let output_path = output.as_ref().join(stripped_path);
                if let Some(parent) = output_path.parent() {
                    std::fs::create_dir_all(parent).map_to_undefined_error()?;
                }
                entry.unpack(output_path).map_err(tar_error)?;
            }

            Ok(())
        }

        let options = options.unwrap_or_default();
        let algorithm = match options.algorithm {
            Some(algorithm) => Some(algorithm),
            None => compression::CompressionAlgorithm::from_file(&archive),
        };
        if let Some(algorithm) = algorithm {
            let reader = compression::read_file(archive, Some(algorithm)).await?;
            extract_impl(reader, output, &options)?;
            Ok(())
        } else {
            let file = std::fs::File::open(archive).map_to_undefined_error()?;
            extract_impl(file, output, &options)?;
            Ok(())
        }
    }

    pub async fn add_file(
        &mut self,
        tar_path: impl AsRef<std::path::Path>,
        file: impl AsRef<std::path::Path>,
    ) -> Result<()> {
        let mut file = std::fs::File::open(file).map_to_undefined_error()?;
        self.tar.append_file(tar_path, &mut file).map_to_undefined_error()?;
        Ok(())
    }

    pub async fn add_dir(
        &mut self,
        tar_path: impl AsRef<std::path::Path>,
        dir: impl AsRef<std::path::Path>,
    ) -> Result<()> {
        self.tar.append_dir(tar_path, dir).map_to_undefined_error()?;
        Ok(())
    }

    pub async fn add_dir_all(
        &mut self,
        tar_path: impl AsRef<std::path::Path>,
        dir: impl AsRef<std::path::Path>,
    ) -> Result<()> {
        self.tar.append_dir_all(tar_path, dir).map_to_undefined_error()?;
        Ok(())
    }

    pub async fn finish(mut self) -> Result<()> {
        self.tar.finish().map_to_undefined_error()?;
        self.tar
            .into_inner()
            .map_to_undefined_error()?
            .flush()
            .map_to_undefined_error()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test;

    #[tokio::test]
    async fn test_extraction_strip_components() {
        let archive_xz = test::data::archive_xz();
        println!("Archive path: {:?}", archive_xz);
        let temp = tempfile::tempdir().unwrap();
        let output = temp.path().join("output");
        Archive::extract_with_options(
            archive_xz,
            &output,
            Some(ExtractOptions {
                algorithm: None,
                strip_components: 2,
            }),
        )
        .await
        .expect("Failed to extract archive.");
        assert!(!output.join("level_0.txt").exists());
        assert!(!output.join("content_0.txt").exists());
        assert!(output.join("content_0_0.txt").exists());
    }
}
