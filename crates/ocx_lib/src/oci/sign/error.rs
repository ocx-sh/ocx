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
use crate::oci::endpoint::UrlRejection;

/// Top-level sign error carrying the identifier being signed + the kind.
///
/// Three-layer pattern: outer struct attaches per-object context (the
/// identifier), inner enum carries the discriminant kind. Chain walking via
/// `source()` surfaces the inner kind for programmatic dispatch.
///
/// The `Display` is the identifier alone: `kind` is `#[source]`, and every
/// render site uses the chain-walking `{err:#}` form, which appends the source
/// itself. Interpolating `{kind}` here as well printed the whole sentence twice.
#[derive(Debug, thiserror::Error)]
#[error("{identifier}")]
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
        match &self.kind {
            // See the verify-side twin: `Internal` means "no sign-side code fits
            // this", so it defers to the chain walker instead of flattening a
            // cause that classifies itself. A registry 401/503/5xx reached
            // through `map_client_error` exits 80/75/69 only because of this
            // arm; an unrecognized cause still lands on `Failure` via
            // `classify_error`'s fall-through.
            SignErrorKind::Internal(_) => None,
            kind => Some(kind.exit_code()),
        }
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
    /// Exit 83 (`RekorUnavailable`). Remediation: retry later.
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
    /// Exit 84 (`ReferrersUnsupported`). Remediation: use a registry with OCI
    /// 1.1 referrers support. The outer `SignError` Display (`"{identifier}:
    /// {kind}"`) already prefixes this with the registry host, so the message
    /// itself does not repeat it.
    #[error(
        "registry does not support the OCI Referrers API (requires OCI Distribution 1.1+); \
         supply-chain commands are unavailable for this registry"
    )]
    ReferrersUnsupported,

    /// The identifier did not resolve to a manifest for the requested platform.
    ///
    /// Exit 79 (`NotFound`). Previously an `Internal` (exit 1), which reported a
    /// plain typo in `--platform` as a bug in ocx.
    #[error("no manifest for platform {platform}")]
    TargetNotFound { platform: String },

    /// OIDC pre-check (expiry, audience) failed client-side — token never sent to Fulcio.
    ///
    /// Exit 77 (`PermissionDenied`). Remediation: per-platform hint table.
    #[error("OIDC pre-check failed: {reason}")]
    OidcPreCheckFailed {
        /// Short reason identifier (e.g., `missing_gha_permission`).
        reason: String,
    },

    /// The physical registry the index rewrote this reference to resolves into
    /// a forbidden range (CWE-918).
    ///
    /// Exit 78 (`ConfigError`) -- the same code the pull path's dial guard
    /// yields for the same refusal. Remediation: add the host to
    /// `trusted_hosts` for that registry, or fix the indirection.
    #[error("refusing to dial the rewritten registry: {reason}")]
    ForbiddenRegistryTarget {
        /// Rendered SSRF refusal: the host and the address it resolved to.
        reason: String,
    },

    /// `--offline` was supplied to `ocx package sign`; S1-E policy rejects offline signing.
    ///
    /// Exit 77 (`PermissionDenied`) — policy rejection of the *action*, not a
    /// passive network access.
    #[error("offline signing is not supported")]
    OfflineSignRefused,

    /// `--identity-token-file` was readable by group or other (mode bits in
    /// `mode & 0o077` were non-zero). Secrets must be owner-readable only.
    ///
    /// Exit 77 (`PermissionDenied`). Remediation: `chmod 600 <path>`.
    ///
    /// The `Display` impl deliberately surfaces only the file's basename — the
    /// full path can leak through CLI stderr, the JSON error envelope, or any
    /// log sink, and a token-file path is a sensitive credential location that
    /// should not be echoed back to whatever pipes the command output
    /// (CWE-209). The full `PathBuf` is preserved on the variant for callers
    /// that legitimately need it.
    #[error(
        "identity token file `{}` has permissive permissions (mode {mode:#o}); expected 0600 or tighter",
        path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "<redacted>".into())
    )]
    IdentityTokenFilePermissive {
        /// Path to the token file that failed the permission check.
        path: std::path::PathBuf,
        /// Raw Unix mode bits (lower 12 bits: setuid/setgid/sticky + rwxrwxrwx).
        mode: u32,
    },

    /// User-supplied Sigstore endpoint URL failed SSRF/scheme validation.
    ///
    /// Surfaces at the boundary where `--fulcio-url` / `--rekor-url` are
    /// parsed. Exit 64 (`UsageError`) — a malformed flag value is a CLI
    /// misuse, not a runtime fault. The `endpoint` field carries the flag
    /// name (e.g. `--fulcio-url`) so the envelope `error.detail` is
    /// programmatically dispatchable.
    #[error("invalid {endpoint} URL: {reason}")]
    InvalidEndpointUrl {
        /// Flag name the URL was supplied via (e.g. `--fulcio-url`).
        endpoint: String,
        /// Structured rejection reason from [`crate::oci::endpoint::validate_sigstore_url`].
        #[source]
        reason: UrlRejection,
    },

    /// Catch-all for Fulcio/Rekor HTTP errors outside the codes above.
    ///
    /// Exit 1 (`Failure`). Carries the underlying error via `#[source]` so
    /// `classify_error` chain-walking and `{err:#}` diagnostics preserve the
    /// cause — never erase it with `.to_string()`.
    #[error("internal signing error")]
    Internal(#[source] Box<dyn std::error::Error + Send + Sync>),
}

impl ClassifyErrorKind for SignErrorKind {
    fn exit_code(&self) -> ExitCode {
        match self {
            Self::FulcioBadRequest => ExitCode::ConfigError,
            Self::ForbiddenRegistryTarget { .. } => ExitCode::ConfigError,
            Self::OidcTokenRejected => ExitCode::AuthError,
            Self::RekorUnavailable => ExitCode::RekorUnavailable,
            Self::RekorSetMalformed => ExitCode::DataError,
            Self::ReferrersUnsupported => ExitCode::ReferrersUnsupported,
            Self::TargetNotFound { .. } => ExitCode::NotFound,
            Self::OidcPreCheckFailed { .. } | Self::OfflineSignRefused | Self::IdentityTokenFilePermissive { .. } => {
                ExitCode::PermissionDenied
            }
            Self::InvalidEndpointUrl { .. } => ExitCode::UsageError,
            Self::Internal(_) => ExitCode::Failure,
        }
    }

    fn kind_detail(&self) -> &'static str {
        // Frozen contract C-S1-1: snake_case parallel of the variant name.
        // Exhaustive match — no wildcard, so adding a variant forces a new arm.
        match self {
            Self::FulcioBadRequest => "fulcio_bad_request",
            Self::OidcTokenRejected => "oidc_token_rejected",
            Self::RekorUnavailable => "rekor_unavailable",
            Self::RekorSetMalformed => "rekor_set_malformed",
            Self::ReferrersUnsupported => "referrers_unsupported",
            Self::TargetNotFound { .. } => "target_not_found",
            Self::OidcPreCheckFailed { .. } => "oidc_pre_check_failed",
            Self::ForbiddenRegistryTarget { .. } => "forbidden_registry_target",
            Self::OfflineSignRefused => "offline_sign_refused",
            Self::IdentityTokenFilePermissive { .. } => "identity_token_file_permissive",
            Self::InvalidEndpointUrl { .. } => "invalid_endpoint_url",
            Self::Internal(_) => "internal",
        }
    }
}

#[cfg(test)]
mod tests {
    //! ADR §"SignErrorKind — variant inventory" contract tests.
    //!
    //! Exit-code mapping is part of the public CLI contract: backend consumers
    //! switch on `$?` to distinguish retryable from terminal failures. Any
    //! change to these assertions is a user-visible contract change — review
    //! carefully.
    use super::*;

    fn id() -> Identifier {
        Identifier::parse("registry.example/pkg:1.0").expect("parse test identifier")
    }

    #[test]
    fn fulcio_bad_request_maps_to_config_error() {
        assert_eq!(SignErrorKind::FulcioBadRequest.exit_code(), ExitCode::ConfigError);
    }

    #[test]
    fn oidc_token_rejected_maps_to_auth_error() {
        assert_eq!(SignErrorKind::OidcTokenRejected.exit_code(), ExitCode::AuthError);
    }

    #[test]
    fn rekor_unavailable_maps_to_rekor_unavailable() {
        assert_eq!(SignErrorKind::RekorUnavailable.exit_code(), ExitCode::RekorUnavailable);
    }

    #[test]
    fn rekor_set_malformed_maps_to_data_error() {
        assert_eq!(SignErrorKind::RekorSetMalformed.exit_code(), ExitCode::DataError);
    }

    #[test]
    fn referrers_unsupported_maps_to_referrers_unsupported() {
        assert_eq!(
            SignErrorKind::ReferrersUnsupported.exit_code(),
            ExitCode::ReferrersUnsupported,
        );
    }

    #[test]
    fn oidc_precheck_failed_maps_to_permission_denied() {
        let kind = SignErrorKind::OidcPreCheckFailed {
            reason: "missing_gha_permission".into(),
        };
        assert_eq!(kind.exit_code(), ExitCode::PermissionDenied);
    }

    #[test]
    fn offline_sign_refused_maps_to_permission_denied() {
        // Policy rejection of the *action*, not a passive network access.
        assert_eq!(
            SignErrorKind::OfflineSignRefused.exit_code(),
            ExitCode::PermissionDenied
        );
    }

    #[test]
    fn identity_token_file_permissive_maps_to_permission_denied() {
        // World-readable token file is a security policy violation.
        let kind = SignErrorKind::IdentityTokenFilePermissive {
            path: std::path::PathBuf::from("/tmp/tok"),
            mode: 0o644,
        };
        assert_eq!(kind.exit_code(), ExitCode::PermissionDenied);
    }

    #[test]
    fn internal_maps_to_failure() {
        // Unclassified errors fall through to Failure (generic).
        let inner: Box<dyn std::error::Error + Send + Sync> = "kaboom".into();
        let kind = SignErrorKind::Internal(inner);
        assert_eq!(kind.exit_code(), ExitCode::Failure);
    }

    #[test]
    fn sign_error_renders_identifier_then_kind_exactly_once() {
        // The contract is the *rendered* line, not the bare Display: every
        // render site (`main.rs`, the JSON envelope) formats an `anyhow::Error`
        // with `{err:#}`, which appends each `source()` after a ": ". A regression
        // that also interpolates `{kind}` into the outer Display still starts with
        // the identifier and still contains the kind — so assert the count.
        let err = anyhow::Error::new(SignError::new(id(), SignErrorKind::OidcTokenRejected));
        let msg = format!("{err:#}");
        assert!(msg.starts_with("registry.example/pkg:1.0:"), "got: {msg}");
        assert_eq!(
            msg.matches("Fulcio rejected OIDC token").count(),
            1,
            "kind must be rendered once, not duplicated by the outer Display: {msg}"
        );
    }

    #[test]
    fn sign_error_kind_display_rules() {
        // API Guidelines C-GOOD-ERR: lowercase when starting with English word,
        // no trailing punctuation. Acronyms retain canonical case.
        assert_eq!(
            format!("{}", SignErrorKind::FulcioBadRequest),
            "Fulcio rejected the CSR as malformed"
        );
        assert_eq!(
            format!("{}", SignErrorKind::OidcTokenRejected),
            "Fulcio rejected OIDC token"
        );
        assert_eq!(
            format!("{}", SignErrorKind::RekorUnavailable),
            "Rekor transparency log unavailable"
        );
        // No trailing periods on any variant.
        for kind in [
            SignErrorKind::FulcioBadRequest,
            SignErrorKind::OidcTokenRejected,
            SignErrorKind::RekorUnavailable,
            SignErrorKind::RekorSetMalformed,
            SignErrorKind::ReferrersUnsupported,
            SignErrorKind::OfflineSignRefused,
            SignErrorKind::IdentityTokenFilePermissive {
                path: std::path::PathBuf::from("/tmp/tok"),
                mode: 0o644,
            },
        ] {
            let msg = format!("{kind}");
            assert!(!msg.ends_with('.'), "trailing period on: {msg}");
        }
    }

    #[test]
    fn sign_error_classify_delegates_to_kind() {
        let err = SignError::new(id(), SignErrorKind::RekorUnavailable);
        assert_eq!(err.classify(), Some(ExitCode::RekorUnavailable));
    }

    #[test]
    fn sign_error_source_chain_preserves_inner_error() {
        // `Internal` carries the inner error via #[source].
        // Chain walking must surface it for diagnostics.
        use std::error::Error;
        let inner: Box<dyn std::error::Error + Send + Sync> = "inner boom".into();
        let kind = SignErrorKind::Internal(inner);
        let err = SignError::new(id(), kind);
        // SignError → SignErrorKind → inner error.
        let source_kind = err.source().expect("SignError has source");
        let source_inner = source_kind.source().expect("SignErrorKind has inner source");
        assert_eq!(format!("{source_inner}"), "inner boom");
    }

    #[test]
    fn kind_detail_values_are_stable() {
        // C-S1-1 frozen contract: these strings ship in JSON envelopes and consumer
        // scripts dispatch on them. A rename or typo here is a user-visible breaking
        // change. The exhaustive match in `kind_detail()` ensures a new variant forces
        // a new arm there; this table ensures the *string value* for each arm is pinned.
        use crate::oci::endpoint::UrlRejection;
        use SignErrorKind::*;

        // Construct one representative instance per variant.
        // Unit/fieldless variants are listed first; struct/tuple variants follow.
        // `Internal` is last because it needs a boxed error allocation.
        let pairs: &[(&'static str, SignErrorKind)] = &[
            ("fulcio_bad_request", FulcioBadRequest),
            ("oidc_token_rejected", OidcTokenRejected),
            ("rekor_unavailable", RekorUnavailable),
            ("rekor_set_malformed", RekorSetMalformed),
            ("referrers_unsupported", ReferrersUnsupported),
            (
                "target_not_found",
                TargetNotFound {
                    platform: "linux/amd64".into(),
                },
            ),
            ("oidc_pre_check_failed", OidcPreCheckFailed { reason: String::new() }),
            (
                "forbidden_registry_target",
                ForbiddenRegistryTarget {
                    reason: "host resolves into a forbidden range".into(),
                },
            ),
            ("offline_sign_refused", OfflineSignRefused),
            (
                "identity_token_file_permissive",
                IdentityTokenFilePermissive {
                    path: std::path::PathBuf::from("/tmp/tok"),
                    mode: 0o644,
                },
            ),
            (
                "invalid_endpoint_url",
                InvalidEndpointUrl {
                    endpoint: "--fulcio-url".into(),
                    reason: UrlRejection {
                        reason: "URL must use HTTPS".into(),
                    },
                },
            ),
            ("internal", Internal(Box::new(std::io::Error::other("test")))),
        ];

        // What this pins, exactly: a row deleted from the table above without
        // the count being lowered. It does NOT force a row for a *new* variant
        // -- `pairs` is an array literal, so `len()` is a compile-time constant
        // and both sides move together if the author simply bumps the number.
        // `kind_detail`'s exhaustive match forces the new *arm*; nothing yet
        // forces the new *row*, which is why the table once sat at 11 rows
        // against 12 arms. Closing that gap needs variant enumeration.
        assert_eq!(
            pairs.len(),
            12,
            "a row was removed from the table above; restore it rather than lowering this count"
        );

        for (expected, kind) in pairs {
            assert_eq!(kind.kind_detail(), *expected, "kind_detail() drift for {kind:?}",);
        }
    }
}
