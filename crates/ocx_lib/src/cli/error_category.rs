// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Coarse error categories for the structured JSON error envelope.
//!
//! The serialized snake_case form of [`ErrorCategory`] is the envelope's
//! `error.kind` value — a frozen wire contract consumers pattern-match on
//! (ADR §C-S1-1).
//!
//! The type lives beside [`ExitCode`] rather than in the CLI crate for one
//! reason: `#[non_exhaustive]` binds only *downstream* crates, so an in-crate
//! match over [`ExitCode`] can be exhaustive with no wildcard. That makes the
//! compiler, rather than a hand-maintained table, the thing that forces every
//! exit code to be classified.

use crate::cli::ExitCode;
use serde::Serialize;

/// Frozen error-category set (ADR C-S1-1). Matches `error.kind` values listed
/// in the ADR's `error_kind` inventory — the serialized lowercase form is
/// the stable contract consumers pattern-match on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCategory {
    UsageError,
    ConfigError,
    DataError,
    AuthError,
    PermissionDenied,
    NotFound,
    Unavailable,
    TempFail,
    TransparencyLogUnavailable,
    ReferrersUnsupported,
    UnsupportedKeyBackend,
    ForgeCapabilityUnavailable,
    IoError,
    Internal,
}

impl ErrorCategory {
    /// Total function mapping every [`ExitCode`] to an [`ErrorCategory`].
    ///
    /// The match is **exhaustive with no wildcard**. [`ExitCode`] is
    /// `#[non_exhaustive]`, but that binds downstream crates only, so here —
    /// in the crate that defines it — a new variant is a compile error until
    /// it is classified. That is the whole guard: the former cross-crate form
    /// needed a `_ => Internal` arm, under which a new exit code compiled
    /// clean, passed clippy, and silently serialized as `internal`.
    ///
    /// Success codes (`Success = 0`, `Failure = 1`) are nonsensical for an error
    /// envelope and map to [`Self::Internal`] as a fail-safe: emitting an error
    /// envelope on exit-code 0 would itself be a bug, and an envelope with
    /// `kind=internal` is a readable trap. `PolicyBlocked` maps to
    /// `PermissionDenied` — it is a deliberate policy rejection, not a network fault.
    pub fn from_exit_code(code: ExitCode) -> Self {
        match code {
            ExitCode::Success | ExitCode::Failure => Self::Internal,
            ExitCode::UsageError => Self::UsageError,
            ExitCode::DataError => Self::DataError,
            ExitCode::Unavailable => Self::Unavailable,
            ExitCode::IoError => Self::IoError,
            ExitCode::TempFail => Self::TempFail,
            ExitCode::PermissionDenied => Self::PermissionDenied,
            ExitCode::ConfigError => Self::ConfigError,
            ExitCode::NotFound => Self::NotFound,
            ExitCode::AuthError => Self::AuthError,
            ExitCode::PolicyBlocked => Self::PermissionDenied,
            // Same genus as `PolicyBlocked`: a deliberate refusal to act (the
            // managed RC block carries user edits and `--force` was absent), not
            // a malformed config. `ConfigError` would erase that distinction,
            // which exit 82 exists to draw.
            ExitCode::DirtyRcBlock => Self::PermissionDenied,
            ExitCode::TransparencyLogUnavailable => Self::TransparencyLogUnavailable,
            ExitCode::ReferrersUnsupported => Self::ReferrersUnsupported,
            // Its own category rather than a fold into `UsageError`: same genus
            // as 84 (`ReferrersUnsupported`) -- a capability is absent, and the
            // invocation that named it was well-formed.
            ExitCode::UnsupportedKeyBackend => Self::UnsupportedKeyBackend,
            ExitCode::ForgeCapabilityUnavailable => Self::ForgeCapabilityUnavailable,
        }
    }
}

#[cfg(test)]
mod tests {
    //! Contract tests for the frozen `error.kind` vocabulary (ADR C-S1-1).
    //!
    //! These tests encode the public contract that `--format json` consumers
    //! pattern-match against. Any change to these tests is a schema bump —
    //! review carefully.
    use super::*;

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
            (
                ErrorCategory::TransparencyLogUnavailable,
                "\"transparency_log_unavailable\"",
            ),
            (ErrorCategory::ReferrersUnsupported, "\"referrers_unsupported\""),
            (ErrorCategory::UnsupportedKeyBackend, "\"unsupported_key_backend\""),
            (
                ErrorCategory::ForgeCapabilityUnavailable,
                "\"forge_capability_unavailable\"",
            ),
            (ErrorCategory::IoError, "\"io_error\""),
            (ErrorCategory::Internal, "\"internal\""),
        ];
        // What this count pins, exactly: a row deleted from the table above.
        // It cannot force a row for a *new* `ErrorCategory` variant -- `cases`
        // is an array literal, so `len()` is a compile-time constant.
        assert_eq!(
            cases.len(),
            14,
            "a row was removed from the table above; restore it rather than lowering this count"
        );
        for (variant, expected) in cases {
            let actual = serde_json::to_string(&variant).unwrap();
            assert_eq!(actual, expected, "variant {variant:?} serialization mismatch");
        }
    }

    #[test]
    fn error_category_round_trips_86() {
        // C-002, both halves in one function.
        //
        // Totality is the compiler's job -- the match is wildcard-free, so 86
        // cannot land without *an* arm. But nothing forces it to be the *right*
        // arm: `ExitCode::ForgeCapabilityUnavailable => Self::Internal` compiles
        // clean, passes clippy, and ships exit 86 with `"kind":"internal"`,
        // which is the precise defect the wildcard-free match exists to
        // prevent. Assertion 1 is what reds on that rewrite. Assertion 2 does
        // not read the binding at all -- it serializes the variant literal, so
        // a serde rename reds it while the rewrite above leaves it green.
        // Neither assertion stands in for the other.
        //
        // "Round trips" is one direction. `ErrorCategory` derives `Serialize`
        // and nothing else -- there is no `Deserialize` and no `FromStr` for it
        // anywhere in the workspace, and none is added to widen this test.
        let category = ErrorCategory::from_exit_code(ExitCode::ForgeCapabilityUnavailable);
        assert_eq!(
            category,
            ErrorCategory::ForgeCapabilityUnavailable,
            "exit 86 must classify as its own category, never a fold into another"
        );
        assert_eq!(
            serde_json::to_string(&ErrorCategory::ForgeCapabilityUnavailable).expect("ErrorCategory serializes"),
            "\"forge_capability_unavailable\"",
            "envelope error.kind must be the dedicated category, never \"internal\""
        );
    }

    #[test]
    fn error_category_total_over_exit_codes() {
        // Totality itself is the compiler's job: `from_exit_code` is an in-crate
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
            (ExitCode::DirtyRcBlock, ErrorCategory::PermissionDenied),
            (
                ExitCode::TransparencyLogUnavailable,
                ErrorCategory::TransparencyLogUnavailable,
            ),
            (ExitCode::ReferrersUnsupported, ErrorCategory::ReferrersUnsupported),
            (ExitCode::UnsupportedKeyBackend, ErrorCategory::UnsupportedKeyBackend),
            (
                ExitCode::ForgeCapabilityUnavailable,
                ErrorCategory::ForgeCapabilityUnavailable,
            ),
        ];
        // What this count pins, exactly: a row deleted from the table above.
        // It cannot force a row for a *new* `ExitCode` variant -- `cases` is an
        // array literal, so `len()` is a compile-time constant. Forcing that is
        // the wildcard-free match's job, not this assertion's.
        assert_eq!(
            cases.len(),
            17,
            "a row was removed from the table above; restore it rather than lowering this count"
        );
        for (code, expected) in cases {
            assert_eq!(
                ErrorCategory::from_exit_code(code),
                expected,
                "exit code {} lost its arm in from_exit_code",
                code as u8,
            );
        }
    }
}
