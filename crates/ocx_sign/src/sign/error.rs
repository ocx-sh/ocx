// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Sign error types (three-layer: [`SignError`] + [`SignErrorKind`]); exit-code classification lives in `ocx::exit`.
//! Variant inventory: [`adr_oci_referrers_signing_v1.md`](../../../../../.claude/artifacts/adr_oci_referrers_signing_v1.md).

use ocx_oci::PackageRef;
use ocx_oci::endpoint::UrlRejection;
use ocx_trust::key_ref::KeyRefError;

/// Top-level sign error carrying the identifier being signed + the kind.
///
/// `Display` is the identifier alone: `{err:#}` appends the `#[source]` kind, so interpolating it prints it twice.
#[derive(Debug, thiserror::Error)]
#[error("{identifier}")]
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
#[derive(Debug, thiserror::Error)]
pub enum SignErrorKind {
    /// Fulcio rejected the CSR (non-401/403). Exit 78.
    #[error("Fulcio rejected the CSR as malformed")]
    FulcioBadRequest,

    /// Fulcio rejected the OIDC token (issuer, audience, expiry). Exit 80.
    #[error("Fulcio rejected OIDC token")]
    OidcTokenRejected,

    /// Fulcio unreachable, or a 429/5xx. Exit 75, apart from Rekor's 83 so the operator can tell which is down.
    #[error("Fulcio unavailable")]
    FulcioUnavailable,

    /// Rekor unavailable at signing time. Exit 83.
    #[error("Rekor transparency log unavailable")]
    TransparencyLogUnavailable,

    /// Rekor answered but its SET or body is unusable; file a bug, not retry. Exit 65.
    #[error("Rekor SET malformed or missing")]
    RekorSetMalformed,

    /// The Referrers API is absent **and** the fallback-index write was refused. Exit 84.
    #[error(
        "registry serves no OCI Referrers API and would not hold the referrers fallback index; \
         supply-chain commands are unavailable for this registry"
    )]
    ReferrersUnsupported,

    /// The identifier did not resolve to a manifest for the requested platform. Exit 79.
    #[error("no manifest for platform {platform}")]
    TargetNotFound { platform: String },

    /// `--platform` was given but the reference resolved to a single manifest. Exit 79.
    ///
    /// Byte-identical to [`VerifyErrorKind::TargetNotAnIndex`](crate::verify::VerifyErrorKind::TargetNotAnIndex).
    #[error("--platform {platform} was given but the reference resolved to a single manifest, not an index")]
    TargetNotAnIndex { platform: String },

    /// The subject digest is not sha256, which cosign artifacts require. Exit 65.
    ///
    /// Raised before any write: `binds_subject` accepts only sha256, so a later refusal follows a permanent Rekor entry.
    /// The sidecar tag truncates the digest to 64 characters, so a colliding tag would carry another subject's signature.
    #[error("cosign artifacts address their subject by sha256; this reference resolves to a {algorithm} digest")]
    SubjectDigestUnsupported { algorithm: String },

    /// OIDC pre-check failed client-side; the token never reached Fulcio. Exit 77.
    #[error("OIDC pre-check failed: {reason}")]
    OidcPreCheckFailed {
        /// Short reason slug (e.g. `missing_gha_permission`).
        reason: String,
    },

    /// The index-rewritten physical registry resolves into a forbidden range (CWE-918). Exit 78.
    #[error("refusing to dial the rewritten registry: {reason}")]
    ForbiddenRegistryTarget { reason: String },

    /// `--offline` was supplied to `ocx package sign`. Exit 77.
    #[error("offline signing is not supported")]
    OfflineSignRefused,

    /// `--identity-token-file` is readable by group or other. Exit 77.
    ///
    /// `Display` shows only the basename: the full path is a credential location (CWE-209).
    #[error(
        "identity token file `{}` has permissive permissions (mode {mode:#o}); expected 0600 or tighter",
        path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "<redacted>".into())
    )]
    IdentityTokenFilePermissive { path: std::path::PathBuf, mode: u32 },

    /// A `--fulcio-url`/`--rekor-url` value failed validation. Exit 64, or 69 when the host does not resolve.
    #[error("invalid {endpoint} URL: {reason}")]
    InvalidEndpointUrl {
        /// Flag name (e.g. `--fulcio-url`), so `error.detail` is dispatchable.
        endpoint: String,
        #[source]
        reason: UrlRejection,
    },

    /// The `--predicate` file did not parse as JSON. Exit 65.
    #[error("predicate file is not valid JSON")]
    PredicateNotJson,

    /// The predicate or statement exceeded its byte limit. Exit 65.
    #[error("predicate payload is at least {actual} bytes, over the {limit}-byte limit")]
    PredicateTooLarge {
        limit: u64,
        /// A lower bound: the bounded `--predicate` read stops one byte past the ceiling.
        actual: u64,
    },

    /// Attach resolved a provenance predicateType below SLSA v1.0. Exit 64.
    #[error("provenance predicate type {resolved} is below v1.0; pass --type slsaprovenance1")]
    ProvenanceVersionUnsupported { resolved: String },

    /// `--offline` was supplied to `package attest` or `push --sbom`. Exit 77, as for signing.
    #[error("offline attestation is not supported")]
    OfflineAttestRefused,

    /// An unsigned attach named a predicate type with no SBOM media type. Exit 64.
    #[error(
        "unsigned attach supports SBOM predicate types only, not {predicate_type}; \
         supply an OIDC identity to attach it as a signed attestation"
    )]
    UnsignedTypeUnsupported { predicate_type: String },

    /// A sidecar `--signature-format` reached an attach with no signing identity. Exit 64.
    #[error(
        "--signature-format {format} writes a sha256-<hex>.att sidecar, which carries a signed \
         DSSE envelope; supply an OIDC identity or a --key, or drop the flag"
    )]
    SidecarRequiresSignature { format: crate::sign::SignatureFormat },

    /// A `--key` reference named a recognised but unimplemented key backend. Exit 85.
    ///
    /// `transparent` skips `source()`: safe only while `KeyRefError` has no source and `exit_code()` answers
    /// this variant directly, or the chain walker misclassifies the exit code.
    #[error(transparent)]
    UnsupportedKeyBackend(KeyRefError),

    /// The key backend could not produce a signature; exits with the wrapped error's own class.
    #[error(transparent)]
    KeyBackend(#[from] crate::sign::key_backend::KeyBackendError),

    /// A `--key` reference could not be parsed. Exit 64.
    #[error(transparent)]
    KeyReferenceInvalid(KeyRefError),

    /// `--no-rekor-upload` was given for a keyless signature. Exit 64.
    ///
    /// Not a clap `requires = "key"`, whose message inverts the reason.
    #[error(
        "--no-rekor-upload requires --key: a keyless signature must be recorded in Rekor, \
         because a Fulcio certificate is valid for about ten minutes and the log entry's \
         timestamp is the only lasting proof the signature was made while it was"
    )]
    RekorUploadRequiredForKeyless,

    /// Catch-all, exit 1; the cause stays a `#[source]`, never `.to_string()`.
    #[error("internal signing error")]
    Internal(#[source] Box<dyn std::error::Error + Send + Sync>),
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
        // backends (85).
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
