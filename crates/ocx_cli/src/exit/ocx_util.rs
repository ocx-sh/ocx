// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Test-only: the classification tests of the `ocx_util` family. Its types declare their own codes with `#[derive(Classify)]`.

use ocx_util::archive::Error as ArchiveError;
use ocx_util::compression::error::Error as CompressionError;
use ocx_util::error::{Error as UtilError, FileError};
use ocx_util::singleflight::Error as SingleflightError;

#[cfg(test)]
mod tests {

    /// The chain walk the binary performs, over one error.
    fn classify<E: std::error::Error + 'static>(err: E) -> ExitCode {
        crate::exit::classify_library_error(&err as &(dyn std::error::Error + 'static))
    }

    use super::*;
    use ocx_exit::{ClassifyExitCode, ExitCode};

    /// C-042: the exit code `ConfigError::InvalidBooleanString` took, asserted
    /// at the type's new home and through the chain walker the binary really
    /// runs — so the `families!` entry is exercised too,
    /// rather than being a line whose green is indistinguishable from never
    /// having run.
    ///
    /// The expected value is read from the pre-split source
    /// (`crates/ocx_lib/src/config/error.rs:143` at `7adaea62` → `ExitCode::DataError`),
    /// never off the arm below.
    #[test]
    fn invalid_boolean_string_still_classifies_as_data_error() {
        use ocx_util::boolean_string::BooleanString;

        let error = BooleanString::try_from("maybe").expect_err("`maybe` is not a boolean spelling");
        assert_eq!(error.classify(), Some(ExitCode::DataError));
        assert_eq!(classify(error), ExitCode::DataError);
    }

    /// Reds on: a delegating or chain-walking arm whose slug names another cause than the code does.
    #[test]
    fn util_details_name_the_cause_that_decides_the_code() {
        use crate::exit::tests::assert_detail;
        use ocx_util::singleflight::SharedError;

        assert_detail(&ArchiveError::EntryEscape("../escape".into()), "archive_entry_escape");
        assert_detail(
            &ArchiveError::internal(std::io::Error::other("boom")),
            "archive_internal",
        );
        let codec = CompressionError::Io(std::io::Error::other("codec boom"));
        assert_detail(&ArchiveError::Compression(codec), "compression_io");
        let denied = FileError::new(
            "/store",
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, "EACCES"),
        );
        assert_detail(&UtilError::File(denied), "file_io");
        let escaped = SharedError::for_test(ArchiveError::EntryEscape("../escape".into()));
        assert_detail(&SingleflightError::Failed(escaped), "archive_entry_escape");
        let unclassified = SharedError::for_test(std::io::Error::other("leader boom"));
        assert_detail(&SingleflightError::Failed(unclassified), "singleflight_failed");
        assert_detail(&SingleflightError::Timeout, "singleflight_timeout");
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
