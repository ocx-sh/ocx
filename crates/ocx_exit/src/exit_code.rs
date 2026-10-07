// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Process exit codes shared by all OCX binaries.

use crate::error_category::ErrorCategory;

/// Declares [`ExitCode`] with its `ALL`, `summary` and `category` from one row per variant.
macro_rules! exit_codes {
    ($($(#[$meta:meta])* $name:ident = $value:literal => $category:ident, $summary:literal;)*) => {
        /// Process exit codes used by all OCX binaries.
        ///
        /// Values follow BSD `sysexits.h` (64+), clear of shell-reserved (1–2) and signal-derived (128+) codes.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        #[repr(u8)]
        #[non_exhaustive]
        pub enum ExitCode {
            $($(#[$meta])* $name = $value,)*
        }

        impl ExitCode {
            /// Every variant, in declaration order: the source of the published exit-code table.
            pub const ALL: &'static [ExitCode] = &[$(ExitCode::$name),*];

            /// One-line user-facing meaning of this code, as the published contract states it.
            pub const fn summary(self) -> &'static str {
                match self {
                    $(ExitCode::$name => $summary,)*
                }
            }

            /// The `error.kind` this code is reported under; `Success` and `Failure` map to `Internal`.
            pub const fn category(self) -> ErrorCategory {
                match self {
                    $(ExitCode::$name => ErrorCategory::$category,)*
                }
            }
        }
    };
}

exit_codes! {
    /// Successful completion.
    Success = 0 => Internal, "The command succeeded";
    /// Generic failure, only when no specific code applies.
    Failure = 1 => Internal, "The command failed and no more specific code applies";
    /// Bad CLI invocation: unknown flag, wrong argument count, invalid syntax (`EX_USAGE`).
    UsageError = 64 => UsageError, "The command line is invalid: unknown flag, wrong argument count or bad syntax";
    /// Malformed input data: bad identifier format, invalid digest (`EX_DATAERR`).
    DataError = 65 => DataError, "Input data is malformed or fails verification";
    /// Required resource unavailable, e.g. registry unreachable; unlike [`ExitCode::TempFail`],
    /// rerunning will not change the outcome (`EX_UNAVAILABLE`).
    Unavailable = 69 => Unavailable, "A required service is unavailable and rerunning will not help";
    /// I/O failure: filesystem permission denied, disk full, read/write error (`EX_IOERR`).
    IoError = 74 => IoError, "A filesystem read or write failed";
    /// Transient failure (rate limit, registry connect failure or timeout); the same command
    /// may succeed on retry, which makes automated retry safe here only (`EX_TEMPFAIL`).
    TempFail = 75 => TempFail, "A transient failure; the same command may succeed on retry";
    /// Filesystem `EPERM`, or a forge refusing a push (protected branch, pre-receive hook,
    /// publisher not on the job-token allowlist) where the refusal is not a missing capability
    /// ([`ExitCode::Unsupported`]) (`EX_NOPERM`).
    PermissionDenied = 77 => PermissionDenied, "The operation was refused for lack of permission";
    /// Bad `config.toml`: parse failure or missing required field (`EX_CONFIG`).
    ConfigError = 78 => ConfigError, "Configuration is invalid or incomplete";
    /// Resource not found: package 404, explicit config path absent.
    NotFound = 79 => NotFound, "A named package, tag, file or resource does not exist";
    /// Authentication failure: registry 401 or 403, missing credentials.
    AuthError = 80 => AuthError, "Authentication failed or credentials are missing";
    /// A deliberate local policy or safeguard refused an operation: offline, frozen, the prune
    /// safeguard against deleting a durable tag, a managed shell-profile block carrying user edits.
    /// The caller's next action is to loosen the flag, pre-populate the local index, or pass
    /// `--force`; a refusal, not a fault like `Unavailable`.
    PolicyBlocked = 81 => PermissionDenied, "A local policy or safeguard refused the operation; loosen the flag or pass --force";
    /// A valid invocation the registry, forge or build cannot honour: the capability is absent or
    /// not enabled, and retrying never helps. The caller uses another registry, forge or build, or
    /// has its operator enable it. Which capability is the `error.detail` slug's job
    /// (`adr_exit_code_taxonomy.md`).
    Unsupported = 82 => Unsupported, "The operation as requested is not supported or not enabled by this registry, forge or build; retrying will not help";
}

/// Exit numbers no [`ExitCode`] may ever take again: once a consumer keyed on a value, reusing it
/// changes the meaning under a script. Each was a per-feature code collapsed into the next-action
/// codes by `adr_exit_code_taxonomy.md`.
pub const RETIRED: &[u8] = &[83, 84, 85, 86, 87];

impl From<ExitCode> for std::process::ExitCode {
    fn from(value: ExitCode) -> Self {
        std::process::ExitCode::from(value as u8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Test 3.1.1: ExitCode numeric conversion ──────────────────────────────
    // Each assertion quotes the canonical numeric value from research_exit_codes.md,
    // NOT derived from reading the enum definition. If the enum value ever drifts,
    // the test catches it.

    #[test]
    fn exit_code_success_is_zero() {
        assert_eq!(ExitCode::Success as u8, 0);
    }

    #[test]
    fn exit_code_failure_is_one() {
        assert_eq!(ExitCode::Failure as u8, 1);
    }

    #[test]
    fn exit_code_usage_error_is_64() {
        // EX_USAGE from sysexits.h
        assert_eq!(ExitCode::UsageError as u8, 64);
    }

    #[test]
    fn exit_code_data_error_is_65() {
        // EX_DATAERR from sysexits.h
        assert_eq!(ExitCode::DataError as u8, 65);
    }

    #[test]
    fn exit_code_unavailable_is_69() {
        // EX_UNAVAILABLE from sysexits.h
        assert_eq!(ExitCode::Unavailable as u8, 69);
    }

    #[test]
    fn exit_code_io_error_is_74() {
        // EX_IOERR from sysexits.h
        assert_eq!(ExitCode::IoError as u8, 74);
    }

    #[test]
    fn exit_code_temp_fail_is_75() {
        // EX_TEMPFAIL from sysexits.h
        assert_eq!(ExitCode::TempFail as u8, 75);
    }

    #[test]
    fn exit_code_permission_denied_is_77() {
        // EX_NOPERM from sysexits.h
        assert_eq!(ExitCode::PermissionDenied as u8, 77);
    }

    #[test]
    fn exit_code_config_error_is_78() {
        // EX_CONFIG from sysexits.h
        assert_eq!(ExitCode::ConfigError as u8, 78);
    }

    #[test]
    fn exit_code_not_found_is_79() {
        // OCX-specific; first slot above EX_CONFIG
        assert_eq!(ExitCode::NotFound as u8, 79);
    }

    #[test]
    fn exit_code_auth_error_is_80() {
        // OCX-specific
        assert_eq!(ExitCode::AuthError as u8, 80);
    }

    #[test]
    fn exit_code_policy_blocked_is_81() {
        // OCX-specific; distinct from Unavailable (deliberate policy, not a fault).
        // Shared by offline and frozen no-resolve policies; value stays 81 so
        // existing scripts/docs keyed on 81 remain valid.
        assert_eq!(ExitCode::PolicyBlocked as u8, 81);
    }

    #[test]
    fn exit_code_unsupported_is_82() {
        // OCX-specific; a capability the registry, forge or build lacks (adr_exit_code_taxonomy.md).
        assert_eq!(ExitCode::Unsupported as u8, 82);
    }

    /// A retired number keeps no meaning a consumer could still key on, so no variant may take it back.
    ///
    /// Reds on: any `ExitCode` declared with a value listed in [`RETIRED`].
    #[test]
    fn retired_numbers_are_never_reused() {
        assert!(!ExitCode::ALL.is_empty(), "nothing to check: ExitCode::ALL is empty");
        for code in ExitCode::ALL {
            assert!(
                !RETIRED.contains(&(*code as u8)),
                "{code:?} reuses retired exit number {}",
                *code as u8
            );
        }
    }

    /// Wildcard-free: a new variant is an `E0004` here until its value is pinned.
    fn pinned_value(code: ExitCode) -> u8 {
        match code {
            ExitCode::Success => 0,
            ExitCode::Failure => 1,
            ExitCode::UsageError => 64,
            ExitCode::DataError => 65,
            ExitCode::Unavailable => 69,
            ExitCode::IoError => 74,
            ExitCode::TempFail => 75,
            ExitCode::PermissionDenied => 77,
            ExitCode::ConfigError => 78,
            ExitCode::NotFound => 79,
            ExitCode::AuthError => 80,
            ExitCode::PolicyBlocked => 81,
            ExitCode::Unsupported => 82,
        }
    }

    /// `ALL` is the published exit-code table, so a code missing from it is a code consumers never learn.
    ///
    /// Reds on: a variant dropped from `ALL`, listed twice, or out of declaration order.
    #[test]
    fn all_lists_every_code_once_in_declaration_order() {
        let listed: Vec<u8> = ExitCode::ALL.iter().map(|code| pinned_value(*code)).collect();
        assert_eq!(
            listed,
            [0, 1, 64, 65, 69, 74, 75, 77, 78, 79, 80, 81, 82],
            "ExitCode::ALL must list every variant exactly once, in declaration order"
        );
        for code in ExitCode::ALL {
            assert_eq!(*code as u8, pinned_value(*code), "{code:?}");
        }
    }

    /// Each summary is copied verbatim into the published schema, so it is one plain line.
    #[test]
    fn every_code_has_a_one_line_summary() {
        assert!(!ExitCode::ALL.is_empty(), "nothing to check: ExitCode::ALL is empty");
        for code in ExitCode::ALL {
            let summary = code.summary();
            assert!(!summary.trim().is_empty(), "{code:?} has no summary");
            assert!(!summary.contains('\n'), "{code:?} summary spans lines: {summary}");
            assert!(
                !summary.ends_with('.'),
                "{code:?} summary ends with a period: {summary}"
            );
            assert!(
                !summary.contains('[') && !summary.contains("::"),
                "{code:?} summary carries a doc link or code path: {summary}"
            );
        }
    }

    #[test]
    fn exit_code_converts_to_process_exit_code() {
        // Smoke test: proves the From impl compiles and is callable.
        // Correctness of the numeric value is covered by the `as u8` tests above.
        let _: std::process::ExitCode = ExitCode::Success.into();
        let _: std::process::ExitCode = ExitCode::Failure.into();
        let _: std::process::ExitCode = ExitCode::ConfigError.into();
    }
}
