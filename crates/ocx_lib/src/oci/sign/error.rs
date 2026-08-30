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
use crate::oci::sign::KeyRefError;

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

    /// Fulcio could not be reached, or answered with a transient fault
    /// (429 or any 5xx).
    ///
    /// Exit 75 (`TempFail`). Remediation: retry. The Rekor twin is
    /// [`Self::TransparencyLogUnavailable`] (83); the two stay separate codes
    /// so an operator can tell which service is down, and both stay separate
    /// from [`Self::FulcioBadRequest`] (78) so retryable is distinguishable
    /// from terminal (PKG-28).
    #[error("Fulcio unavailable")]
    FulcioUnavailable,

    /// Rekor unavailable at time of signing.
    ///
    /// Exit 83 (`TransparencyLogUnavailable`). Remediation: retry later.
    #[error("Rekor transparency log unavailable")]
    TransparencyLogUnavailable,

    /// Rekor returned the entry but SET could not be extracted or parsed.
    ///
    /// Distinct from [`Self::TransparencyLogUnavailable`] because the remediation is
    /// "file a bug," not "retry." Exit 65 (`DataError`).
    #[error("Rekor SET malformed or missing")]
    RekorSetMalformed,

    /// The Referrers API is absent **and** the tag-schema fallback write was
    /// refused (spec D3).
    ///
    /// Exit 84 (`ReferrersUnsupported`). The old message — "registry does not
    /// support the OCI Referrers API" — became false the moment the capability
    /// gates were removed (ADR Amendment 10, C-009): an absent API is now
    /// served by the fallback index, so reaching 84 on the write side means the
    /// fallback could not hold the referrer either. Remediation is therefore a
    /// registry that serves the Referrers API, which carries no such ceiling.
    /// The outer `SignError` Display (`"{identifier}: {kind}"`) already
    /// prefixes this with the registry host, so the message does not repeat it.
    #[error(
        "registry serves no OCI Referrers API and would not hold the referrers fallback index; \
         supply-chain commands are unavailable for this registry"
    )]
    ReferrersUnsupported,

    /// The identifier did not resolve to a manifest for the requested platform.
    ///
    /// Exit 79 (`NotFound`). Previously an `Internal` (exit 1), which reported a
    /// plain typo in `--platform` as a bug in ocx.
    #[error("no manifest for platform {platform}")]
    TargetNotFound { platform: String },

    /// `--platform` was given but the reference resolved to a single manifest.
    ///
    /// Exit 79 (`NotFound`) and a slug of its own, byte-identical to
    /// [`VerifyErrorKind::TargetNotAnIndex`](crate::oci::verify::VerifyErrorKind::TargetNotAnIndex)
    /// — one refusal, one word, whichever verb reported it. Separate from
    /// [`Self::TargetNotFound`] because the remedies differ: "this package
    /// ships no such platform" sends you looking for a build, "this reference
    /// has no platforms to choose from" tells you to drop the flag.
    #[error("--platform {platform} was given but the reference resolved to a single manifest, not an index")]
    TargetNotAnIndex { platform: String },

    /// The subject resolved to a digest OCX cannot address in a cosign
    /// artifact. Everything the sign and attest paths write is sha256-only.
    ///
    /// Exit 65 (`DataError`), deliberately not 64: the algorithm is a property
    /// of the *published manifest*, not of anything the caller typed, so no
    /// amount of retyping the reference or the `--platform` fixes it. Same
    /// class as [`Self::PredicateNotJson`] — material OCX was handed and
    /// cannot use.
    ///
    /// Raised at target resolution, before a blob, a manifest or a Rekor entry
    /// is written, because what OCX writes cannot carry the subject and fails
    /// *later* and worse. The in-toto Statement's DigestSet is emitted from
    /// the subject's own algorithm while
    /// [`binds_subject`](crate::oci::attest::statement) accepts `sha256`
    /// alone, so the refusal (`statement_subject_weak_algorithm`) arrives at
    /// verify time — after a permanent transparency-log entry has been burned
    /// and the run exited 0. cosign itself is sha256-only, so accepting a
    /// stronger algorithm and failing later is strictly worse than refusing
    /// up front, and the alternative — widening `binds_subject` — would
    /// enlarge the trust surface to algorithms nothing else in the pipeline
    /// handles.
    ///
    /// The sidecar tag is the reason the refusal is *not* narrowed to the
    /// legs that build a Statement. It is
    /// `<algorithm>-<encoded truncated to 64>.<suffix>`, so two subjects
    /// sharing a 64-character prefix share one tag: the spec accepts that
    /// collision for a referrers index, where the index is re-read and
    /// filtered, but a signature parked under a colliding tag is simply the
    /// wrong subject's. One algorithm end to end is the only shape in which
    /// that question does not have to be asked.
    #[error("cosign artifacts address their subject by sha256; this reference resolves to a {algorithm} digest")]
    SubjectDigestUnsupported {
        /// The algorithm prefix the subject digest carries, e.g. `sha384`.
        algorithm: String,
    },

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

    /// The `--predicate` file did not parse as JSON.
    ///
    /// Exit 65 (`DataError`) — the *content* of a file the user named is
    /// malformed, not the invocation. Contrast
    /// [`Self::ProvenanceVersionUnsupported`], where the offending value came
    /// from the command line and the code is 64.
    #[error("predicate file is not valid JSON")]
    PredicateNotJson,

    /// The `--predicate` file exceeded `MAX_PREDICATE_FILE_BYTES`.
    ///
    /// Exit 65 (`DataError`).
    #[error("predicate payload is at least {actual} bytes, over the {limit}-byte limit")]
    PredicateTooLarge {
        /// The configured ceiling, in bytes.
        limit: u64,
        /// Bytes counted before the limit tripped — a lower bound, not the
        /// size on disk. The signer passes the exact statement length; the
        /// CLI's `--predicate` read is bounded and stops one byte past the
        /// ceiling, so it never learns how far over the file actually is.
        /// Hence "at least" in the message: it is true for both producers.
        actual: u64,
    },

    /// Attach resolved a provenance predicateType below SLSA v1.0.
    ///
    /// Exit 64 (`UsageError`), not 65: the offending value came from the
    /// invocation, so the fix is a different flag value rather than a
    /// different file. The message names that value.
    #[error("provenance predicate type {resolved} is below v1.0; pass --type slsaprovenance1")]
    ProvenanceVersionUnsupported {
        /// The predicateType the requested `--type` resolved to.
        resolved: String,
    },

    /// `--offline` was supplied to an attestation-publishing command
    /// (`ocx package attest`, or `ocx package push --sbom`).
    ///
    /// Exit 77 (`PermissionDenied`), reused verbatim from
    /// [`Self::OfflineSignRefused`]: attesting *is* signing, and a policy
    /// refusal must not classify differently depending on which verb reached
    /// it. Refused before token resolution, so no credential is touched.
    #[error("offline attestation is not supported")]
    OfflineAttestRefused,

    /// An unsigned attach was asked for a predicate type that has no SBOM
    /// media type to carry it.
    ///
    /// Exit 64 (`UsageError`), the same code and the same reasoning as
    /// [`Self::ProvenanceVersionUnsupported`]: the offending value came from
    /// the invocation. An unsigned referrer records what it is in its
    /// `artifactType` and nowhere else, so a provenance or custom predicate has
    /// no place to state its type — the fix is to supply a signing identity,
    /// not a different file.
    #[error(
        "unsigned attach supports SBOM predicate types only, not {predicate_type}; \
         supply an OIDC identity to attach it as a signed attestation"
    )]
    UnsignedTypeUnsupported {
        /// The predicateType the requested `--type` resolved to.
        predicate_type: String,
    },

    /// `--signature-format simplesigning` or `both` reached an attach with no
    /// signing identity at all.
    ///
    /// Exit 64 (`UsageError`), the same code and the same reasoning as
    /// [`Self::UnsignedTypeUnsupported`]: the offending value came from the
    /// invocation. A `sha256-<hex>.att` sidecar layer **is** a DSSE envelope,
    /// so an unsigned attach has nothing to put in one — and quietly writing
    /// the bundle shape instead would make `--signature-format` a flag that did
    /// something other than what it says, which is the failure mode this
    /// surface rejects everywhere else. Remediation: supply an OIDC identity or
    /// a `--key`, or drop the flag.
    #[error(
        "--signature-format {format} writes a sha256-<hex>.att sidecar, which carries a signed \
         DSSE envelope; supply an OIDC identity or a --key, or drop the flag"
    )]
    SidecarRequiresSignature {
        /// The requested format, echoed so the message names what was asked for.
        format: crate::oci::sign::SignatureFormat,
    },

    /// A `--key` reference named a key backend OCX recognises but has not
    /// implemented (`awskms://`, `gcpkms://`, `azurekms://`, `hashivault://`,
    /// `k8s://`).
    ///
    /// Exit 85 (`UnsupportedKeyBackend`), with its own envelope `error.kind`
    /// rather than a fold into `usage_error`: the invocation was well-formed
    /// and the backend is real, so a script can branch on "not built yet"
    /// separately from "you typed it wrong". Remediation: pass a file key, or
    /// wait for the backend. Never reported as "no such file or directory" --
    /// the refusal happens at the parse boundary, before anything treats the
    /// reference as a path.
    ///
    /// `transparent` rather than a wrapping message: the wrapped
    /// [`KeyRefError`] already names the scheme, and a prefix here would render
    /// the sentence twice under `{err:#}`. Transparent forwards `source()`
    /// *past* the value it wraps, which is harmless here for two reasons --
    /// `KeyRefError` is a leaf with no source of its own, and `exit_code()`
    /// answers for this variant directly instead of delegating to the chain
    /// walker (contrast `CopyErrorKind::Registry`, which must delegate).
    #[error(transparent)]
    UnsupportedKeyBackend(KeyRefError),

    /// The key backend could not produce a signature.
    ///
    /// Exit code is the wrapped [`KeyBackendError`]'s own class, decided by
    /// [`Self::exit_code`] below: unreachable backend → 75 (retry), unreadable
    /// key material → 74, malformed key → 65, recognised-but-unimplemented
    /// backend → 85. A KMS signs over the network, so "the backend was down"
    /// and "the key is wrong" are different operator actions and must not
    /// collapse into one code.
    ///
    /// `#[from]` because the conversion is unambiguous — `KeyBackendError` has
    /// exactly one home in this taxonomy.
    #[error(transparent)]
    KeyBackend(#[from] crate::oci::sign::key_backend::KeyBackendError),

    /// A `--key` reference could not be parsed: an unrecognised scheme token,
    /// or nothing following the scheme.
    ///
    /// Exit 64 (`UsageError`). Remediation: fix the reference. Same
    /// `transparent` reasoning as [`Self::UnsupportedKeyBackend`]; the two are
    /// separate variants because their exit codes and their remedies differ,
    /// and `From<KeyRefError>` is the single place that decides which applies.
    #[error(transparent)]
    KeyReferenceInvalid(KeyRefError),

    /// `--no-rekor-upload` was given for a keyless signature.
    ///
    /// Exit 64 (`UsageError`) -- the flags parse, but the combination asks for
    /// something that cannot be honoured.
    ///
    /// Deliberately **not** a clap `requires = "key"` (plan D-7): clap would
    /// print "the following required arguments were not provided: --key", which
    /// inverts the reason. The reason is the whole point. A Fulcio certificate
    /// is valid for roughly ten minutes, so the Rekor entry's inclusion
    /// timestamp is the only durable proof that the signature was produced
    /// while the certificate still was. Skipping the entry does not make the
    /// signature unverifiable now -- it makes it unverifiable forever, ten
    /// minutes from now. So under keyless the flag is an error and never a
    /// silent no-op.
    ///
    /// Carried G0 constraint, stated here so no later loop re-derives it
    /// backwards: verification anchors certificate validity to that
    /// *signing-time* proof -- the Rekor entry's integrated time -- and never
    /// to wall-clock "is this certificate valid now". A golden keyless fixture
    /// whose certificate expired ten minutes after capture must still verify.
    #[error(
        "--no-rekor-upload requires --key: a keyless signature must be recorded in Rekor, \
         because a Fulcio certificate is valid for about ten minutes and the log entry's \
         timestamp is the only lasting proof the signature was made while it was"
    )]
    RekorUploadRequiredForKeyless,

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
            Self::FulcioUnavailable => ExitCode::TempFail,
            Self::TransparencyLogUnavailable => ExitCode::TransparencyLogUnavailable,
            Self::RekorSetMalformed
            | Self::PredicateNotJson
            | Self::PredicateTooLarge { .. }
            | Self::SubjectDigestUnsupported { .. } => ExitCode::DataError,
            Self::ReferrersUnsupported => ExitCode::ReferrersUnsupported,
            Self::TargetNotFound { .. } | Self::TargetNotAnIndex { .. } => ExitCode::NotFound,
            Self::UnsupportedKeyBackend(_) => ExitCode::UnsupportedKeyBackend,
            // The backend's own class, not a flattened one: a KMS signs over
            // the network, so "unreachable" (retry) and "wrong key" (fix the
            // config) are different operator actions.
            Self::KeyBackend(error) => match error {
                crate::oci::sign::key_backend::KeyBackendError::Unavailable { .. } => ExitCode::TempFail,
                crate::oci::sign::key_backend::KeyBackendError::Io(_) => ExitCode::IoError,
                crate::oci::sign::key_backend::KeyBackendError::MalformedKey { .. } => ExitCode::DataError,
                crate::oci::sign::key_backend::KeyBackendError::Unsupported { .. } => ExitCode::UnsupportedKeyBackend,
            },
            // OfflineAttestRefused shares 77 with OfflineSignRefused by
            // design: one policy, two verbs.
            Self::OidcPreCheckFailed { .. }
            | Self::OfflineSignRefused
            | Self::OfflineAttestRefused
            | Self::IdentityTokenFilePermissive { .. } => ExitCode::PermissionDenied,
            Self::InvalidEndpointUrl { .. }
            | Self::ProvenanceVersionUnsupported { .. }
            | Self::UnsignedTypeUnsupported { .. }
            | Self::SidecarRequiresSignature { .. }
            | Self::KeyReferenceInvalid(_)
            | Self::RekorUploadRequiredForKeyless => ExitCode::UsageError,
            Self::Internal(_) => ExitCode::Failure,
        }
    }

    fn kind_detail(&self) -> &'static str {
        // Frozen contract C-S1-1: snake_case parallel of the variant name.
        // Exhaustive match — no wildcard, so adding a variant forces a new arm.
        match self {
            Self::FulcioBadRequest => "fulcio_bad_request",
            Self::OidcTokenRejected => "oidc_token_rejected",
            Self::FulcioUnavailable => "fulcio_unavailable",
            Self::TransparencyLogUnavailable => "transparency_log_unavailable",
            Self::RekorSetMalformed => "rekor_set_malformed",
            Self::ReferrersUnsupported => "referrers_unsupported",
            Self::TargetNotFound { .. } => "target_not_found",
            Self::TargetNotAnIndex { .. } => "target_not_an_index",
            Self::SubjectDigestUnsupported { .. } => "subject_digest_unsupported",
            Self::OidcPreCheckFailed { .. } => "oidc_pre_check_failed",
            Self::ForbiddenRegistryTarget { .. } => "forbidden_registry_target",
            Self::OfflineSignRefused => "offline_sign_refused",
            Self::IdentityTokenFilePermissive { .. } => "identity_token_file_permissive",
            Self::InvalidEndpointUrl { .. } => "invalid_endpoint_url",
            Self::PredicateNotJson => "predicate_not_json",
            Self::PredicateTooLarge { .. } => "predicate_too_large",
            Self::ProvenanceVersionUnsupported { .. } => "provenance_version_unsupported",
            Self::OfflineAttestRefused => "offline_attest_refused",
            Self::UnsignedTypeUnsupported { .. } => "unsigned_type_unsupported",
            Self::SidecarRequiresSignature { .. } => "sidecar_requires_signature",
            Self::UnsupportedKeyBackend(_) => "unsupported_key_backend",
            Self::KeyBackend(_) => "key_backend",
            Self::KeyReferenceInvalid(_) => "key_reference_invalid",
            Self::RekorUploadRequiredForKeyless => "rekor_upload_required_for_keyless",
            Self::Internal(_) => "internal",
        }
    }
}

/// Select the sign-side variant a `--key` parse failure belongs to.
///
/// The split is the whole reason two variants exist: an unimplemented backend
/// exits 85 with its own `error.kind`, everything else is a malformed
/// reference and exits 64. The match is exhaustive with no wildcard --
/// `KeyRefError` is `#[non_exhaustive]`, but that binds downstream crates
/// only, so in the crate that defines it a new rejection reason is a compile
/// error until it is classified here.
///
/// The error is carried structurally, never through `.to_string()`: its
/// `Display` is what names the offending scheme.
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
    fn transparency_log_unavailable_maps_to_transparency_log_unavailable() {
        assert_eq!(
            SignErrorKind::TransparencyLogUnavailable.exit_code(),
            ExitCode::TransparencyLogUnavailable
        );
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
    fn sign_error_classify_delegates_to_kind() {
        let err = SignError::new(id(), SignErrorKind::TransparencyLogUnavailable);
        assert_eq!(err.classify(), Some(ExitCode::TransparencyLogUnavailable));
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
    fn predicate_content_failures_map_to_data_error() {
        // 65: the file the user named exists and was read, and its *content* is
        // wrong. Contrast `ProvenanceVersionUnsupported` below, where the
        // offending value came from argv.
        assert_eq!(SignErrorKind::PredicateNotJson.exit_code(), ExitCode::DataError);
        assert_eq!(
            SignErrorKind::PredicateTooLarge {
                limit: 1024,
                actual: 2048
            }
            .exit_code(),
            ExitCode::DataError
        );
    }

    #[test]
    fn provenance_version_unsupported_maps_to_usage_error() {
        // 64, not 65. The value came from `--type`, so the remedy is a
        // different flag value; the message names it.
        let kind = SignErrorKind::ProvenanceVersionUnsupported {
            resolved: "https://slsa.dev/provenance/v0.2".into(),
        };
        assert_eq!(kind.exit_code(), ExitCode::UsageError);
        assert!(
            format!("{kind}").contains("--type slsaprovenance1"),
            "the message must name the flag value that fixes it, got: {kind}"
        );
    }

    #[test]
    fn offline_attest_refused_maps_to_permission_denied() {
        // 77, byte-identical to `OfflineSignRefused`: attesting is signing, and
        // a policy refusal must not classify differently depending on which
        // verb reached it. Asserted against its twin rather than against the
        // literal, so the two can never drift apart.
        assert_eq!(
            SignErrorKind::OfflineAttestRefused.exit_code(),
            SignErrorKind::OfflineSignRefused.exit_code()
        );
        assert_eq!(
            SignErrorKind::OfflineAttestRefused.exit_code(),
            ExitCode::PermissionDenied
        );
    }

    #[test]
    fn key_ref_unsupported_scheme_exits_85_with_its_own_category() {
        // T-16 / C-014. Every link is the production one: the real parser
        // produces the error, the real `From` impl picks the variant, the real
        // `classify()` yields the exit code, and the real `from_exit_code`
        // turns that into the envelope's `error.kind`. Nothing is simulated.
        //
        // Asserting the number alone would not discriminate. An arm rewritten
        // to `ErrorCategory::Internal` still exits 85 while the envelope says
        // `"internal"` -- exactly the silent failure the wildcard-free
        // `from_exit_code` exists to expose -- so the *serialized* category is
        // asserted as well.
        use crate::cli::ErrorCategory;
        use crate::oci::sign::KeyRef;

        let rejected = KeyRef::parse("awskms://alias/release").expect_err("awskms has no implementation");
        let error = SignError::new(id(), SignErrorKind::from(rejected));

        let exit = error.classify().expect("an unsupported backend classifies itself");
        assert_eq!(exit, ExitCode::UnsupportedKeyBackend);
        assert_eq!(exit as u8, 85, "the number is what `case $? in 85)` matches");
        assert_eq!(error.kind.kind_detail(), "unsupported_key_backend");

        let category = ErrorCategory::from_exit_code(exit);
        assert_eq!(
            serde_json::to_string(&category).expect("ErrorCategory serializes"),
            "\"unsupported_key_backend\"",
            "envelope error.kind must be the dedicated category, never \"internal\""
        );

        // E-04: the rendered chain names the backend, and never reads as a
        // missing file.
        let rendered = format!("{:#}", anyhow::Error::new(error));
        assert!(
            rendered.contains("awskms"),
            "the message must name the scheme: {rendered}"
        );
        assert!(
            !rendered.contains("No such file"),
            "a recognised backend must never be reported as a missing path: {rendered}"
        );
    }

    #[test]
    fn key_reference_that_is_not_a_backend_is_a_usage_error() {
        // The other half of the `From` split, asserted next to it so the two
        // codes cannot quietly converge: an unrecognised scheme and an empty
        // reference are malformed invocations (64), not unimplemented
        // backends (85).
        use crate::oci::sign::KeyRef;

        for value in ["vault://secret/cosign", "file:"] {
            let kind = SignErrorKind::from(KeyRef::parse(value).expect_err("not a usable key reference"));
            assert_eq!(kind.exit_code(), ExitCode::UsageError, "value: {value}");
            assert_eq!(kind.kind_detail(), "key_reference_invalid", "value: {value}");
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
        assert_eq!(kind.exit_code(), ExitCode::UsageError);
        assert_eq!(kind.kind_detail(), "rekor_upload_required_for_keyless");

        let msg = format!("{kind}");
        assert!(msg.contains("--key"), "must name the flag that makes it legal: {msg}");
        assert!(
            msg.contains("ten minutes"),
            "the certificate window is the reason, and must survive a reword: {msg}"
        );
    }

    #[test]
    fn kind_detail_values_are_stable() {
        // C-S1-1 frozen contract: these strings ship in JSON envelopes and consumer
        // scripts dispatch on them. A rename or typo here is a user-visible breaking
        // change. The exhaustive match in `kind_detail()` ensures a new variant forces
        // a new arm there; this table ensures the *string value* for each arm is pinned.
        use crate::oci::endpoint::UrlRejection;
        use crate::oci::sign::{KeyRefError, Scheme};
        use SignErrorKind::*;

        // Construct one representative instance per variant.
        // Unit/fieldless variants are listed first; struct/tuple variants follow.
        // `Internal` is last because it needs a boxed error allocation.
        let pairs: &[(&'static str, SignErrorKind)] = &[
            ("fulcio_bad_request", FulcioBadRequest),
            ("oidc_token_rejected", OidcTokenRejected),
            ("fulcio_unavailable", FulcioUnavailable),
            ("transparency_log_unavailable", TransparencyLogUnavailable),
            ("rekor_set_malformed", RekorSetMalformed),
            ("referrers_unsupported", ReferrersUnsupported),
            (
                "target_not_found",
                TargetNotFound {
                    platform: "linux/amd64".into(),
                },
            ),
            (
                "target_not_an_index",
                TargetNotAnIndex {
                    platform: "linux/amd64".into(),
                },
            ),
            (
                "subject_digest_unsupported",
                SubjectDigestUnsupported {
                    algorithm: "sha384".into(),
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
            ("predicate_not_json", PredicateNotJson),
            (
                "predicate_too_large",
                PredicateTooLarge {
                    limit: 1024,
                    actual: 2048,
                },
            ),
            (
                "provenance_version_unsupported",
                ProvenanceVersionUnsupported {
                    resolved: "https://slsa.dev/provenance/v0.2".into(),
                },
            ),
            ("offline_attest_refused", OfflineAttestRefused),
            (
                "unsigned_type_unsupported",
                UnsignedTypeUnsupported {
                    predicate_type: "https://slsa.dev/provenance/v1".into(),
                },
            ),
            (
                "sidecar_requires_signature",
                SidecarRequiresSignature {
                    format: crate::oci::sign::SignatureFormat::Simplesigning,
                },
            ),
            (
                "unsupported_key_backend",
                UnsupportedKeyBackend(KeyRefError::UnsupportedBackend { scheme: Scheme::AwsKms }),
            ),
            ("key_reference_invalid", KeyReferenceInvalid(KeyRefError::Empty)),
            (
                "key_backend",
                KeyBackend(crate::oci::sign::key_backend::KeyBackendError::Unavailable { reason: "test".into() }),
            ),
            ("rekor_upload_required_for_keyless", RekorUploadRequiredForKeyless),
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
            25,
            "a row was removed from the table above; restore it rather than lowering this count"
        );

        for (expected, kind) in pairs {
            assert_eq!(kind.kind_detail(), *expected, "kind_detail() drift for {kind:?}",);
        }
    }
}
