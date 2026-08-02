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
//!     "context": {
//!       "identifier": "ocx.sh/cmake:3.28",
//!       "bundle_digest": null,
//!       "rekor_url": "https://rekor.sigstore.dev"
//!     }
//!   }
//! }
//! ```
//!
//! The `remediation` key is **reserved** in the v1 shape but not currently
//! emitted: [`render_error_envelope`] always leaves it `None`, so it is omitted
//! from real output. Consumers must treat it as optional.

use ocx_lib::cli::{ClassifyErrorKind, ExitCode, classify_error};
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

impl ErrorCategory {
    /// Total function mapping every [`ExitCode`] to an [`ErrorCategory`].
    ///
    /// All current variants of [`ExitCode`] are listed explicitly so that
    /// adding a new variant to the enum causes a dead-code or unreachable-pattern
    /// warning here, prompting the author to assign a category. The `_` wildcard
    /// is required only because `ExitCode` is `#[non_exhaustive]` (it lives in
    /// `ocx_lib`, a separate crate); it covers only genuinely unknown future
    /// variants and maps them to [`Self::Internal`] as a safe fallback.
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
            ExitCode::RekorUnavailable => Self::RekorUnavailable,
            ExitCode::ReferrersUnsupported => Self::ReferrersUnsupported,
            // Wildcard required by `#[non_exhaustive]` on ExitCode (cross-crate match).
            // Any future variant added to ExitCode should get an explicit arm above;
            // falling through here is a bug signal, not a stable contract.
            _ => Self::Internal,
        }
    }
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
    /// Reserved remediation hint — part of the frozen v1 shape but not
    /// currently emitted (`render_error_envelope` always leaves it `None`, so
    /// `skip_serializing_if` omits it). Consumers must treat it as optional.
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

/// Render an `anyhow::Error` as a JSON error envelope (emitted on stdout by
/// `main.rs` when `--format json` is active).
///
/// Classifies the exit code via [`ocx_lib::cli::classify_error`] (walks the
/// error chain via `source()`), maps that to an [`ErrorCategory`], collects
/// identifier context from the chain, and serializes a byte-stable JSON
/// envelope matching the v1 contract (see [`ENVELOPE_SCHEMA_VERSION`]).
///
/// The `message` field is `{err:#}` (the full chain), matching the
/// plain-format `tracing::error!` line. Because the `tracing` line goes to
/// stderr and the envelope goes to stdout, consumers can parse stdout via
/// `json.loads()` without stripping logs.
///
/// # Errors
///
/// Returns an error only if `serde_json::to_string` fails. In practice, the
/// envelope shape is `Serialize`-infallible, so this is defensive — we
/// propagate rather than panicking to keep the error path robust.
pub fn render_error_envelope(command: &str, err: &anyhow::Error) -> anyhow::Result<String> {
    let err_ref: &(dyn std::error::Error + 'static) = err.as_ref();
    let exit_code = classify_error(err_ref);
    let kind = ErrorCategory::from_exit_code(exit_code);
    let message = format!("{err:#}");
    let context = collect_context(err_ref);
    let detail = collect_detail(err_ref);
    let envelope = ErrorEnvelope {
        schema_version: ENVELOPE_SCHEMA_VERSION,
        command,
        exit_code: exit_code as u8,
        error: EnvelopeError {
            kind,
            detail,
            message,
            remediation: None,
            context,
        },
    };
    Ok(serde_json::to_string(&envelope)?)
}

/// Walk the error chain via `std::iter::successors` and collect structured
/// context (identifier, etc.) for the envelope's `context` map.
///
/// Currently pulls the identifier from `SignError` / `VerifyError` — the only
/// two subsystems that carry a user-visible identifier in their Slice 1 error
/// surface. Additional subsystems attach their own context as they gain
/// envelope-relevant metadata.
fn collect_context(err: &(dyn std::error::Error + 'static)) -> BTreeMap<&'static str, serde_json::Value> {
    use ocx_lib::oci::sign::SignError;
    use ocx_lib::oci::verify::VerifyError;

    let mut context = BTreeMap::new();
    for cause in std::iter::successors(Some(err), |e| e.source()) {
        if let Some(sign_err) = cause.downcast_ref::<SignError>() {
            context.insert("identifier", serde_json::Value::String(sign_err.identifier.to_string()));
            return context;
        }
        if let Some(verify_err) = cause.downcast_ref::<VerifyError>() {
            context.insert(
                "identifier",
                serde_json::Value::String(verify_err.identifier.to_string()),
            );
            return context;
        }
    }
    context
}

/// Walk the error chain and pull the fine-grained `detail` discriminant from
/// the first leaf "kind" enum encountered.
///
/// Per C-S1-1, `envelope.error.detail` carries the snake_case variant name
/// (e.g. `"offline_sign_refused"`) so consumers can dispatch programmatically
/// without parsing stderr. The lookup walks `source()` to find the inner
/// [`SignErrorKind`] / [`VerifyErrorKind`] carried by the typed three-layer
/// errors. Returning `None` (no match) leaves `detail` absent in the JSON
/// envelope via `skip_serializing_if`.
fn collect_detail(err: &(dyn std::error::Error + 'static)) -> Option<&'static str> {
    use ocx_lib::oci::sign::SignErrorKind;
    use ocx_lib::oci::verify::VerifyErrorKind;

    for cause in std::iter::successors(Some(err), |e| e.source()) {
        if let Some(kind) = cause.downcast_ref::<SignErrorKind>() {
            return Some(kind.kind_detail());
        }
        if let Some(kind) = cause.downcast_ref::<VerifyErrorKind>() {
            return Some(kind.kind_detail());
        }
    }
    None
}

/// Render the success-path JSON envelope, serializing `data` under the
/// `data` top-level key.
///
/// Success envelopes hard-code `exit_code = 0` — any command that wants to
/// exit with a non-zero "success-ish" code (e.g. "nothing to do" for an idle
/// operation) should return that code directly through [`ExitCode`] rather
/// than layering a success envelope on top.
pub fn render_success_envelope<T: Serialize>(command: &str, data: &T) -> anyhow::Result<String> {
    let envelope = SuccessEnvelope::new(command, data);
    Ok(serde_json::to_string(&envelope)?)
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
    fn render_error_envelope_produces_v1_shape_for_synthetic_error() {
        // A synthetic anyhow error classifies to `Failure` (1) → Internal category.
        let err = anyhow::anyhow!("synthetic error for envelope probe");
        let json = render_error_envelope("package sign", &err).expect("render ok");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(parsed["schema_version"], 1);
        assert_eq!(parsed["command"], "package sign");
        assert_eq!(parsed["exit_code"], 1);
        assert_eq!(parsed["error"]["kind"], "internal");
        assert!(
            parsed["error"]["message"]
                .as_str()
                .is_some_and(|m| m.contains("synthetic error")),
            "message missing from {json}",
        );
        assert!(
            parsed["error"]["context"].is_object(),
            "context must always be an object",
        );
    }

    #[test]
    fn render_error_envelope_classifies_verify_not_found() {
        // A `VerifyError(NoSignaturesFound)` surfaces as `kind=not_found`,
        // exit 79 — matches the frozen contract test in `test_verify.py`.
        let id = ocx_lib::oci::Identifier::parse("registry.example/pkg:1.0").unwrap();
        let inner =
            ocx_lib::oci::verify::VerifyError::new(id, ocx_lib::oci::verify::VerifyErrorKind::NoSignaturesFound);
        let err = anyhow::Error::from(inner);
        let json = render_error_envelope("verify", &err).expect("render ok");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(parsed["command"], "verify");
        assert_eq!(parsed["exit_code"], 79);
        assert_eq!(parsed["error"]["kind"], "not_found");
        // Identifier surfaces in context from the SignError/VerifyError chain walk.
        assert_eq!(parsed["error"]["context"]["identifier"], "registry.example/pkg:1.0");
    }

    #[test]
    fn render_error_envelope_classifies_sign_auth_error() {
        let id = ocx_lib::oci::Identifier::parse("registry.example/pkg:1.0").unwrap();
        let inner = ocx_lib::oci::sign::SignError::new(id, ocx_lib::oci::sign::SignErrorKind::OidcTokenRejected);
        let err = anyhow::Error::from(inner);
        let json = render_error_envelope("package sign", &err).expect("render ok");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(parsed["command"], "package sign");
        assert_eq!(parsed["exit_code"], 80);
        assert_eq!(parsed["error"]["kind"], "auth_error");
        assert_eq!(parsed["error"]["context"]["identifier"], "registry.example/pkg:1.0");
    }

    #[test]
    fn envelope_detail_populated_for_offline_sign_refused() {
        // C-S1-1 frozen contract: `envelope.error.detail` carries the snake_case
        // discriminant of the inner `SignErrorKind`. Previously hard-coded to
        // `None`, which left scripts unable to distinguish e.g. an offline-refusal
        // from any other PermissionDenied without parsing stderr.
        let id = ocx_lib::oci::Identifier::parse("registry.example/pkg:1.0").unwrap();
        let inner = ocx_lib::oci::sign::SignError::new(id, ocx_lib::oci::sign::SignErrorKind::OfflineSignRefused);
        let err = anyhow::Error::from(inner);
        let json = render_error_envelope("package sign", &err).expect("render ok");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(parsed["exit_code"], 77);
        assert_eq!(parsed["error"]["kind"], "permission_denied");
        assert_eq!(parsed["error"]["detail"], "offline_sign_refused");
    }

    #[test]
    fn envelope_detail_populated_for_verify_identity_mismatch() {
        // Mirror coverage on the verify side: a reachable VerifyErrorKind variant
        // must surface its snake_case discriminant via `envelope.error.detail`.
        let id = ocx_lib::oci::Identifier::parse("registry.example/pkg:1.0").unwrap();
        let inner = ocx_lib::oci::verify::VerifyError::new(id, ocx_lib::oci::verify::VerifyErrorKind::IdentityMismatch);
        let err = anyhow::Error::from(inner);
        let json = render_error_envelope("verify", &err).expect("render ok");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(parsed["exit_code"], 77);
        assert_eq!(parsed["error"]["kind"], "permission_denied");
        assert_eq!(parsed["error"]["detail"], "identity_mismatch");
    }

    #[test]
    fn error_category_total_over_exit_codes() {
        // Spot-check representative values; the match is exhaustive so drift
        // will fail at compile time in `ErrorCategory::from_exit_code`.
        assert!(matches!(
            ErrorCategory::from_exit_code(ExitCode::NotFound),
            ErrorCategory::NotFound
        ));
        assert!(matches!(
            ErrorCategory::from_exit_code(ExitCode::PolicyBlocked),
            ErrorCategory::PermissionDenied,
        ));
        assert!(matches!(
            ErrorCategory::from_exit_code(ExitCode::ReferrersUnsupported),
            ErrorCategory::ReferrersUnsupported,
        ));
        assert!(matches!(
            ErrorCategory::from_exit_code(ExitCode::Failure),
            ErrorCategory::Internal,
        ));
    }

    #[test]
    fn render_success_envelope_golden_shape() {
        #[derive(Serialize)]
        struct D {
            a: u32,
        }
        let json = render_success_envelope("verify", &D { a: 7 }).expect("render ok");
        let expected = r#"{"schema_version":1,"command":"verify","exit_code":0,"data":{"a":7}}"#;
        assert_eq!(json, expected);
    }
}
