// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Sign error types (three-layer: [`SignError`] + [`SignErrorKind`]).
//!
//! Per
//! [`adr_oci_referrers_signing_v1.md`](../../../../../.claude/artifacts/adr_oci_referrers_signing_v1.md)
//! §"SignErrorKind — variant inventory": every kind below is justified by a
//! distinct user-facing remediation *and* a distinct exit code. The kind enum
//! is a pure discriminant (`ClassifyErrorKind`); the outer [`SignError`] carries
//! the per-signing context (identifier) and delegates classification via
//! [`ClassifyExitCode`].

use crate::cli::{ClassifyErrorKind, ClassifyExitCode, ExitCode};
use crate::oci::Identifier;

/// Top-level sign error carrying the identifier being signed + the kind.
///
/// Three-layer pattern: outer struct attaches per-object context (the
/// identifier), inner enum carries the discriminant kind. Chain walking via
/// `source()` surfaces the inner kind for programmatic dispatch.
#[derive(Debug, thiserror::Error)]
#[error("{identifier}: {kind}")]
pub struct SignError {
    /// Identifier being signed when the failure occurred.
    pub identifier: Identifier,
    /// Discriminant kind of the failure.
    #[source]
    pub kind: SignErrorKind,
}

impl SignError {
    /// Build a [`SignError`] from an identifier + kind.
    pub fn new(identifier: Identifier, kind: SignErrorKind) -> Self {
        Self { identifier, kind }
    }
}

impl ClassifyExitCode for SignError {
    fn classify(&self) -> Option<ExitCode> {
        Some(self.kind.exit_code())
    }
}

/// Discriminant kind for [`SignError`].
///
/// Each variant is justified by a distinct user-facing remediation AND a
/// distinct exit code (see ADR §"Variant inventory & justification"). Variants
/// that would map to identical remediation + exit code are merged.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SignErrorKind {
    /// Fulcio rejected the CSR (non-401/403) — config-side defect.
    ///
    /// Exit 78 (`ConfigError`). Remediation: file a bug.
    #[error("Fulcio rejected the CSR as malformed")]
    FulcioBadRequest,

    /// Fulcio rejected the OIDC token — issuer mismatch, audience wrong, expired.
    ///
    /// Exit 80 (`AuthError`). Remediation: refresh token, check issuer URL.
    #[error("Fulcio rejected OIDC token")]
    OidcTokenRejected,

    /// Rekor unavailable at time of signing.
    ///
    /// Exit 82 (`RekorUnavailable`). Remediation: retry later.
    #[error("Rekor transparency log unavailable")]
    RekorUnavailable,

    /// Rekor returned the entry but SET could not be extracted or parsed.
    ///
    /// Distinct from [`Self::RekorUnavailable`] because the remediation is
    /// "file a bug," not "retry." Exit 65 (`DataError`).
    #[error("Rekor SET malformed or missing")]
    RekorSetMalformed,

    /// Registry returned 404 on `/v2/<name>/referrers/`.
    ///
    /// Exit 83 (`ReferrersUnsupported`). Remediation: use a registry with OCI
    /// 1.1 referrers support.
    #[error("registry does not support the OCI Referrers API")]
    ReferrersUnsupported,

    /// OIDC pre-check (expiry, audience) failed client-side — token never sent to Fulcio.
    ///
    /// Exit 77 (`PermissionDenied`). Remediation: per-platform hint table.
    #[error("OIDC pre-check failed: {reason}")]
    OidcPreCheckFailed {
        /// Short reason identifier (e.g., `missing_gha_permission`).
        reason: String,
    },

    /// `--offline` was supplied to `ocx package sign`; S1-E policy rejects offline signing.
    ///
    /// Exit 77 (`PermissionDenied`) — policy rejection of the *action*, not a
    /// passive network access.
    #[error("offline signing is not supported")]
    OfflineSignRefused,

    /// Catch-all for Fulcio/Rekor HTTP errors outside the codes above.
    ///
    /// Exit 1 (`Failure`). Carries the underlying error via `#[source]` so
    /// `classify_error` chain-walking and `{err:#}` diagnostics preserve the
    /// cause — never erase it with `.to_string()`.
    #[error("signing pipeline internal error")]
    SigningPipelineInternal(#[source] Box<dyn std::error::Error + Send + Sync>),
}

impl ClassifyErrorKind for SignErrorKind {
    fn exit_code(&self) -> ExitCode {
        match self {
            Self::FulcioBadRequest => ExitCode::ConfigError,
            Self::OidcTokenRejected => ExitCode::AuthError,
            Self::RekorUnavailable => ExitCode::RekorUnavailable,
            Self::RekorSetMalformed => ExitCode::DataError,
            Self::ReferrersUnsupported => ExitCode::ReferrersUnsupported,
            Self::OidcPreCheckFailed { .. } | Self::OfflineSignRefused => ExitCode::PermissionDenied,
            Self::SigningPipelineInternal(_) => ExitCode::Failure,
        }
    }
}
