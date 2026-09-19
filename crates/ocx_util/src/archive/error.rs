// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

/// Errors that can occur during archive operations (create, extract, add).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A file I/O error with the associated path.
    #[error("archive I/O error for '{}': {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// A tar archive operation failed.
    #[error("tar error: {0}")]
    Tar(#[source] std::io::Error),
    /// A zip archive operation failed.
    #[error("zip error: {0}")]
    Zip(#[source] zip::result::ZipError),
    /// An archive entry path escapes the extraction root (path traversal).
    #[error("archive entry '{path}' escapes the extraction root", path = .0.display())]
    EntryEscape(PathBuf),
    /// A symlink target escapes the archive or extraction root (path traversal).
    #[error("symlink '{link}' with target '{target}' escapes the root directory", link = .link.display(), target = .target.display())]
    SymlinkEscape { link: PathBuf, target: PathBuf },
    /// A hard-link entry's target does not resolve inside the extraction root.
    #[error("hard link '{link}' target '{target}' does not resolve inside the extraction root", link = .link.display(), target = .target.display())]
    HardLinkEscape { link: PathBuf, target: PathBuf },
    /// The archive format is not supported.
    #[error("unsupported archive format: {0}")]
    UnsupportedFormat(String),
    /// A GNU sparse entry was encountered. Refused because its apparent size is
    /// materialized without consuming tar-stream bytes, bypassing the
    /// decompressed-stream cap (CWE-400); OCX never emits sparse entries.
    #[error("archive entry '{}' uses the unsupported GNU sparse format", .0.display())]
    GnuSparseUnsupported(PathBuf),
    /// Extraction produced more decompressed bytes than the cap allows
    /// (CWE-400). The cap is derived from the compressed input size, mirroring
    /// the registry pull path's decompression-bomb guard.
    #[error("archive extraction exceeded the {cap}-byte decompression cap")]
    ExtractionCapExceeded { cap: u64 },
    /// An unexpected internal error (e.g. task join failure).
    #[error("internal archive error: {0}")]
    Internal(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// A compression or decompression step of an archive operation failed.
    ///
    /// `Archive::create_with_compression` and `Archive::extract_with_options`
    /// drive the codec themselves, so the archive tier's own error has to be
    /// able to carry the codec's — before E1 they both raised the crate-wide
    /// `Error`, which is what made the codec failure expressible at all.
    ///
    /// `#[error(transparent)]` forwards **both** `Display` and `source()`, so a
    /// codec failure renders and chains byte-for-byte as it did when the wide
    /// `Error::Compression` carried it, and `ocx_cli`'s `ArchiveError` arm
    /// delegates to the inner classification for the same reason: the exit code
    /// is the codec's, not the archive tier's.
    #[error(transparent)]
    Compression(#[from] crate::compression::error::Error),
    /// A file operation performed while writing the extracted tree failed —
    /// today, creating an entry's symlink.
    ///
    /// The extractor called `utility::fs::symlink::create` and let `?` convert
    /// its [`FileError`](crate::error::FileError) into the crate-wide
    /// `Error::InternalFile`; E1 removes that destination, so the archive tier
    /// carries the bottom-tier type itself. `#[error(transparent)]` forwards
    /// `Display` and `source()`, and `FileError` renders the *same literal*
    /// `Error::InternalFile` did (pinned by `file_error_display_is_the_moved_literal`),
    /// so neither the text nor the chain moved with the construction site.
    #[error(transparent)]
    File(#[from] crate::error::FileError),
}

impl Error {
    /// Wrap any error as an [`Error::Internal`].
    pub fn internal(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Internal(Box::new(error))
    }
}

/// [`Result`](std::result::Result) over this module's [`Error`] — what every
/// archive entry point returns now that the tier raises its own type instead of
/// the crate-wide one (E1).
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
