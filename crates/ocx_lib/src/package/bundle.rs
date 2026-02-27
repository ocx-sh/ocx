use std::path::PathBuf;

use crate::{Error, ErrorExt, Result, archive, compression, log};

#[derive(Debug, Clone)]
enum BundleSource {
    Directory(PathBuf),
    Executable(PathBuf),
}

pub struct BundleBuilder {
    source: BundleSource,
    compression: compression::CompressionOptions,
    temp_dir: Option<tempfile::TempDir>,
}

impl BundleBuilder {
    /// Creates a new bundle builder from the given path.
    /// If the path is a directory, it will be bundled as is.
    /// If the path is a file, it will be bundled as an executable (placed in a "bin" directory in the bundle).
    pub fn from(path: impl AsRef<std::path::Path>) -> Self {
        let path: &std::path::Path = path.as_ref();
        if path.is_dir() {
            Self::from_dir(path)
        } else {
            Self::from_executable(path)
        }
    }

    pub fn from_dir(path: impl AsRef<std::path::Path>) -> Self {
        let path = path.as_ref().to_path_buf();
        Self {
            source: BundleSource::Directory(path),
            compression: Default::default(),
            temp_dir: None,
        }
    }

    pub fn from_executable(path: impl AsRef<std::path::Path>) -> Self {
        let path = path.as_ref().to_path_buf();
        Self {
            source: BundleSource::Executable(path),
            compression: Default::default(),
            temp_dir: None,
        }
    }

    pub fn with_compression(mut self, compression: compression::CompressionOptions) -> Self {
        self.compression = compression;
        self
    }

    pub async fn create(self, output: impl AsRef<std::path::Path>) -> Result<()> {
        let source = self.source.clone();
        match source {
            BundleSource::Directory(dir) => self.create_from_dir(dir, output).await,
            BundleSource::Executable(exe) => self.create_from_executable(exe, output).await,
        }
    }

    async fn create_from_dir(
        self,
        dir: impl AsRef<std::path::Path>,
        output: impl AsRef<std::path::Path>,
    ) -> Result<()> {
        let compression = self.compression;
        let mut archive = archive::Archive::create_with_compression(output, compression).await?;
        archive.add_dir_all("", dir).await?;
        archive.finish().await?;
        Ok(())
    }

    async fn create_from_executable(
        mut self,
        executable: impl AsRef<std::path::Path>,
        output: impl AsRef<std::path::Path>,
    ) -> Result<()> {
        let executable = executable.as_ref();
        if !executable.is_file() {
            log::error!("Executable path {} is not a file", executable.display());
            return Err(Error::Undefined);
        }

        let temp_dir = self.required_temp_dir()?;
        let bin_dir = &temp_dir.join("bin");
        std::fs::create_dir_all(bin_dir).map_to_undefined_error()?;
        let target_path = bin_dir.join(executable.file_name().map_to_undefined_error()?);
        std::fs::copy(executable, &target_path).map_to_undefined_error()?;
        self.create_from_dir(temp_dir, output).await
    }

    fn required_temp_dir(&mut self) -> Result<std::path::PathBuf> {
        if self.temp_dir.is_none() {
            self.temp_dir = Some(tempfile::TempDir::new().map_to_undefined_error()?);
        }
        Ok(self.temp_dir.as_ref().unwrap().path().to_path_buf())
    }
}
