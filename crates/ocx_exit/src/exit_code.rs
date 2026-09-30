// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Process exit codes shared by all OCX binaries.

/// Process exit codes used by all OCX binaries.
///
/// Values follow BSD `sysexits.h` (64+), clear of shell-reserved (1–2) and signal-derived (128+) codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
#[non_exhaustive]
pub enum ExitCode {
    /// Successful completion.
    Success = 0,
    /// Generic failure, only when no specific code applies.
    Failure = 1,
    /// Bad CLI invocation: unknown flag, wrong argument count, invalid syntax (`EX_USAGE`).
    UsageError = 64,
    /// Malformed input data: bad identifier format, invalid digest (`EX_DATAERR`).
    DataError = 65,
    /// Required resource unavailable, e.g. registry unreachable; unlike [`ExitCode::TempFail`],
    /// rerunning will not change the outcome (`EX_UNAVAILABLE`).
    Unavailable = 69,
    /// I/O failure: filesystem permission denied, disk full, read/write error (`EX_IOERR`).
    IoError = 74,
    /// Transient failure (rate limit, registry connect failure or timeout); the same command
    /// may succeed on retry, which makes automated retry safe here only (`EX_TEMPFAIL`).
    TempFail = 75,
    /// Filesystem `EPERM`, or a forge refusing a push (protected branch, pre-receive hook) where
    /// the refusal is not a capability gate ([`ExitCode::ForgeCapabilityUnavailable`]) (`EX_NOPERM`).
    PermissionDenied = 77,
    /// Bad `config.toml`: parse failure or missing required field (`EX_CONFIG`).
    ConfigError = 78,
    /// Resource not found: package 404, explicit config path absent.
    NotFound = 79,
    /// Authentication failure: registry 401 or 403, missing credentials.
    AuthError = 80,
    /// A deliberate local policy (offline, frozen, the prune safeguard against deleting a
    /// durable tag) refused an operation; loosen the flag, pre-populate the local index, or pass
    /// `--force` to prune. A refusal, not a fault like `Unavailable`.
    PolicyBlocked = 81,
    /// A managed shell-integration block carries user edits and was left untouched
    /// (`ocx self setup` without `--force`).
    DirtyRcBlock = 82,
    /// Rekor unavailable on sign (upload failed) or on verify (SET and TSA absent, SET invalid
    /// against the Rekor key, or lookup 5xx/timeout); distinct from a registry `Unavailable`.
    TransparencyLogUnavailable = 83,
    /// Registry has neither the OCI Referrers API nor a fallback-tag referrers index; discovery
    /// fails rather than returning empty results.
    ReferrersUnsupported = 84,
    /// A key reference (`--key`, a `[[trust.policy]]` signer, managed config) names a recognised
    /// but unimplemented backend (`awskms://`, `gcpkms://`, `azurekms://`, `hashivault://`,
    /// `k8s://`); "not built yet", never a `config_error`.
    UnsupportedKeyBackend = 85,
    /// A reachable forge refuses a write because the instance or target project lacks the
    /// capability the transport needs (job-token push disabled, publisher not allowlisted);
    /// the credential is valid and an administrator, not the caller, must act.
    ForgeCapabilityUnavailable = 86,
    /// The registry does not delete tags (405, 400 `UNSUPPORTED`, or 400 `DIGEST_INVALID` from a
    /// registry that deletes by digest only); an operator must enable deletion or use another
    /// registry, so a retry never helps.
    RegistryDeleteUnsupported = 87,
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
    fn exit_code_forge_capability_unavailable_is_86() {
        // C-001; canonical source is adr_index_claim_command.md, section
        // "Exit codes — full mapping". 85 was taken by UnsupportedKeyBackend,
        // so 86 is the first free slot.
        assert_eq!(ExitCode::ForgeCapabilityUnavailable as u8, 86);
    }

    #[test]
    fn exit_code_registry_delete_unsupported_is_87() {
        // Tool-specific; 86 was taken by ForgeCapabilityUnavailable, so 87 is the next free slot.
        assert_eq!(ExitCode::RegistryDeleteUnsupported as u8, 87);
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
