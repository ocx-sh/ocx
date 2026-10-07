// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Coarse error categories for the structured JSON error envelope.

/// Declares [`ErrorCategory`] with its `ALL` and `summary` from one row per variant.
///
/// A row without its summary does not match, so a category cannot exist unexplained:
///
/// ```compile_fail
/// ocx_exit::error_categories! {
///     Fine => "Has a summary";
///     Bare;
/// }
/// ```
///
/// The same table with every summary present compiles:
///
/// ```
/// ocx_exit::error_categories! {
///     Fine => "Has a summary";
///     Other => "Also has one";
/// }
/// assert_eq!(ErrorCategory::ALL.len(), 2);
/// assert_eq!(ErrorCategory::Other.summary(), "Also has one");
/// ```
// Exported only so the doctests above can reach it; `ErrorCategory` itself is declared once, below.
#[doc(hidden)]
#[macro_export]
macro_rules! error_categories {
    ($($(#[$meta:meta])* $name:ident => $summary:literal;)*) => {
        /// Frozen `error.kind` vocabulary: the snake_case serialization is a wire contract consumers match on.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, ::serde::Serialize)]
        #[serde(rename_all = "snake_case")]
        pub enum ErrorCategory {
            $($(#[$meta])* $name,)*
        }

        impl ErrorCategory {
            /// Every variant, in declaration order: the source of the published `error.kind` table.
            pub const ALL: &'static [ErrorCategory] = &[$(ErrorCategory::$name),*];

            /// One-line user-facing meaning of this category, as the published contract states it.
            pub const fn summary(self) -> &'static str {
                match self {
                    $(ErrorCategory::$name => $summary,)*
                }
            }
        }
    };
}

error_categories! {
    UsageError => "The command line is invalid";
    ConfigError => "Configuration is invalid or incomplete";
    DataError => "Input data is malformed or fails verification";
    AuthError => "Authentication failed or credentials are missing";
    PermissionDenied => "The operation was refused: a permission or a local policy";
    NotFound => "A named package, tag, file or resource does not exist";
    Unavailable => "A required service is unavailable and rerunning will not help";
    TempFail => "A transient failure; the same command may succeed on retry";
    Unsupported => "The operation as requested is not supported or not enabled by this registry, forge or build; retrying will not help";
    IoError => "A filesystem read or write failed";
    Internal => "A failure with no more specific category";
}

#[cfg(test)]
mod tests {
    //! Contract tests for the frozen `error.kind` vocabulary (ADR C-S1-1).
    //!
    //! These tests encode the public contract that `--format json` consumers
    //! pattern-match against. Any change to these tests is a schema bump —
    //! review carefully.
    use super::*;
    use crate::ExitCode;

    #[test]
    fn error_category_serializes_snake_case() {
        // Every frozen variant must serialize to the snake_case form documented
        // in the ADR error_kind inventory.
        let cases = [
            (ErrorCategory::UsageError, "\"usage_error\""),
            (ErrorCategory::ConfigError, "\"config_error\""),
            (ErrorCategory::DataError, "\"data_error\""),
            (ErrorCategory::AuthError, "\"auth_error\""),
            (ErrorCategory::PermissionDenied, "\"permission_denied\""),
            (ErrorCategory::NotFound, "\"not_found\""),
            (ErrorCategory::Unavailable, "\"unavailable\""),
            (ErrorCategory::TempFail, "\"temp_fail\""),
            (ErrorCategory::Unsupported, "\"unsupported\""),
            (ErrorCategory::IoError, "\"io_error\""),
            (ErrorCategory::Internal, "\"internal\""),
        ];
        // What this count pins, exactly: a row deleted from the table above.
        // It cannot force a row for a *new* `ErrorCategory` variant -- `cases`
        // is an array literal, so `len()` is a compile-time constant.
        assert_eq!(
            cases.len(),
            11,
            "a row was removed from the table above; restore it rather than lowering this count"
        );
        for (variant, expected) in cases {
            let actual = serde_json::to_string(&variant).unwrap();
            assert_eq!(actual, expected, "variant {variant:?} serialization mismatch");
        }
    }

    #[test]
    fn error_category_round_trips_82() {
        // Both halves in one function. Totality is the compiler's job -- the match is
        // wildcard-free -- but nothing forces the *right* arm: `ExitCode::Unsupported =>
        // Self::Internal` compiles clean and ships exit 82 with `"kind":"internal"`.
        // Assertion 1 reds on that rewrite. Assertion 2 serializes the variant literal, so a
        // serde rename reds it while the rewrite above leaves it green.
        //
        // "Round trips" is one direction: `ErrorCategory` derives `Serialize` and nothing else.
        assert_eq!(
            ExitCode::Unsupported.category(),
            ErrorCategory::Unsupported,
            "exit 82 must classify as its own category, never a fold into another"
        );
        assert_eq!(
            serde_json::to_string(&ErrorCategory::Unsupported).expect("ErrorCategory serializes"),
            "\"unsupported\"",
            "error document error.kind must be the dedicated category, never \"internal\""
        );
    }

    /// Wildcard-free: a new category is an `E0004` here until its wire value is pinned.
    fn pinned_kind(category: ErrorCategory) -> &'static str {
        match category {
            ErrorCategory::UsageError => "usage_error",
            ErrorCategory::ConfigError => "config_error",
            ErrorCategory::DataError => "data_error",
            ErrorCategory::AuthError => "auth_error",
            ErrorCategory::PermissionDenied => "permission_denied",
            ErrorCategory::NotFound => "not_found",
            ErrorCategory::Unavailable => "unavailable",
            ErrorCategory::TempFail => "temp_fail",
            ErrorCategory::Unsupported => "unsupported",
            ErrorCategory::IoError => "io_error",
            ErrorCategory::Internal => "internal",
        }
    }

    /// `ALL` is the published `error.kind` table, so a category missing from it is one consumers never learn.
    ///
    /// Reds on: a category dropped from `ALL`, listed twice, out of declaration order, or serializing
    /// to other than its pinned value.
    #[test]
    fn all_lists_every_category_once_in_declaration_order() {
        let listed: Vec<&str> = ErrorCategory::ALL
            .iter()
            .map(|category| pinned_kind(*category))
            .collect();
        assert_eq!(
            listed,
            [
                "usage_error",
                "config_error",
                "data_error",
                "auth_error",
                "permission_denied",
                "not_found",
                "unavailable",
                "temp_fail",
                "unsupported",
                "io_error",
                "internal",
            ],
            "ErrorCategory::ALL must list every variant exactly once, in declaration order"
        );
        for category in ErrorCategory::ALL {
            assert_eq!(
                serde_json::to_value(category).expect("ErrorCategory serializes"),
                pinned_kind(*category),
                "{category:?}"
            );
        }
    }

    /// Every category exists to be some exit code's `error.kind`, so that image is what `ALL` must list.
    ///
    /// Reds on: a category dropped from or duplicated in `ALL`, and a category no exit code reports under.
    #[test]
    fn all_lists_exactly_the_categories_exit_codes_report_under() {
        let mut reported: Vec<ErrorCategory> = Vec::new();
        for code in ExitCode::ALL {
            if !reported.contains(&code.category()) {
                reported.push(code.category());
            }
        }
        assert!(!reported.is_empty(), "nothing to check: ExitCode::ALL is empty");
        for category in &reported {
            assert_eq!(
                ErrorCategory::ALL.iter().filter(|listed| *listed == category).count(),
                1,
                "{category:?} must appear in ErrorCategory::ALL exactly once"
            );
        }
        assert_eq!(
            ErrorCategory::ALL.len(),
            reported.len(),
            "ALL lists a category no exit code uses"
        );
    }

    /// Each summary is copied verbatim into the published schema, so it is one plain line.
    #[test]
    fn every_category_has_a_one_line_summary() {
        assert!(
            !ErrorCategory::ALL.is_empty(),
            "nothing to check: ErrorCategory::ALL is empty"
        );
        for category in ErrorCategory::ALL {
            let summary = category.summary();
            assert!(!summary.trim().is_empty(), "{category:?} has no summary");
            assert!(!summary.contains('\n'), "{category:?} summary spans lines: {summary}");
            assert!(
                !summary.ends_with('.'),
                "{category:?} summary ends with a period: {summary}"
            );
            assert!(
                !summary.contains('[') && !summary.contains("::"),
                "{category:?} summary carries a doc link or code path: {summary}"
            );
        }
    }

    #[test]
    fn error_category_total_over_exit_codes() {
        // Totality itself is the compiler's job: `category` is an in-crate
        // match with no wildcard, so an unclassified `ExitCode` variant is an
        // E0004 build failure, not a silent `internal`.
        //
        // What this table adds is the *mapping*, which the compiler cannot check:
        // an arm rewritten to the wrong category still compiles. Two rows cannot
        // discriminate and are listed for completeness only — `Success` and
        // `Failure` map to `Internal` deliberately.
        let cases = [
            (ExitCode::Success, ErrorCategory::Internal),
            (ExitCode::Failure, ErrorCategory::Internal),
            (ExitCode::UsageError, ErrorCategory::UsageError),
            (ExitCode::DataError, ErrorCategory::DataError),
            (ExitCode::Unavailable, ErrorCategory::Unavailable),
            (ExitCode::IoError, ErrorCategory::IoError),
            (ExitCode::TempFail, ErrorCategory::TempFail),
            (ExitCode::PermissionDenied, ErrorCategory::PermissionDenied),
            (ExitCode::ConfigError, ErrorCategory::ConfigError),
            (ExitCode::NotFound, ErrorCategory::NotFound),
            (ExitCode::AuthError, ErrorCategory::AuthError),
            (ExitCode::PolicyBlocked, ErrorCategory::PermissionDenied),
            (ExitCode::Unsupported, ErrorCategory::Unsupported),
        ];
        // What this count pins, exactly: a row deleted from the table above.
        // It cannot force a row for a *new* `ExitCode` variant -- `cases` is an
        // array literal, so `len()` is a compile-time constant. Forcing that is
        // the wildcard-free match's job, not this assertion's.
        assert_eq!(
            cases.len(),
            13,
            "a row was removed from the table above; restore it rather than lowering this count"
        );
        for (code, expected) in cases {
            assert_eq!(
                code.category(),
                expected,
                "exit code {} lost its arm in category()",
                code as u8,
            );
        }
    }
}
