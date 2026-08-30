// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Process exit codes shared by all OCX binaries.
//!
//! Numeric values align with BSD `sysexits.h` (EX__BASE = 64) to avoid
//! collisions with shell-reserved codes (1–2) and signal-derived codes
//! (128+). Scripts can `case $?` on these values for structured error
//! handling.

/// Process exit codes used by all OCX binaries.
///
/// Numeric values align with BSD `sysexits.h` (EX__BASE = 64) to avoid
/// collisions with shell-reserved codes (1–2) and signal-derived codes
/// (128+). Scripts can `case $?` on these values for structured error
/// handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
#[non_exhaustive]
pub enum ExitCode {
    /// Successful completion.
    Success = 0,
    /// Generic failure — use only when no specific code applies.
    Failure = 1,
    /// Bad CLI invocation: unknown flag, wrong argument count, invalid syntax.
    /// Mirrors `EX_USAGE` (64).
    UsageError = 64,
    /// Input data malformed: bad identifier format, invalid digest.
    /// Mirrors `EX_DATAERR` (65).
    DataError = 65,
    /// Required resource unavailable: network down, registry unreachable.
    /// Mirrors `EX_UNAVAILABLE` (69).
    ///
    /// Rerunning the same command will not change the outcome — that is what
    /// separates this from [`ExitCode::TempFail`].
    Unavailable = 69,
    /// I/O error: filesystem permission denied, disk full, read/write failure.
    /// Mirrors `EX_IOERR` (74).
    IoError = 74,
    /// Temporary failure that may succeed on retry: rate limit, transient
    /// network, registry connect failure or timeout.
    /// Mirrors `EX_TEMPFAIL` (75).
    ///
    /// The same command may succeed if it is run again — which is what makes
    /// automated retry safe on 75 and unsafe on [`ExitCode::Unavailable`].
    TempFail = 75,
    /// Insufficient permissions: filesystem `EPERM`.
    /// Mirrors `EX_NOPERM` (77).
    PermissionDenied = 77,
    /// Configuration error: bad `config.toml`, missing required field, parse failure.
    /// Mirrors `EX_CONFIG` (78).
    ConfigError = 78,
    /// Resource not found: package 404, explicit config path absent.
    /// OCX-specific; first slot above `EX_CONFIG`.
    NotFound = 79,
    /// Authentication failure: registry 401 or 403, missing credentials.
    /// OCX-specific.
    AuthError = 80,
    /// A deliberate local policy (offline, frozen, ...) refused a
    /// network or resolution operation; loosen the flag or pre-populate the
    /// local index (e.g. `ocx index update`).
    /// Distinct from `Unavailable`: the refusal is deliberate policy, not a fault.
    PolicyBlocked = 81,
    /// A managed shell-integration block carries user edits and was left
    /// untouched (`ocx self setup` without `--force`).
    /// OCX-specific; script-discoverable so a rerun with `--force` is easy.
    DirtyRcBlock = 82,
    /// Rekor transparency log service unavailable.
    ///
    /// Used by the sign path (Rekor upload failure) AND the verify path
    /// (Rekor-required verification cannot complete: SET absent + TSA absent,
    /// Rekor SET verification fails against known Rekor public key, or Rekor
    /// lookup returns 5xx/timeout). OCX-specific; distinct from `Unavailable`
    /// to let operators distinguish "registry down" (retry likely helps) from
    /// "Rekor down" (sign cannot complete, verify of existing v0.3 bundles
    /// fails if Rekor is needed for SET verification).
    TransparencyLogUnavailable = 83,
    /// Registry does not implement the OCI Referrers API and has no fallback-tag
    /// referrers index. The operation cannot proceed — discovery fails hard rather
    /// than silently returning empty results. OCX-specific.
    ReferrersUnsupported = 84,
    /// A key reference named a key backend OCX recognises but does not
    /// implement (`awskms://`, `gcpkms://`, `azurekms://`, `hashivault://`,
    /// `k8s://`). Distinct from `NotFound` (79) and `IoError` (74): the reference
    /// is well-formed and the backend is real, it simply has no implementation
    /// here. OCX-specific; scripts can branch on it before any backend exists.
    ///
    /// Three doors, one code: `--key`, a `key = "…"` signer in a matched
    /// `[[trust.policy]]`, and a managed-config payload carrying one. Each
    /// flattened onto 78 `config_error` at some point, which is the reading
    /// this code exists to prevent — "not built yet" is not "your config is
    /// wrong", and only one of them is fixed by editing the config.
    UnsupportedKeyBackend = 85,
}

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
    fn exit_code_dirty_rc_block_is_82() {
        // OCX-specific; script-discoverable dirty-RC-skip outcome (plan D3).
        // Distinct from ConfigError (78) so a refused managed block is not
        // conflated with a bad-config failure.
        assert_eq!(ExitCode::DirtyRcBlock as u8, 82);
    }

    #[test]
    fn exit_code_transparency_log_unavailable_is_83() {
        // Tool-specific; distinct from Unavailable — Rekor is a separate,
        // non-retryable supply-chain dependency (vs registry transient faults).
        assert_eq!(ExitCode::TransparencyLogUnavailable as u8, 83);
    }

    #[test]
    fn exit_code_referrers_unsupported_is_84() {
        // Tool-specific; registry lacks OCI 1.1 referrers — no fallback.
        assert_eq!(ExitCode::ReferrersUnsupported as u8, 84);
    }

    #[test]
    fn exit_code_unsupported_key_backend_is_85() {
        // Tool-specific; canonical source is design_spec_cosign_parity.md
        // section "Exit codes". 85 is the first free slot above 84.
        assert_eq!(ExitCode::UnsupportedKeyBackend as u8, 85);
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
