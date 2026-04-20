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
