use crate::{error, log, prelude::*};

/// A utility struct that deletes a file or directory when dropped, unless explicitly retained.
pub struct DropFile {
    path: Option<std::path::PathBuf>,
}

impl DropFile {
    /// Creates a new DropFile for the given path.
    pub fn new(path: impl Into<std::path::PathBuf>) -> Self {
        Self { path: Some(path.into()) }
    }

    /// Prevents the file or directory from being deleted when dropped.
    pub fn retain(&mut self) {
        self.path = None;
    }

    /// Deletes the file or directory if it exists.
    pub fn unlink(&mut self) -> Result<()> {
        let path = match &self.path {
            Some(path) => path,
            None => return Ok(()),
        };

        if path.is_dir() {
            std::fs::remove_dir_all(path).map_err(|error| error::file_error(path, error))?;
        } else {
            std::fs::remove_file(path).map_err(|error| error::file_error(path, error))?;
        }

        Ok(())
    }
}

impl Drop for DropFile {
    fn drop(&mut self) {
        if let Err(error) = self.unlink() {
            log::debug!("Failed to remove file: {}", error);
        }
    }
}
