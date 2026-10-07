// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
#[exit(family = "ArchiveError")]
pub enum Error {
    #[error("archive I/O error for '{}': {source}", path.display())]
    #[exit(IoError, slug = "archive_io", summary = "Reading or writing an archive entry failed")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("tar error: {0}")]
    #[exit(DataError, slug = "archive_tar_invalid", summary = "The tar stream is malformed")]
    Tar(#[source] std::io::Error),
    #[error("zip error: {0}")]
    #[exit(DataError, slug = "archive_zip_invalid", summary = "The zip archive is malformed")]
    Zip(#[source] zip::result::ZipError),
    #[error("archive entry '{path}' escapes the extraction root", path = .0.display())]
    #[exit(
        DataError,
        slug = "archive_entry_escape",
        summary = "An archive entry path escapes the extraction root"
    )]
    EntryEscape(PathBuf),
    #[error("symlink '{link}' with target '{target}' escapes the root directory", link = .link.display(), target = .target.display())]
    #[exit(
        DataError,
        slug = "archive_symlink_escape",
        summary = "An archive symlink points outside the extraction root"
    )]
    SymlinkEscape { link: PathBuf, target: PathBuf },
    #[error("hard link '{link}' target '{target}' does not resolve inside the extraction root", link = .link.display(), target = .target.display())]
    #[exit(
        DataError,
        slug = "archive_hard_link_escape",
        summary = "An archive hard link does not resolve inside the extraction root"
    )]
    HardLinkEscape { link: PathBuf, target: PathBuf },
    #[error("unsupported archive format: {0}")]
    #[exit(
        DataError,
        slug = "archive_unsupported_format",
        summary = "The archive format is not supported"
    )]
    UnsupportedFormat(String),
    /// Refused: a sparse entry's apparent size is materialized without stream bytes, bypassing the decompression cap.
    #[error("archive entry '{}' uses the unsupported GNU sparse format", .0.display())]
    #[exit(
        DataError,
        slug = "archive_gnu_sparse_unsupported",
        summary = "An archive entry uses the unsupported GNU sparse format"
    )]
    GnuSparseUnsupported(PathBuf),
    #[error("archive extraction exceeded the {cap}-byte decompression cap")]
    #[exit(
        DataError,
        slug = "archive_extraction_cap_exceeded",
        summary = "Extraction exceeded the decompressed-size cap"
    )]
    ExtractionCapExceeded { cap: u64 },
    #[error("internal archive error: {0}")]
    #[exit(Failure, slug = "archive_internal", summary = "An internal archive operation failed")]
    Internal(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// Transparent, so a codec failure renders and chains exactly as the codec's own error.
    #[error(transparent)]
    #[exit(delegate)]
    Compression(#[from] crate::compression::error::Error),
    #[error(transparent)]
    #[exit(delegate)]
    File(#[from] crate::error::FileError),
}

impl Error {
    pub fn internal(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Internal(Box::new(error))
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    /// The two `#[error(transparent)]` arms add no prefix and truncate no
    /// chain — the claim the extraction commit made in prose and nothing
    /// checked (B5-9).
    ///
    /// Asserted as equality against the inner error rather than against a
    /// literal: the only end-to-end witness of `Compression` was a
    /// **substring** assertion in the acceptance suite
    /// (`test_package_create_extract.py`), which a prefixed rendering passes,
    /// and `File` had no witness at all. Replacing either `transparent` with
    /// `#[error("archive compression error: {0}")]` left 257 of 257 tests
    /// green.
    #[test]
    fn transparent_arms_are_invisible() {
        // `Compression` → `compression::Error::Io` → `std::io::Error`: the
        // two-level chain the arm exists to carry.
        let inner = crate::compression::error::Error::Io(std::io::Error::other("codec boom"));
        let inner_text = inner.to_string();
        let inner_source = std::error::Error::source(&inner)
            .expect("compression::Error::Io carries the io failure")
            .to_string();
        let wrapped = Error::from(inner);
        assert_eq!(wrapped.to_string(), inner_text, "the Compression arm adds no prefix");
        assert_eq!(
            std::error::Error::source(&wrapped)
                .expect("the Compression arm forwards the inner error's own source")
                .to_string(),
            inner_source,
            "the Compression arm must not truncate the chain"
        );

        // `File` → `FileError`, which deliberately has no `source()` (D-042):
        // forwarding means the wrap has none either, rather than inventing a
        // link to the error it carries.
        let file = crate::error::FileError::new("/tmp/example/link", std::io::Error::other("boom"));
        let file_text = file.to_string();
        assert!(std::error::Error::source(&file).is_none());
        let wrapped = Error::from(file);
        assert_eq!(wrapped.to_string(), file_text, "the File arm adds no prefix");
        assert!(
            std::error::Error::source(&wrapped).is_none(),
            "the File arm forwards FileError's deliberate source-lessness (D-042, ocx#286)"
        );
    }
}
