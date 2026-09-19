// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Exit-code classification for the archive, compression and filesystem-utility error family — the `ocx_util` rung of the
//! ladder, here rather than in that crate because classification is `ocx_cli`'s alone.

use ocx_exit::ExitCode;

use ocx_util::archive::Error as ArchiveError;
use ocx_util::boolean_string::BooleanStringError;
use ocx_util::compression::error::Error as CompressionError;
use ocx_util::error::{Error as UtilError, FileError, SerializationError};
use ocx_util::fs::EmptyOrAbsentError;
use ocx_util::fs::SameFilesystemError;
use ocx_util::fs::SymlinkWalkError;
use ocx_util::fs::path::PathEscapeError;
use ocx_util::singleflight::Error as SingleflightError;

use super::{ClassifyExitCode, downcast_arm};

impl ClassifyExitCode for ArchiveError {
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            Self::Io { .. } => ExitCode::IoError,
            Self::Tar(_)
            | Self::Zip(_)
            | Self::EntryEscape(_)
            | Self::SymlinkEscape { .. }
            | Self::HardLinkEscape { .. }
            | Self::UnsupportedFormat(_)
            | Self::GnuSparseUnsupported(_)
            | Self::ExtractionCapExceeded { .. } => ExitCode::DataError,
            Self::Internal(_) => ExitCode::Failure,
            // E1: `Archive::create_with_compression` / `extract_with_options`
            // drive the codec, so a codec failure now reaches the binary inside
            // `ArchiveError` instead of as `ocx_lib::Error::Compression`. The
            // code is **copied** from that arm — `Self::Compression(e) =>
            // e.classify()` in `exit::classify` — not re-derived: the exit code
            // is the codec's, and the archive wrapper renders nothing of its
            // own (`#[error(transparent)]`).
            Self::Compression(error) => return error.classify(),
            // E1: the extractor's symlink write raised `FileError` and let `?`
            // fold it into `ocx_lib::Error::InternalFile`, whose arm is
            // `Some(ExitCode::IoError)`. The code is **copied** from there by
            // delegating to `FileError`'s own impl below, which carries that
            // same 74 for that same reason.
            Self::File(error) => return error.classify(),
        })
    }
}

impl ClassifyExitCode for CompressionError {
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            Self::UnknownFormat(_) | Self::DecodeOnly(_) => ExitCode::DataError,
            Self::Open { .. } | Self::Create { .. } | Self::Io(_) => ExitCode::IoError,
            Self::EngineInit(_) => ExitCode::Failure,
        })
    }
}

impl ClassifyExitCode for SingleflightError {
    fn classify(&self) -> Option<ExitCode> {
        match self {
            // Failed: return None so the chain walker continues via source()
            // into the SharedError payload. SharedError::source() exposes the
            // leader's typed error directly, letting classify_error downcast to
            // the inner discriminant (e.g. PackageErrorKind::EntrypointCollision
            // → DataError). If the inner error has no specific code the walker
            // falls through to ExitCode::Failure.
            Self::Failed(_) => None,
            // Abandoned has no inner cause — return Failure directly.
            Self::Abandoned => Some(ExitCode::Failure),
            // Transient: retry once in-flight blobs complete.
            Self::Timeout | Self::CapacityExceeded { .. } => Some(ExitCode::TempFail),
        }
    }
}

impl ClassifyExitCode for EmptyOrAbsentError {
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            Self::NotADirectory { .. } | Self::NonEmpty { .. } => ExitCode::UsageError,
            Self::Io { .. } => ExitCode::IoError,
        })
    }
}

impl ClassifyExitCode for PathEscapeError {
    fn classify(&self) -> Option<ExitCode> {
        // A rejected containment path is malformed input data — surfaced when a
        // hostile manifest annotation is re-validated at read time (65). Publish
        // (CLI parse) wraps it in `LayerRefParseError`, which maps to 64.
        Some(ExitCode::DataError)
    }
}

impl ClassifyExitCode for SameFilesystemError {
    fn classify(&self) -> Option<ExitCode> {
        Some(ExitCode::IoError)
    }
}

impl ClassifyExitCode for BooleanStringError {
    /// C-042: the value `ConfigError::InvalidBooleanString` carried, copied
    /// from the arm it replaces rather than re-derived — a mistyped boolean in
    /// `ocx.toml` or an `OCX_*` variable is bad input *data*, 65.
    fn classify(&self) -> Option<ExitCode> {
        Some(ExitCode::DataError)
    }
}

impl ClassifyExitCode for FileError {
    /// C-042: the code `ocx_lib::Error::InternalFile` carries, copied from that
    /// arm rather than re-derived. `utility` now raises this type where it used
    /// to build that variant, and a `FileError` that reaches the binary through
    /// a `utility` signature never passes through `ocx_lib::Error` on the way —
    /// so without an arm of its own the chain walk finds nothing to downcast
    /// and the run exits 1 instead of 74.
    fn classify(&self) -> Option<ExitCode> {
        Some(ExitCode::IoError)
    }
}

impl ClassifyExitCode for SerializationError {
    /// The code `ocx_lib::Error::SerializationFailure` carries, for the same
    /// reason and by the same copy.
    fn classify(&self) -> Option<ExitCode> {
        Some(ExitCode::DataError)
    }
}

impl ClassifyExitCode for UtilError {
    /// The tier's two-arm union delegates to whichever concrete error it
    /// carries. Both arms are `#[error(transparent)]`, so the *rendered* text
    /// was never the problem — the exit code was: `SerdeExt::read_json` returns
    /// this type, and `ocx package create` reads its metadata through it, so a
    /// malformed sidecar exited 1 rather than 65 until this arm existed.
    fn classify(&self) -> Option<ExitCode> {
        match self {
            Self::File(error) => error.classify(),
            Self::Serialization(error) => error.classify(),
        }
    }
}

impl ClassifyExitCode for SymlinkWalkError {
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            Self::Ancestor { .. } => ExitCode::UsageError,
            Self::Io { .. } => ExitCode::IoError,
        })
    }
}

pub(super) fn try_downcast(cause: &(dyn std::error::Error + 'static)) -> Option<ExitCode> {
    downcast_arm!(cause, BooleanStringError);
    downcast_arm!(cause, UtilError);
    downcast_arm!(cause, FileError);
    downcast_arm!(cause, SerializationError);
    downcast_arm!(cause, SymlinkWalkError);
    downcast_arm!(cause, SameFilesystemError);
    downcast_arm!(cause, EmptyOrAbsentError);
    downcast_arm!(cause, ArchiveError);
    downcast_arm!(cause, CompressionError);
    downcast_arm!(cause, SingleflightError);
    downcast_arm!(cause, PathEscapeError);
    None
}

#[cfg(test)]
mod tests {

    /// The chain walk the binary performs, over one error.
    fn classify<E: std::error::Error + 'static>(err: E) -> ExitCode {
        crate::exit::classify_library_error(&err as &(dyn std::error::Error + 'static))
    }

    use super::*;
    use ocx_exit::ExitCode;

    /// C-042: the exit code `ConfigError::InvalidBooleanString` took, asserted
    /// at the type's new home and through the chain walker the binary really
    /// runs — so the `downcast_arm!` registration above is exercised too,
    /// rather than being a line whose green is indistinguishable from never
    /// having run.
    ///
    /// The expected value is read from the pre-split baseline
    /// (`classify_baseline_7adaea62.json`, `Error` /
    /// `crates/ocx_lib/src/config/error.rs:143` → `ExitCode::DataError`),
    /// never off the arm below.
    #[test]
    fn invalid_boolean_string_still_classifies_as_data_error() {
        use ocx_util::boolean_string::BooleanString;

        let error = BooleanString::try_from("maybe").expect_err("`maybe` is not a boolean spelling");
        assert_eq!(error.classify(), Some(ExitCode::DataError));
        assert_eq!(classify(error), ExitCode::DataError);
    }

    // recovered from crate::exit::classify
    #[test]
    fn singleflight_error_classifies_to_correct_exit_codes() {
        use ocx_util::singleflight::{self, SharedError};

        // Failed with a generic io::Error inner cause → Failure(1): the inner
        // error has no specific classification, so the walker falls through.
        let shared = SharedError::for_test(std::io::Error::other("leader boom"));
        let err = singleflight::Error::Failed(shared);
        assert_eq!(
            classify(err),
            ExitCode::Failure,
            "Failed with unclassified inner error must fall through to Failure(1)"
        );

        // Abandoned → Failure(1): leader was dropped without completing.
        let err = singleflight::Error::Abandoned;
        assert_eq!(classify(err), ExitCode::Failure, "Abandoned must map to Failure(1)");

        // Timeout → TempFail(75): transient; retry once in-flight work settles.
        let err = singleflight::Error::Timeout;
        assert_eq!(classify(err), ExitCode::TempFail, "Timeout must map to TempFail(75)");

        // CapacityExceeded → TempFail(75): transient; same rationale.
        let err = singleflight::Error::CapacityExceeded { max: 1024 };
        assert_eq!(
            classify(err),
            ExitCode::TempFail,
            "CapacityExceeded must map to TempFail(75)"
        );
    }
}
