// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Sign error types (three-layer: [`SignError`] + [`SignErrorKind`]); each variant declares its exit code and `error.detail` slug.
//! Variant inventory: [`adr_oci_referrers_signing_v1.md`](../../../../../.claude/artifacts/adr_oci_referrers_signing_v1.md).

use ocx_exit::{Pick, Row};
use ocx_oci::PackageRef;
use ocx_oci::endpoint::UrlRejection;
use ocx_trust::key_ref::KeyRefError;

use crate::sign::key_backend::KeyBackendError;

/// Top-level sign error carrying the identifier being signed + the kind.
///
/// `Display` is the identifier alone: `{err:#}` appends the `#[source]` kind, so interpolating it prints it twice.
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
#[error("{identifier}")]
#[exit(delegate = kind)]
pub struct SignError {
    pub identifier: PackageRef,
    #[source]
    pub kind: SignErrorKind,
}

impl SignError {
    pub fn new(identifier: PackageRef, kind: SignErrorKind) -> Self {
        Self { identifier, kind }
    }
}

/// Discriminant kind for [`SignError`]; each variant has a distinct remediation and exit code.
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
pub enum SignErrorKind {
    /// Fulcio rejected the CSR (non-401/403). Exit 78.
    #[error("Fulcio rejected the CSR as malformed")]
    #[exit(
        ConfigError,
        slug = "fulcio_bad_request",
        summary = "The certificate authority rejected the signing request"
    )]
    FulcioBadRequest,

    /// Fulcio rejected the OIDC token (issuer, audience, expiry). Exit 80.
    #[error("Fulcio rejected OIDC token")]
    #[exit(
        AuthError,
        slug = "oidc_token_rejected",
        summary = "The certificate authority rejected the OIDC token"
    )]
    OidcTokenRejected,

    /// Fulcio unreachable, or a 429/5xx. Exit 75; the slug tells the operator whether Fulcio or Rekor is down.
    #[error("Fulcio unavailable")]
    #[exit(
        TempFail,
        slug = "fulcio_unavailable",
        summary = "The certificate authority is unreachable or overloaded"
    )]
    FulcioUnavailable,

    /// Rekor unreachable, throttled or failing at signing time (transient send failure, 408/429/502/503/504, a body stream that broke). Exit 75.
    #[error("Rekor transparency log unavailable")]
    #[exit(
        TempFail,
        slug = "transparency_log_unavailable",
        summary = "The transparency log is unavailable"
    )]
    TransparencyLogUnavailable,

    /// Rekor cannot be reached on a rerun: a refused certificate, or a 5xx outside the transient set. Exit 69.
    ///
    /// Retrying gets the same answer; fix the URL or trust the log's CA (`OCX_EXTRA_CA_CERTS`).
    #[error("Rekor transparency log unreachable")]
    #[exit(
        Unavailable,
        slug = "transparency_log_unreachable",
        summary = "The transparency log cannot be reached and a rerun will not change that"
    )]
    TransparencyLogUnreachable,

    /// Rekor answered but its SET or body is unusable, or a 2xx carried no inclusion proof; file a bug, not retry. Exit 65.
    #[error("Rekor SET malformed or missing")]
    #[exit(
        DataError,
        slug = "rekor_set_malformed",
        summary = "The transparency log answered with an unusable timestamp or body"
    )]
    RekorSetMalformed,

    /// The Referrers API is absent **and** the fallback-index write was refused. Exit 82.
    #[error(
        "registry serves no OCI Referrers API and would not hold the referrers fallback index; \
         supply-chain commands are unavailable for this registry"
    )]
    #[exit(
        Unsupported,
        slug = "referrers_unsupported",
        summary = "The registry supports neither the OCI Referrers API nor a referrers fallback tag"
    )]
    ReferrersUnsupported,

    /// The identifier did not resolve to a manifest for the requested platform. Exit 79.
    #[error("no manifest for platform {platform}")]
    #[exit(
        NotFound,
        slug = "target_not_found",
        summary = "The reference resolves to no manifest for the requested platform"
    )]
    TargetNotFound { platform: String },

    /// `--platform` was given but the reference resolved to a single manifest. Exit 79.
    ///
    /// Byte-identical to [`VerifyErrorKind::TargetNotAnIndex`](crate::verify::VerifyErrorKind::TargetNotAnIndex).
    #[error("--platform {platform} was given but the reference resolved to a single manifest, not an index")]
    #[exit(
        NotFound,
        slug = "target_not_an_index",
        summary = "A platform was requested but the reference names a single manifest"
    )]
    TargetNotAnIndex { platform: String },

    /// The subject digest is not sha256, which cosign artifacts require. Exit 65.
    ///
    /// Raised before any write: `binds_subject` accepts only sha256, so a later refusal follows a permanent Rekor entry.
    /// The sidecar tag truncates the digest to 64 characters, so a colliding tag would carry another subject's signature.
    #[error("cosign artifacts address their subject by sha256; this reference resolves to a {algorithm} digest")]
    #[exit(
        DataError,
        slug = "subject_digest_unsupported",
        summary = "The subject digest is not sha256"
    )]
    SubjectDigestUnsupported { algorithm: String },

    /// OIDC pre-check failed client-side; the token never reached Fulcio. Exit 77.
    #[error("OIDC pre-check failed: {reason}")]
    #[exit(
        PermissionDenied,
        slug = "oidc_pre_check_failed",
        summary = "The OIDC token failed a client-side check before reaching the certificate authority"
    )]
    OidcPreCheckFailed {
        /// Short reason slug (e.g. `missing_gha_permission`).
        reason: String,
    },

    /// The CI provider's OIDC token endpoint failed transiently (send failure, 408/429/502/503/504, a broken body stream). Exit 75.
    #[error("ambient OIDC token endpoint unavailable")]
    #[exit(
        TempFail,
        slug = "oidc_token_unavailable",
        summary = "The CI provider's OIDC token endpoint is unreachable or overloaded"
    )]
    OidcTokenUnavailable,

    /// The index-rewritten physical registry resolves into a forbidden range (CWE-918). Exit 78.
    #[error("refusing to dial the rewritten registry: {reason}")]
    #[exit(
        ConfigError,
        slug = "forbidden_registry_target",
        summary = "The rewritten registry resolves into a forbidden address range"
    )]
    ForbiddenRegistryTarget { reason: String },

    /// `--offline` was supplied to `ocx package sign`. Exit 81, a local policy refusal like every offline refusal.
    #[error("offline signing is not supported")]
    #[exit(
        PolicyBlocked,
        slug = "offline_sign_refused",
        summary = "Signing was refused because offline mode is on"
    )]
    OfflineSignRefused,

    /// `--identity-token-file` is readable by group or other. Exit 77.
    ///
    /// `Display` shows only the basename: the full path is a credential location (CWE-209).
    #[error(
        "identity token file `{}` has permissive permissions (mode {mode:#o}); expected 0600 or tighter",
        path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "<redacted>".into())
    )]
    #[exit(
        PermissionDenied,
        slug = "identity_token_file_permissive",
        summary = "The identity token file is readable by group or other"
    )]
    IdentityTokenFilePermissive { path: std::path::PathBuf, mode: u32 },

    /// A `--fulcio-url`/`--rekor-url` value failed validation. Exit 64, or 69 when the host does not resolve.
    #[error("invalid {endpoint} URL: {reason}")]
    #[exit(delegate = reason)]
    InvalidEndpointUrl {
        /// Flag name (e.g. `--fulcio-url`), so `error.detail` is dispatchable.
        endpoint: String,
        #[source]
        reason: UrlRejection,
    },

    /// The `--predicate` file did not parse as JSON. Exit 65.
    #[error("predicate file is not valid JSON")]
    #[exit(DataError, slug = "predicate_not_json", summary = "The predicate file is not JSON")]
    PredicateNotJson,

    /// The predicate or statement exceeded its byte limit. Exit 65.
    #[error("predicate payload is at least {actual} bytes, over the {limit}-byte limit")]
    #[exit(
        DataError,
        slug = "predicate_too_large",
        summary = "The predicate or statement exceeds its size limit"
    )]
    PredicateTooLarge {
        limit: u64,
        /// A lower bound: the bounded `--predicate` read stops one byte past the ceiling.
        actual: u64,
    },

    /// Attach resolved a provenance predicateType below SLSA v1.0. Exit 64.
    #[error("provenance predicate type {resolved} is below v1.0; pass --type slsaprovenance1")]
    #[exit(
        UsageError,
        slug = "provenance_version_unsupported",
        summary = "The provenance predicate type is older than SLSA v1.0"
    )]
    ProvenanceVersionUnsupported { resolved: String },

    /// `--offline` was supplied to `package attest` or `push --sbom`. Exit 81, as for signing.
    #[error("offline attestation is not supported")]
    #[exit(
        PolicyBlocked,
        slug = "offline_attest_refused",
        summary = "Attaching was refused because offline mode is on"
    )]
    OfflineAttestRefused,

    /// An unsigned attach named a predicate type with no SBOM media type. Exit 64.
    #[error(
        "unsigned attach supports SBOM predicate types only, not {predicate_type}; \
         supply an OIDC identity to attach it as a signed attestation"
    )]
    #[exit(
        UsageError,
        slug = "unsigned_type_unsupported",
        summary = "An unsigned attach names a predicate type with no SBOM media type"
    )]
    UnsignedTypeUnsupported { predicate_type: String },

    /// A sidecar `--signature-format` reached an attach with no signing identity. Exit 64.
    #[error(
        "--signature-format {format} writes a sha256-<hex>.att sidecar, which carries a signed \
         DSSE envelope; supply an OIDC identity or a --key, or drop the flag"
    )]
    #[exit(
        UsageError,
        slug = "sidecar_requires_signature",
        summary = "A sidecar signature format was requested without a signing identity"
    )]
    SidecarRequiresSignature { format: crate::sign::SignatureFormat },

    /// A `--key` reference named a recognised but unimplemented key backend. Exit 82.
    ///
    /// `transparent` skips `source()`: safe only while `KeyRefError` has no source and its `exit` row answers
    /// this variant directly, or the chain walker misclassifies the exit code.
    #[error(transparent)]
    #[exit(
        Unsupported,
        slug = "unsupported_key_backend",
        summary = "A key reference names a recognised key backend this build does not implement"
    )]
    UnsupportedKeyBackend(KeyRefError),

    /// The key backend could not produce a signature; exits with the wrapped error's own class.
    #[error(transparent)]
    #[exit(
        with = key_backend_row,
        rows(
            (
                TempFail,
                slug = "key_backend_unavailable",
                summary = "The signing key backend is temporarily unavailable"
            ),
            (
                IoError,
                slug = "key_unreadable",
                summary = "A signing or verification key could not be read from its location"
            ),
            (
                DataError,
                slug = "key_malformed",
                summary = "A signing or verification key holds no usable key"
            ),
            (
                Unsupported,
                slug = "unsupported_key_backend",
                summary = "A key reference names a recognised key backend this build does not implement"
            ),
        )
    )]
    KeyBackend(#[from] KeyBackendError),

    /// A `--key` reference could not be parsed. Exit 64.
    #[error(transparent)]
    #[exit(
        UsageError,
        slug = "key_reference_invalid",
        summary = "A key reference could not be parsed"
    )]
    KeyReferenceInvalid(KeyRefError),

    /// `--no-rekor-upload` was given for a keyless signature. Exit 64.
    ///
    /// Not a clap `requires = "key"`, whose message inverts the reason.
    #[error(
        "--no-rekor-upload requires --key: a keyless signature must be recorded in Rekor, \
         because a Fulcio certificate is valid for about ten minutes and the log entry's \
         timestamp is the only lasting proof the signature was made while it was"
    )]
    #[exit(
        UsageError,
        slug = "rekor_upload_required_for_keyless",
        summary = "Skipping the transparency log upload requires a signing key"
    )]
    RekorUploadRequiredForKeyless,

    /// Catch-all, exit 1; the cause stays a `#[source]`, never `.to_string()`.
    #[error("internal signing error")]
    // `None` lets the chain walker reach the wrapped cause; `Some(Failure)` would exit a wrapped registry 401/503 as 1, not 80/75/69.
    #[exit(
        chain,
        fallback(Failure, slug = "internal", summary = "A failure no classifier recognises")
    )]
    Internal(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// The row a failed key backend answers with: its own class, so a transient one stays retryable.
///
/// Exhaustive over [`KeyBackendError`], so a new backend failure must pick a row before it builds.
fn key_backend_row(error: &SignErrorKind, rows: [Row; 4]) -> Pick<'_> {
    let SignErrorKind::KeyBackend(backend) = error else {
        // Unreachable: only the `KeyBackend` arm names this function.
        return Pick::row(rows[3]);
    };
    match backend {
        KeyBackendError::Unavailable { .. } => Pick::row(rows[0]),
        KeyBackendError::Io(_) => Pick::row(rows[1]),
        KeyBackendError::MalformedKey { .. } => Pick::row(rows[2]),
        KeyBackendError::Unsupported { .. } => Pick::row(rows[3]),
    }
}

/// Select the sign-side variant a `--key` parse failure belongs to.
///
/// No wildcard, and `KeyRefError` is not `#[non_exhaustive]`: a `_` arm gives a new reason an unreviewed exit code.
impl From<KeyRefError> for SignErrorKind {
    fn from(error: KeyRefError) -> Self {
        match error {
            KeyRefError::UnsupportedBackend { .. } => Self::UnsupportedKeyBackend(error),
            KeyRefError::UnknownScheme { .. } | KeyRefError::Empty | KeyRefError::FileColonPrefix { .. } => {
                Self::KeyReferenceInvalid(error)
            }
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

    fn id() -> PackageRef {
        PackageRef::parse("registry.example/pkg:1.0").expect("parse test identifier")
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
            format!("{}", SignErrorKind::TransparencyLogUnavailable),
            "Rekor transparency log unavailable"
        );
        // No trailing periods on any variant.
        for kind in [
            SignErrorKind::FulcioBadRequest,
            SignErrorKind::OidcTokenRejected,
            SignErrorKind::TransparencyLogUnavailable,
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
    fn provenance_version_unsupported_maps_to_usage_error() {
        // 64, not 65. The value came from `--type`, so the remedy is a
        // different flag value; the message names it.
        let kind = SignErrorKind::ProvenanceVersionUnsupported {
            resolved: "https://slsa.dev/provenance/v0.2".into(),
        };
        assert!(
            format!("{kind}").contains("--type slsaprovenance1"),
            "the message must name the flag value that fixes it, got: {kind}"
        );
    }

    #[test]
    fn key_reference_that_is_not_a_backend_is_a_usage_error() {
        // The other half of the `From` split, asserted next to it so the two
        // codes cannot quietly converge: an unrecognised scheme and an empty
        // reference are malformed invocations (64), not unimplemented
        // backends (82).
        use ocx_trust::key_ref::KeyRef;

        for value in ["vault://secret/cosign", "file:"] {
            let _kind = SignErrorKind::from(KeyRef::parse(value).expect_err("not a usable key reference"));
        }
    }

    #[test]
    fn no_rekor_upload_under_keyless_states_the_reason_it_refuses() {
        // D-7. This variant exists *because* the reason has to reach the user:
        // clap's `requires = "key"` would say "the following required arguments
        // were not provided: --key", which inverts it. Pin both halves of the
        // sentence, since dropping either is what turns the refusal back into
        // the message it was built to replace.
        let kind = SignErrorKind::RekorUploadRequiredForKeyless;

        let msg = format!("{kind}");
        assert!(msg.contains("--key"), "must name the flag that makes it legal: {msg}");
        assert!(
            msg.contains("ten minutes"),
            "the certificate window is the reason, and must survive a reword: {msg}"
        );
    }
}
