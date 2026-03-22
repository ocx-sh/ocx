// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

/// Errors that can occur during archive operations (create, extract, add).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A file I/O error with the associated path.
    #[error("Archive I/O error for '{}': {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// A tar archive operation failed.
    #[error("Tar error: {0}")]
    Tar(#[source] std::io::Error),
    /// A zip archive operation failed.
    #[error("Zip error: {0}")]
    Zip(#[source] zip::result::ZipError),
    /// An archive entry path escapes the extraction root (path traversal).
    #[error("Archive entry '{path}' escapes the extraction root", path = .0.display())]
    EntryEscape(PathBuf),
    /// A symlink target escapes the archive or extraction root (path traversal).
    #[error("Symlink '{link}' with target '{target}' escapes the root directory", link = .link.display(), target = .target.display())]
    SymlinkEscape { link: PathBuf, target: PathBuf },
    /// The archive format is not supported.
    #[error("Unsupported archive format: {0}")]
    UnsupportedFormat(String),
    /// An unexpected internal error (e.g. task join failure).
    #[error("Internal archive error: {0}")]
    Internal(#[source] Box<dyn std::error::Error + Send + Sync>),
}

impl Error {
    /// Wrap any error as an [`Error::Internal`].
    pub fn internal(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Internal(Box::new(error))
    }
}
