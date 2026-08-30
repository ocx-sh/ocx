// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Structured JSON error envelope for `--format json` error output.
//!
//! Per ADR §C-S1-1, the envelope shape is frozen and treated as a stable
//! public contract; the version integer moves only when the shape does (see
//! [`ENVELOPE_SCHEMA_VERSION`]). Root-level keys are strictly
//! `schema_version`, `command`, `exit_code`, and `error` (error path) or
//! `schema_version`, `command`, `exit_code`, `data` (success path).
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
//! The `remediation` key is **reserved** in the shape but not currently
//! emitted: [`render_error_envelope`] always leaves it `None`, so it is omitted
//! from real output. Consumers must treat it as optional.

use ocx_lib::cli::{ClassifyErrorKind, ErrorCategory, ExitCode};
use serde::Serialize;
use std::collections::BTreeMap;

/// Schema version for the JSON envelope. Bump on any breaking change.
///
/// Freeze per C-S1-1: additive fields (new keys) do not bump; shape changes
/// (rename, remove, re-nest) do. The [`ErrorCategory`] vocabulary is
/// additive: new variants appear without a bump, and renaming a variant
/// bumps only when a *released* binary ever emitted the old spelling —
/// otherwise no consumer can observe the rename, while the version flip
/// itself would break scripts pinning the number.
///
/// `rekor_unavailable` -> `transparency_log_unavailable` (exit 83
/// unchanged) is exactly that case: the old slug never shipped in a
/// release, so version 1 is still the contract.
pub const ENVELOPE_SCHEMA_VERSION: u32 = 1;

/// Error-branch JSON envelope.
///
/// Top-level shape per the ADR C-S1-1 frozen contract: `schema_version`,
/// `command`, `exit_code`, `error`. `success` is NOT present — consumers
/// branch on whether the `error` or `data` key is present.
#[derive(Debug, Serialize)]
pub struct ErrorEnvelope<'a> {
    /// Envelope schema version. Always [`ENVELOPE_SCHEMA_VERSION`].
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
    /// Coarse human-readable category. Frozen set — see [`ErrorCategory`].
    pub kind: ErrorCategory,
    /// Fine-grained snake_case variant name for programmatic matching
    /// (e.g., `"oidc_token_rejected"`). Optional.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<&'a str>,
    /// Full user-facing message (the outermost `Display` of the error chain).
    pub message: String,
    /// Reserved remediation hint — part of the frozen shape but not
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

    /// Wrap `data` in an envelope reporting `exit_code`.
    ///
    /// For the one shape a plain success envelope cannot describe: a command
    /// that produced a report **and** is about to exit non-zero, because part
    /// of its work landed and part did not (`ocx package sign
    /// --signature-format both`). The report is that run's only stdout
    /// document, so hard-coding 0 here would put a `"exit_code":0` in front of
    /// a consumer whose `$?` says 75.
    pub fn with_exit_code(command: &'a str, data: &'a T, exit_code: ExitCode) -> Self {
        Self {
            schema_version: ENVELOPE_SCHEMA_VERSION,
            command,
            exit_code: exit_code as u8,
            data,
        }
    }
}

/// Render an `anyhow::Error` as a JSON error envelope (emitted on stdout by
/// `app.rs` when `--format json` is active and the failing command printed no
/// report of its own — a report-then-fail command keeps its report as the one
/// stdout document, and the failure detail stays on stderr).
///
/// Classifies the exit code via [`crate::app::classify_error`] — the same
/// authority `main.rs` returns from, so the envelope's `exit_code` can never
/// disagree with the process's. The library classifier alone cannot downcast
/// a CLI-local [`crate::app::CommandError`], so using it here rendered every
/// such refusal as `1`/`internal` while the process exited 64 or 65 (CLI-04).
/// The result maps to an [`ErrorCategory`]; identifier context is collected
/// from the chain, and the whole is serialized as a byte-stable JSON envelope
/// matching the frozen contract (see [`ENVELOPE_SCHEMA_VERSION`]).
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
    let exit_code = crate::app::classify_error(err_ref);
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
/// Pulls the identifier from `SignError` / `VerifyError`, and both endpoints
/// from `CopyError` — a copy is the one operation whose failure is about a pair
/// of repositories, so a single `identifier` key could not say which end
/// refused. Additional subsystems attach their own context as they gain
/// envelope-relevant metadata.
fn collect_context(err: &(dyn std::error::Error + 'static)) -> BTreeMap<&'static str, serde_json::Value> {
    use ocx_lib::oci::sign::SignError;
    use ocx_lib::oci::verify::VerifyError;
    use ocx_lib::publisher::CopyError;

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
        if let Some(copy_err) = cause.downcast_ref::<CopyError>() {
            context.insert(
                "source",
                serde_json::Value::String(copy_err.source_identifier.to_string()),
            );
            context.insert(
                "target",
                serde_json::Value::String(copy_err.target_identifier.to_string()),
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
    use ocx_lib::publisher::CopyErrorKind;

    for cause in std::iter::successors(Some(err), |e| e.source()) {
        if let Some(kind) = cause.downcast_ref::<SignErrorKind>() {
            return Some(kind.kind_detail());
        }
        if let Some(kind) = cause.downcast_ref::<VerifyErrorKind>() {
            return Some(kind.kind_detail());
        }
        if let Some(kind) = cause.downcast_ref::<CopyErrorKind>() {
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
/// operation) should return that code directly through
/// [`ExitCode`](ocx_lib::cli::ExitCode) rather
/// than layering a success envelope on top.
pub fn render_success_envelope<T: Serialize>(command: &str, data: &T) -> anyhow::Result<String> {
    let envelope = SuccessEnvelope::new(command, data);
    Ok(serde_json::to_string(&envelope)?)
}

/// Render the same envelope, reporting `exit_code` instead of assuming 0.
///
/// See [`SuccessEnvelope::with_exit_code`] for the one case that needs it.
///
/// # Errors
///
/// Propagates a [`serde_json`] serialization failure.
pub fn render_envelope_with_exit_code<T: Serialize>(
    command: &str,
    data: &T,
    exit_code: ExitCode,
) -> anyhow::Result<String> {
    let envelope = SuccessEnvelope::with_exit_code(command, data, exit_code);
    Ok(serde_json::to_string(&envelope)?)
}

#[cfg(test)]
mod tests {
    //! Contract tests for the frozen JSON envelope shape (ADR C-S1-1).
    //!
    //! These tests encode the public contract that `--format json` consumers
    //! pattern-match against. Any change to these tests is a schema bump —
    //! review carefully.
    use super::*;
    use serde::Serialize;

    #[test]
    fn schema_version_is_two() {
        // v1 named the exit-83 category `rekor_unavailable`; renaming it to
        // `transparency_log_unavailable` moved an enumerated value consumers
        // match on, which this module's own bump rule calls a shape change.
        assert_eq!(ENVELOPE_SCHEMA_VERSION, 1);
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

    /// ADR item 6: a `--format json` copy failure has to carry the same
    /// machine-readable `detail` slug and identifier context that sign and
    /// verify already do. Before this arm existed a structural refusal
    /// serialized with no `detail` at all and an empty `context`, so a CI job
    /// could only match on prose.
    ///
    /// Rendered end to end through `render_error_envelope`, not by building an
    /// `ErrorEnvelope` by hand — the defect was in the two collectors, which a
    /// hand-built envelope never calls.
    #[test]
    fn a_copy_refusal_carries_its_slug_and_both_endpoints() {
        use ocx_lib::publisher::{CopyError, CopyErrorKind};

        let error = anyhow::Error::new(CopyError {
            source_identifier: "dev.example.com/acme/tool:1.4.2".parse().expect("source"),
            target_identifier: "prod.example.com/acme/tool:1.4.2".parse().expect("target"),
            kind: CopyErrorKind::IndexNamedByDigest,
        });
        let rendered = render_error_envelope("package copy", &error).expect("render");

        assert!(
            rendered.contains(r#""detail":"index_named_by_digest""#),
            "the frozen kind slug must reach the envelope: {rendered}"
        );
        assert!(
            rendered.contains(r#""source":"dev.example.com/acme/tool:1.4.2""#)
                && rendered.contains(r#""target":"prod.example.com/acme/tool:1.4.2""#),
            "a copy failure is about a pair of repositories, and both must be named: {rendered}"
        );
        assert!(
            rendered.contains(r#""exit_code":64"#),
            "a structural refusal is a usage fault: {rendered}"
        );

        // Positive control: same renderer, an error with no `CopyError` on the
        // chain. `detail` is absent and `context` is empty, so the assertions
        // above cannot be the collectors emitting those keys unconditionally.
        let unrelated = anyhow::anyhow!("dev.example.com/acme/tool:1.4.2 to prod.example.com/acme/tool:1.4.2");
        let control = render_error_envelope("package copy", &unrelated).expect("render");
        assert!(!control.contains(r#""detail""#), "control leaked a detail: {control}");
        assert!(control.contains(r#""context":{}"#), "control leaked context: {control}");
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
    fn render_error_envelope_produces_the_frozen_shape_for_a_synthetic_error() {
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
    fn render_error_envelope_classifies_sign_unsupported_key_backend() {
        // The last link of the exit-85 chain, which the library-side test
        // cannot reach: `render_error_envelope` is what a `--format json`
        // consumer actually reads, and it derives `error.kind` through
        // `classify_error` -> `ErrorCategory::from_exit_code`.
        //
        // Both assertions are load-bearing, and the second is the one that
        // discriminates: map the 85 arm to `ErrorCategory::Internal` and the
        // envelope still says `"exit_code": 85` while `error.kind` silently
        // becomes `"internal"`. A code-only assertion passes through exactly
        // the failure the dedicated category exists to prevent.
        let id = ocx_lib::oci::Identifier::parse("registry.example/pkg:1.0").unwrap();
        let rejected = ocx_lib::oci::sign::KeyRef::parse("awskms://alias/release")
            .expect_err("awskms is recognised but unimplemented");
        let inner = ocx_lib::oci::sign::SignError::new(id, ocx_lib::oci::sign::SignErrorKind::from(rejected));
        let err = anyhow::Error::from(inner);
        let json = render_error_envelope("package sign", &err).expect("render ok");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(parsed["exit_code"], 85);
        assert_eq!(
            parsed["error"]["kind"], "unsupported_key_backend",
            "the envelope must name the dedicated category, never `internal`: {json}"
        );
        assert_eq!(parsed["error"]["detail"], "unsupported_key_backend");
    }

    #[test]
    fn render_error_envelope_classifies_verify_unsupported_key_backend() {
        // Verify parses `--key` on its own path, so the same reference must
        // reach the same envelope through `VerifyErrorKind`. One vocabulary,
        // two taxonomies: a script reads one word for one failure.
        let id = ocx_lib::oci::Identifier::parse("registry.example/pkg:1.0").unwrap();
        let rejected = ocx_lib::oci::sign::KeyRef::parse("awskms://alias/release")
            .expect_err("awskms is recognised but unimplemented");
        let inner = ocx_lib::oci::verify::VerifyError::new(id, ocx_lib::oci::verify::VerifyErrorKind::from(rejected));
        let err = anyhow::Error::from(inner);
        let json = render_error_envelope("package verify", &err).expect("render ok");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(parsed["exit_code"], 85);
        assert_eq!(
            parsed["error"]["kind"], "unsupported_key_backend",
            "the envelope must name the dedicated category, never `internal`: {json}"
        );
        assert_eq!(parsed["error"]["detail"], "unsupported_key_backend");
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
    fn a_command_error_carries_the_code_the_process_exits_with() {
        // Regression (CLI-04): the envelope used to classify with the *library*
        // classifier, which by construction cannot downcast the CLI-local
        // `CommandError` -- so a refusal that exits 64 rendered as 1/internal
        // and a script branching on `exit_code` read the wrong thing. The
        // literals here are deliberate rather than a cross-check against
        // `app::classify_error`: comparing the envelope to the function it
        // calls would pass under any classifier at all.
        let err = anyhow::Error::new(crate::app::CommandError::new(
            "refusing to write the predicate to a terminal".to_string(),
            ExitCode::UsageError,
        ));
        let json = render_error_envelope("package sbom", &err).expect("render ok");
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");

        assert_eq!(value["exit_code"], 64, "envelope: {json}");
        assert_eq!(value["error"]["kind"], "usage_error", "envelope: {json}");
        assert_eq!(
            value["exit_code"].as_u64().expect("exit_code is a number"),
            crate::app::classify_error(err.as_ref()) as u8 as u64,
            "the envelope and the process must not disagree",
        );
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
