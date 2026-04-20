// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Structured JSON error envelope for `--format json` error output.
//!
//! Per ADR §C-S1-1, the envelope shape is frozen at `schema_version = 1`
//! for Slice 1 and treated as a stable public contract. Root-level keys are
//! strictly `schema_version`, `command`, `exit_code`, and `error` (error path)
//! or `schema_version`, `command`, `exit_code`, `data` (success path).
//!
//! Shape:
//!
//! ```json
//! {
//!   "schema_version": 1,
//!   "command": "package sign",
//!   "exit_code": 80,
//!   "error": {
//!     "kind": "auth_error",
//!     "detail": "oidc_token_rejected",
//!     "message": "Fulcio rejected OIDC token: issuer not in trust root",
//!     "remediation": "Verify --certificate-oidc-issuer matches a Fulcio-trusted issuer",
//!     "context": {
//!       "identifier": "ocx.sh/cmake:3.28",
//!       "bundle_digest": null,
//!       "rekor_url": "https://rekor.sigstore.dev"
//!     }
//!   }
//! }
//! ```
//!
//! Phase 1 stub — body of [`render_error_envelope`] uses `unimplemented!()`.

// Stubs are consumed by `main.rs` in Phase 5 once error-routing dispatch
// branches on `--format json`. Silence `dead_code` until then.
#![allow(dead_code)]

use serde::Serialize;
use std::collections::BTreeMap;

/// Schema version for the JSON envelope. Bump on any breaking change.
///
/// Freeze per C-S1-1: version 1 is the slice-1 contract. Additive fields
/// (new keys) do not bump; shape changes (rename, remove, re-nest) do. Adding
/// a new [`ErrorCategory`] variant is a `schema_version` bump per ADR rules.
pub const ENVELOPE_SCHEMA_VERSION: u32 = 1;

/// Frozen error-category set (ADR C-S1-1). Matches `error.kind` values listed
/// in the ADR's `error_kind` inventory — the serialized lowercase form is
/// the stable contract consumers pattern-match on.
#[derive(Debug, Clone, Copy, Serialize)]
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
    RekorUnavailable,
    ReferrersUnsupported,
    IoError,
    Internal,
}

/// Error-branch JSON envelope.
///
/// Top-level shape per ADR C-S1-1 frozen v1 contract: `schema_version`,
/// `command`, `exit_code`, `error`. `success` is NOT present — consumers
/// branch on whether the `error` or `data` key is present.
#[derive(Debug, Serialize)]
pub struct ErrorEnvelope<'a> {
    /// Envelope schema version. Always [`ENVELOPE_SCHEMA_VERSION`] for v1.
    pub schema_version: u32,
    /// Canonical command string (e.g., `"package sign"`, `"verify"`).
    pub command: &'a str,
    /// Process exit code that will be returned (numeric value of `ExitCode`).
    pub exit_code: u8,
    /// Structured error payload.
    pub error: EnvelopeError<'a>,
}

/// The `error` object inside [`ErrorEnvelope`].
#[derive(Debug, Serialize)]
pub struct EnvelopeError<'a> {
    /// Coarse human-readable category. Frozen v1 set — see [`ErrorCategory`].
    pub kind: ErrorCategory,
    /// Fine-grained snake_case variant name for programmatic matching
    /// (e.g., `"oidc_token_rejected"`). Optional.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<&'a str>,
    /// Full user-facing message (the outermost `Display` of the error chain).
    pub message: String,
    /// Optional remediation hint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
    /// Structured context — identifier, digests, URLs. Values are
    /// `serde_json::Value` so null and numeric fields serialize faithfully
    /// (the ADR example shows `"bundle_digest": null`).
    ///
    /// Stable key ordering via `BTreeMap` — tests compare byte-for-byte
    /// without sorting. Always emitted (may be an empty object).
    pub context: BTreeMap<&'static str, serde_json::Value>,
}

/// Success-branch JSON envelope. Mirrors [`ErrorEnvelope`] at the top level
/// (`schema_version`, `command`, `exit_code`) with `data` replacing `error`.
#[derive(Debug, Serialize)]
pub struct SuccessEnvelope<'a, T: Serialize> {
    pub schema_version: u32,
    pub command: &'a str,
    pub exit_code: u8,
    pub data: &'a T,
}

impl<'a, T: Serialize> SuccessEnvelope<'a, T> {
    /// Wrap `data` in a success envelope.
    pub fn new(command: &'a str, data: &'a T) -> Self {
        Self {
            schema_version: ENVELOPE_SCHEMA_VERSION,
            command,
            exit_code: 0,
            data,
        }
    }
}

/// Render an `anyhow::Error` as a JSON error envelope on stderr (or stdout
/// if `--format json` directs success payloads there; callers decide).
///
/// Walks the `anyhow::Error::chain()` to pick the most specific known kind,
/// classifies the exit code via [`ocx_lib::cli::classify_error`], and
/// collects identifier context. The `message` is `{err:#}` (full chain),
/// matching the plain-format output.
///
/// Phase 1 stub.
pub fn render_error_envelope(_command: &str, _err: &anyhow::Error) -> anyhow::Result<String> {
    unimplemented!(
        "render_error_envelope — Phase 5 classifies the error chain, collects \
         identifier context, and returns the serialized envelope"
    )
}

#[cfg(test)]
mod tests {
    //! Contract tests for the frozen v1 JSON envelope shape (ADR C-S1-1).
    //!
    //! These tests encode the public contract that `--format json` consumers
    //! pattern-match against. Any change to these tests is a v1 → v2 schema
    //! bump — review carefully.
    use super::*;
    use serde::Serialize;

    #[test]
    fn schema_version_is_one() {
        assert_eq!(ENVELOPE_SCHEMA_VERSION, 1);
    }

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
            (ErrorCategory::RekorUnavailable, "\"rekor_unavailable\""),
            (ErrorCategory::ReferrersUnsupported, "\"referrers_unsupported\""),
            (ErrorCategory::IoError, "\"io_error\""),
            (ErrorCategory::Internal, "\"internal\""),
        ];
        for (variant, expected) in cases {
            let actual = serde_json::to_string(&variant).unwrap();
            assert_eq!(actual, expected, "variant {variant:?} serialization mismatch");
        }
    }

    #[test]
    fn error_envelope_golden_shape() {
        // Golden byte-for-byte JSON matching the ADR §C-S1-1 example.
        let mut context = BTreeMap::new();
        context.insert("identifier", serde_json::Value::String("ocx.sh/cmake:3.28".into()));
        context.insert("bundle_digest", serde_json::Value::Null);
        context.insert(
            "rekor_url",
            serde_json::Value::String("https://rekor.sigstore.dev".into()),
        );
        let envelope = ErrorEnvelope {
            schema_version: ENVELOPE_SCHEMA_VERSION,
            command: "package sign",
            exit_code: 80,
            error: EnvelopeError {
                kind: ErrorCategory::AuthError,
                detail: Some("oidc_token_rejected"),
                message: "Fulcio rejected OIDC token: issuer not in trust root".into(),
                remediation: Some("Verify --certificate-oidc-issuer matches a Fulcio-trusted issuer".into()),
                context,
            },
        };
        let actual = serde_json::to_string(&envelope).unwrap();
        // Keys land in the declared struct order at the top level, and BTreeMap
        // sorts context keys lexicographically.
        let expected = concat!(
            r#"{"schema_version":1,"command":"package sign","exit_code":80,"#,
            r#""error":{"kind":"auth_error","detail":"oidc_token_rejected","#,
            r#""message":"Fulcio rejected OIDC token: issuer not in trust root","#,
            r#""remediation":"Verify --certificate-oidc-issuer matches a Fulcio-trusted issuer","#,
            r#""context":{"bundle_digest":null,"identifier":"ocx.sh/cmake:3.28","#,
            r#""rekor_url":"https://rekor.sigstore.dev"}}}"#,
        );
        assert_eq!(actual, expected);
    }

    #[test]
    fn error_envelope_omits_none_detail_and_remediation() {
        // Optional fields absent → not emitted (skip_serializing_if).
        let envelope = ErrorEnvelope {
            schema_version: ENVELOPE_SCHEMA_VERSION,
            command: "verify",
            exit_code: 79,
            error: EnvelopeError {
                kind: ErrorCategory::NotFound,
                detail: None,
                message: "no signatures found for package".into(),
                remediation: None,
                context: BTreeMap::new(),
            },
        };
        let actual = serde_json::to_string(&envelope).unwrap();
        // detail + remediation must not appear; context is always emitted (may be {}).
        assert!(!actual.contains("\"detail\""), "detail should be skipped: {actual}");
        assert!(
            !actual.contains("\"remediation\""),
            "remediation should be skipped: {actual}"
        );
        assert!(
            actual.contains("\"context\":{}"),
            "empty context should be `{{}}`: {actual}"
        );
        assert!(actual.contains("\"kind\":\"not_found\""));
    }

    #[test]
    fn error_envelope_context_keys_are_stably_ordered() {
        // BTreeMap orders keys lexicographically — consumers can rely on this for
        // byte-for-byte diffing across runs.
        let mut context = BTreeMap::new();
        context.insert("zeta", serde_json::Value::String("z".into()));
        context.insert("alpha", serde_json::Value::String("a".into()));
        context.insert("mike", serde_json::Value::String("m".into()));
        let envelope = ErrorEnvelope {
            schema_version: ENVELOPE_SCHEMA_VERSION,
            command: "verify",
            exit_code: 1,
            error: EnvelopeError {
                kind: ErrorCategory::Internal,
                detail: None,
                message: "x".into(),
                remediation: None,
                context,
            },
        };
        let actual = serde_json::to_string(&envelope).unwrap();
        // Lexicographic: alpha, mike, zeta.
        let alpha_idx = actual.find("\"alpha\"").expect("alpha present");
        let mike_idx = actual.find("\"mike\"").expect("mike present");
        let zeta_idx = actual.find("\"zeta\"").expect("zeta present");
        assert!(alpha_idx < mike_idx && mike_idx < zeta_idx, "bad order: {actual}");
    }

    #[test]
    fn success_envelope_golden_shape() {
        #[derive(Serialize)]
        struct SignData {
            subject_digest: &'static str,
            bundle_digest: &'static str,
        }
        let data = SignData {
            subject_digest: "sha256:aaaa",
            bundle_digest: "sha256:bbbb",
        };
        let envelope = SuccessEnvelope::new("package sign", &data);
        let actual = serde_json::to_string(&envelope).unwrap();
        // Success branch: `data`, never `error`. schema_version and exit_code (0) are present.
        let expected = concat!(
            r#"{"schema_version":1,"command":"package sign","exit_code":0,"#,
            r#""data":{"subject_digest":"sha256:aaaa","bundle_digest":"sha256:bbbb"}}"#,
        );
        assert_eq!(actual, expected);
    }

    #[test]
    fn success_envelope_sets_exit_code_zero() {
        #[derive(Serialize)]
        struct Empty {}
        let envelope = SuccessEnvelope::new("verify", &Empty {});
        assert_eq!(envelope.exit_code, 0);
        assert_eq!(envelope.schema_version, ENVELOPE_SCHEMA_VERSION);
        assert_eq!(envelope.command, "verify");
    }

    #[test]
    #[should_panic(expected = "not implemented")]
    fn render_error_envelope_is_phase_1_stub() {
        // Phase 5 will implement this; the test proves the stub panics today
        // and flips to pass automatically once Phase 5 fills the body.
        let err = anyhow::anyhow!("synthetic error for stub probe");
        let _ = render_error_envelope("package sign", &err);
    }
}
