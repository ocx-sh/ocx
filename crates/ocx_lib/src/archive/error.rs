// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

/// Errors that can occur during archive operations (create, extract, add).
#[derive(Debug)]
pub enum Error {
    /// A file I/O error with the associated path.
    Io(PathBuf, std::io::Error),
    /// A tar archive operation failed.
    Tar(std::io::Error),
    /// A zip archive operation failed.
    Zip(zip::result::ZipError),
    /// An archive entry path escapes the extraction root (path traversal).
    EntryEscape(PathBuf),
    /// A symlink target escapes the archive or extraction root (path traversal).
    SymlinkEscape { link: PathBuf, target: PathBuf },
    /// An unexpected internal error (e.g. task join failure).
    Internal(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(path, error) => write!(f, "Archive I/O error for '{}': {}", path.display(), error),
            Error::Tar(error) => {
                write!(f, "Tar error: {error}")?;
                let mut source = std::error::Error::source(error);
                while let Some(cause) = source {
                    write!(f, ": {cause}")?;
                    source = cause.source();
                }
                Ok(())
            }
            Error::Zip(error) => write!(f, "Zip error: {error}"),
            Error::EntryEscape(path) => {
                write!(f, "Archive entry '{}' escapes the extraction root", path.display())
            }
            Error::SymlinkEscape { link, target } => write!(
                f,
                "Symlink '{}' with target '{}' escapes the root directory",
                link.display(),
                target.display()
            ),
            Error::Internal(message) => write!(f, "Internal archive error: {message}"),
        }
    }
}

impl std::error::Error for Error {}
