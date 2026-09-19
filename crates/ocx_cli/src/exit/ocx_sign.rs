// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Exit-code classification for the signing and verification error family — the `ocx_sign` rung of the
//! ladder, here rather than in that crate because classification is `ocx_cli`'s alone.

use ocx_exit::ExitCode;

use ocx_sign::verify::TrustRootLoadReason;

use ocx_sign::sign::SignError;
use ocx_sign::sign::SignErrorKind;
use ocx_sign::verify::VerifyError;
use ocx_sign::verify::VerifyErrorKind;

use super::{ClassifyErrorKind, ClassifyExitCode, downcast_arm};

impl ClassifyExitCode for VerifyError {
    fn classify(&self) -> Option<ExitCode> {
        match &self.kind {
            // `Internal` means "no verify-side code fits this", not "the code is
            // 1". Answering `Some(Failure)` here would short-circuit the chain
            // walker at the outermost wrapper and discard a cause that already
            // classifies itself -- a registry 401/503/5xx reached through
            // `map_client_error` exits 80/75/69 only because this defers. A cause
            // nothing in the ladder recognizes still lands on `Failure` via
            // `classify_error`'s own fall-through, so the catch-all keeps its
            // catch-all exit code without asserting it.
            VerifyErrorKind::Internal(_) => None,
            kind => Some(kind.exit_code()),
        }
    }
}

impl ClassifyErrorKind for VerifyErrorKind {
    fn exit_code(&self) -> ExitCode {
        match self {
            // AttestationNotFound joins the family for the same reason
            // NoSignaturesFound is here: the scan completed and found nothing
            // to check, which is an absence, not a verification failure.
            Self::NoSignaturesFound
            | Self::NoUsableBundle
            | Self::TargetNotFound { .. }
            | Self::TargetNotAnIndex { .. }
            | Self::AttestationNotFound => ExitCode::NotFound,
            Self::IdentityMismatch | Self::IssuerMismatch | Self::UnsignedRejectedByPolicy => {
                ExitCode::PermissionDenied
            }
            Self::CertChainInvalid
            | Self::SignatureInvalid
            | Self::SubjectDigestMismatch
            | Self::BundleParseFailed
            // RekorSetInvalid and TransparencyBodyMismatch are data integrity
            // failures (tampered/spliced bundle), not service-unavailability
            // signals. Exit 65 so retry logic does not fire.
            | Self::RekorSetInvalid
            | Self::TransparencyBodyMismatch
            | Self::RekorInclusionProofAbsent
            // CandidateLimitExhausted is a fail-closed "could not examine all
            // candidates" outcome, not "unsigned" (79) — keep it in the
            // verification-failed bucket.
            | Self::CandidateLimitExhausted { .. }
            // Every attestation shape, binding and bound failure is 65: the
            // bytes arrived and did not hold up. A retry re-fetches the same
            // bytes, so none of these is transient.
            | Self::PredicateTypeMismatch { .. }
            | Self::StatementSubjectMismatch { .. }
            | Self::StatementSubjectAbsent
            | Self::StatementSubjectWeakAlgorithm { .. }
            | Self::BuilderMismatch { .. }
            | Self::StatementTypeUnsupported { .. }
            | Self::PayloadTypeUnsupported { .. }
            | Self::SimpleSigningClaimUnsupported { .. }
            | Self::MultipleSignatures { .. }
            | Self::MultipleAttestations { .. }
            | Self::UnsupportedTlogEntryKind { .. }
            | Self::TlogBindingMismatch
            | Self::CertificateValidityWindow { .. }
            | Self::SbomMediaTypeUnsupported { .. }
            | Self::AttestationTooLarge { .. }
            | Self::AttestationPayloadTooLarge { .. }
            | Self::TooManyAttestations { .. }
            | Self::AttestationBudgetExhausted { .. } => ExitCode::DataError,
            Self::RekorSetAbsentTsaPresent | Self::TransparencyLogUnavailable => ExitCode::TransparencyLogUnavailable,
            Self::UnsupportedKeyBackend(_) => ExitCode::UnsupportedKeyBackend,
            // Before the flatten below: the same refusal through a second door.
            Self::TrustPolicyInvalid(error) if error.names_unsupported_backend() => {
                ExitCode::UnsupportedKeyBackend
            }
            // The second door again, one class over. A path key reference
            // that cannot be read is a filesystem failure on a path the operator
            // typed, and the `--key` sign door has always answered 74 `io_error`
            // for it (`KeyBackendError::Io`). The flatten below answered 78
            // `config_error` for the identical invocation — a category error:
            // the "scope" its message names is the literal string `--key`, and
            // no config file is involved at all.
            Self::TrustPolicyInvalid(ocx_trust::TrustPolicyError::KeyUnreadable { .. })
            | Self::TrustPolicyInvalid(ocx_trust::TrustPolicyError::KeyMalformed {
                fault: ocx_trust::KeyFault::Path,
                ..
            }) => ExitCode::IoError,
            // A regular file that read fine and holds something that is not a
            // key: the bytes are the problem, which is the 65 `ocx package sign`
            // answers for the same file. An inline `key_pem` falls through to
            // 78 below — there the config text itself is what is wrong.
            Self::TrustPolicyInvalid(ocx_trust::TrustPolicyError::KeyMalformed {
                fault: ocx_trust::KeyFault::FileBytes,
                ..
            }) => ExitCode::DataError,
            // The same carve-out as `KeyUnreadable` above, one flag family
            // over: a trust-root path the operator typed that cannot be read is
            // a filesystem failure, and `--key file:<missing>` already answers
            // 74 for it. The `TrustRootLoad(_)` arm below would otherwise call
            // an operator's typo a configuration error, naming a scope that is
            // a flag rather than any config file.
            Self::TrustRootLoad(TrustRootLoadReason::TrustRootUnreadable { .. }) => ExitCode::IoError,
            Self::TrustRootUnavailable
            | Self::TrustRootLoad(_)
            | Self::TrustPolicyInvalid(_)
            | Self::ForbiddenRegistryTarget { .. } => ExitCode::ConfigError,
            // The rejection verdict was decided in `oci/endpoint.rs`
            // (`UrlRejection` carries an exit code set by its
            // `From<SsrfError>` impl): 69 for an endpoint that does not
            // resolve, else the documented 64. Delegate rather than
            // flattening every rejection to one code.
            Self::InvalidEndpointUrl { reason, .. } => reason.exit_code(),
            Self::NoIdentityProvided | Self::KeyReferenceInvalid(_) => ExitCode::UsageError,
            Self::Internal(_) => ExitCode::Failure,
        }
    }

    fn kind_detail(&self) -> &'static str {
        // Frozen contract C-S1-1: snake_case parallel of the variant name.
        // Exhaustive match — no wildcard, so adding a variant forces a new arm.
        match self {
            Self::NoSignaturesFound => "no_signatures_found",
            Self::TargetNotFound { .. } => "target_not_found",
            Self::TargetNotAnIndex { .. } => "target_not_an_index",
            Self::NoUsableBundle => "no_usable_bundle",
            Self::CandidateLimitExhausted { .. } => "candidate_limit_exhausted",
            Self::IdentityMismatch => "identity_mismatch",
            Self::UnsignedRejectedByPolicy => "unsigned_rejected_by_policy",
            Self::IssuerMismatch => "issuer_mismatch",
            Self::CertChainInvalid => "cert_chain_invalid",
            Self::SignatureInvalid => "signature_invalid",
            Self::SubjectDigestMismatch => "subject_digest_mismatch",
            Self::RekorSetInvalid => "rekor_set_invalid",
            Self::TransparencyBodyMismatch => "transparency_body_mismatch",
            Self::RekorInclusionProofAbsent => "rekor_inclusion_proof_absent",
            Self::RekorSetAbsentTsaPresent => "rekor_set_absent_tsa_present",
            Self::TransparencyLogUnavailable => "transparency_log_unavailable",
            Self::BundleParseFailed => "bundle_parse_failed",
            Self::ForbiddenRegistryTarget { .. } => "forbidden_registry_target",
            Self::TrustRootUnavailable => "trust_root_unavailable",
            Self::TrustRootLoad(TrustRootLoadReason::TrustRootUnreadable { .. }) => "trust_root_unreadable",
            Self::TrustRootLoad(_) => "trust_root_load",
            Self::NoIdentityProvided => "no_identity_provided",
            Self::TrustPolicyInvalid(error) if error.names_unsupported_backend() => "unsupported_key_backend",
            Self::TrustPolicyInvalid(ocx_trust::TrustPolicyError::KeyUnreadable { .. })
            | Self::TrustPolicyInvalid(ocx_trust::TrustPolicyError::KeyMalformed {
                fault: ocx_trust::KeyFault::Path,
                ..
            }) => "key_unreadable",
            Self::TrustPolicyInvalid(ocx_trust::TrustPolicyError::KeyMalformed {
                fault: ocx_trust::KeyFault::FileBytes,
                ..
            }) => "key_malformed",
            Self::TrustPolicyInvalid(_) => "trust_policy_invalid",
            Self::InvalidEndpointUrl { .. } => "invalid_endpoint_url",
            Self::AttestationNotFound => "attestation_not_found",
            Self::PredicateTypeMismatch { .. } => "predicate_type_mismatch",
            Self::StatementSubjectMismatch { .. } => "statement_subject_mismatch",
            Self::StatementSubjectAbsent => "statement_subject_absent",
            Self::StatementSubjectWeakAlgorithm { .. } => "statement_subject_weak_algorithm",
            Self::BuilderMismatch { .. } => "builder_mismatch",
            Self::StatementTypeUnsupported { .. } => "statement_type_unsupported",
            Self::PayloadTypeUnsupported { .. } => "payload_type_unsupported",
            Self::SimpleSigningClaimUnsupported { .. } => "simple_signing_claim_unsupported",
            Self::MultipleSignatures { .. } => "multiple_signatures",
            Self::MultipleAttestations { .. } => "multiple_attestations",
            Self::UnsupportedTlogEntryKind { .. } => "unsupported_tlog_entry_kind",
            Self::TlogBindingMismatch => "tlog_binding_mismatch",
            Self::CertificateValidityWindow { .. } => "certificate_validity_window",
            Self::SbomMediaTypeUnsupported { .. } => "sbom_media_type_unsupported",
            Self::AttestationTooLarge { .. } => "attestation_too_large",
            Self::AttestationPayloadTooLarge { .. } => "attestation_payload_too_large",
            Self::TooManyAttestations { .. } => "too_many_attestations",
            Self::AttestationBudgetExhausted { .. } => "attestation_budget_exhausted",
            Self::UnsupportedKeyBackend(_) => "unsupported_key_backend",
            Self::KeyReferenceInvalid(_) => "key_reference_invalid",
            Self::Internal(_) => "internal",
        }
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
                ocx_sign::sign::key_backend::KeyBackendError::Unavailable { .. } => ExitCode::TempFail,
                ocx_sign::sign::key_backend::KeyBackendError::Io(_) => ExitCode::IoError,
                ocx_sign::sign::key_backend::KeyBackendError::MalformedKey { .. } => ExitCode::DataError,
                ocx_sign::sign::key_backend::KeyBackendError::Unsupported { .. } => ExitCode::UnsupportedKeyBackend,
            },
            // OfflineAttestRefused shares 77 with OfflineSignRefused by
            // design: one policy, two verbs.
            Self::OidcPreCheckFailed { .. }
            | Self::OfflineSignRefused
            | Self::OfflineAttestRefused
            | Self::IdentityTokenFilePermissive { .. } => ExitCode::PermissionDenied,
            // The rejection verdict was decided in `oci/endpoint.rs`
            // (`UrlRejection` carries an exit code set by its
            // `From<SsrfError>` impl): 69 for an endpoint that does not
            // resolve, else the documented 64. Delegate rather than
            // flattening every rejection to one code.
            Self::InvalidEndpointUrl { reason, .. } => reason.exit_code(),
            Self::ProvenanceVersionUnsupported { .. }
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

pub(super) fn try_downcast(cause: &(dyn std::error::Error + 'static)) -> Option<ExitCode> {
    downcast_arm!(cause, SignError);
    downcast_arm!(cause, VerifyError);
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use ocx_oci::client::error::ClientError;

    use ocx_oci::identifier::Identifier;

    use ocx_trust::key_ref::KeyRefError;

    // ── moved from ocx_lib::oci::attest::pipeline with the impl ──

    // ── moved from ocx_lib::oci::sign::error with the impl ──

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

    /// Plan row 12 (sign half): a Sigstore endpoint whose host does not resolve
    /// exits `Unavailable` (69).
    ///
    /// The flag value is well-formed; the service behind it is unreachable, so
    /// reporting CLI misuse tells a script to fix its arguments when the
    /// correct response is to retry or check the network. The rejection built
    /// from an `SsrfError` already carries that verdict — the variant's arm
    /// must read it rather than answer 64 for every rejection alike.
    #[test]
    fn an_endpoint_url_that_does_not_resolve_maps_to_unavailable() {
        use ocx_oci::endpoint::UrlRejection;
        let kind = SignErrorKind::InvalidEndpointUrl {
            endpoint: "--fulcio-url".into(),
            reason: UrlRejection::from(ocx_oci::ssrf::SsrfError::Resolution {
                host: "fulcio.invalid".to_string(),
                source: std::io::Error::new(std::io::ErrorKind::NotFound, "name or service not known"),
            }),
        };
        assert_eq!(kind.exit_code(), ExitCode::Unavailable);
    }

    /// Plan row 12 (sign half), paired positive: every rejection verdict other
    /// than an unresolvable host keeps the documented `UsageError` (64).
    ///
    /// Guards the fix against over-reach — delegating to the carried verdict
    /// must not import the *registry* guard's table, where a forbidden target
    /// is 78. On the Sigstore side a refused endpoint URL stays 64.
    #[test]
    fn an_endpoint_url_refused_as_a_forbidden_target_stays_a_usage_error() {
        use ocx_oci::endpoint::UrlRejection;
        let kind = SignErrorKind::InvalidEndpointUrl {
            endpoint: "--fulcio-url".into(),
            reason: UrlRejection::from(ocx_oci::ssrf::SsrfError::ForbiddenTarget {
                host: "fulcio.internal".to_string(),
                ip: std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            }),
        };
        assert_eq!(kind.exit_code(), ExitCode::UsageError);
    }

    #[test]
    fn internal_maps_to_failure() {
        // Unclassified errors fall through to Failure (generic).
        let inner: Box<dyn std::error::Error + Send + Sync> = "kaboom".into();
        let kind = SignErrorKind::Internal(inner);
        assert_eq!(kind.exit_code(), ExitCode::Failure);
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
    fn key_reference_that_is_not_a_backend_is_a_usage_error() {
        // The other half of the `From` split, asserted next to it so the two
        // codes cannot quietly converge: an unrecognised scheme and an empty
        // reference are malformed invocations (64), not unimplemented
        // backends (85).
        use ocx_trust::key_ref::KeyRef;

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
        use SignErrorKind::*;
        use ocx_oci::endpoint::UrlRejection;
        use ocx_trust::key_ref::{KeyRefError, Scheme};

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
                    reason: UrlRejection::new("URL must use HTTPS"),
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
                    format: ocx_sign::sign::SignatureFormat::Simplesigning,
                },
            ),
            (
                "unsupported_key_backend",
                UnsupportedKeyBackend(KeyRefError::UnsupportedBackend { scheme: Scheme::AwsKms }),
            ),
            ("key_reference_invalid", KeyReferenceInvalid(KeyRefError::Empty)),
            (
                "key_backend",
                KeyBackend(ocx_sign::sign::key_backend::KeyBackendError::Unavailable { reason: "test".into() }),
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

    // ── moved from ocx_lib::oci::sign::fulcio with the impl ──

    // ── moved from ocx_lib::oci::sign::key_signer with the impl ──

    // ── moved from ocx_lib::oci::sign::pipeline with the impl ──

    // ── moved from ocx_lib::oci::verify::error with the impl ──

    #[test]
    fn not_found_family_maps_to_not_found_exit() {
        // "not signed" signal — publisher never signed or signed a different
        // platform — plus "not here at all": `TargetNotFound` shares the code
        // (79) precisely because it must never share the slug, so its exit code
        // needs pinning next to the family it sits in.
        for kind in [
            VerifyErrorKind::NoSignaturesFound,
            VerifyErrorKind::NoUsableBundle,
            VerifyErrorKind::TargetNotFound {
                platform: "linux/amd64".into(),
            },
            VerifyErrorKind::TargetNotAnIndex {
                platform: "linux/amd64".into(),
            },
        ] {
            assert_eq!(kind.exit_code(), ExitCode::NotFound, "variant: {kind:?}");
        }
    }

    #[test]
    fn candidate_limit_exhausted_maps_to_data_error() {
        // Fail-closed: the cap was hit with candidates unexamined and none passed.
        // 65 (DataError), not 79 (NotFound) — candidates exist, so "not signed"
        // would misreport a possibly-signed artifact.
        assert_eq!(
            VerifyErrorKind::CandidateLimitExhausted { unexamined: 3 }.exit_code(),
            ExitCode::DataError
        );
    }

    #[test]
    fn identity_family_maps_to_permission_denied() {
        // 77 = "you verified, but not by the signer you expected".
        assert_eq!(
            VerifyErrorKind::IdentityMismatch.exit_code(),
            ExitCode::PermissionDenied
        );
        assert_eq!(VerifyErrorKind::IssuerMismatch.exit_code(), ExitCode::PermissionDenied);
        // An unsigned attachment under a policy that demands a signature is
        // the same class of refusal as the wrong signer, and scripts branch on
        // 77 for exactly that: "the artifact exists, the trust decision said
        // no". 79 would tell them to go looking for an attach that happened.
        assert_eq!(
            VerifyErrorKind::UnsignedRejectedByPolicy.exit_code(),
            ExitCode::PermissionDenied
        );
    }

    #[test]
    fn data_error_family_maps_to_data_error() {
        // 65 = "something in the bundle doesn't verify or doesn't parse".
        for kind in [
            VerifyErrorKind::CertChainInvalid,
            VerifyErrorKind::SignatureInvalid,
            // A registry serving bytes that do not hash to the resolved digest is
            // corrupt or hostile, never retryable — 65, not a transient code.
            VerifyErrorKind::SubjectDigestMismatch,
            VerifyErrorKind::BundleParseFailed,
        ] {
            assert_eq!(kind.exit_code(), ExitCode::DataError, "variant: {kind:?}");
        }
    }

    #[test]
    fn rekor_set_invalid_maps_to_data_error() {
        // RekorSetInvalid is a tampered-bundle / crypto failure — exit 65 (DataError),
        // NOT exit 83 (TransparencyLogUnavailable). A `case $? in 83) retry` handler must not
        // retry a tampered SET.
        assert_eq!(VerifyErrorKind::RekorSetInvalid.exit_code(), ExitCode::DataError);
    }

    #[test]
    fn transparency_body_mismatch_maps_to_data_error() {
        // A spliced SET/body (GHSA-whqx class) is a tampered-bundle failure — exit
        // 65 (DataError), same class as SignatureInvalid. Never a retryable fault.
        assert_eq!(
            VerifyErrorKind::TransparencyBodyMismatch.exit_code(),
            ExitCode::DataError
        );
    }

    #[test]
    fn transparency_log_unavailable_family_maps_to_transparency_log_unavailable() {
        // 83 = "Rekor service unreachable or TSA transition" — retry may help.
        for kind in [
            VerifyErrorKind::RekorSetAbsentTsaPresent,
            VerifyErrorKind::TransparencyLogUnavailable,
        ] {
            assert_eq!(
                kind.exit_code(),
                ExitCode::TransparencyLogUnavailable,
                "variant: {kind:?}"
            );
        }
    }

    #[test]
    fn trust_root_unavailable_maps_to_config_error() {
        assert_eq!(VerifyErrorKind::TrustRootUnavailable.exit_code(), ExitCode::ConfigError);
    }

    #[test]
    fn no_identity_provided_maps_to_usage_error() {
        // 64 = "you invoked verify without telling it whose signature to trust"
        // (no flags, no matching [trust.policy]) — continuity with the prior
        // required-flag behavior.
        assert_eq!(VerifyErrorKind::NoIdentityProvided.exit_code(), ExitCode::UsageError);
    }

    #[test]
    fn trust_policy_invalid_maps_to_config_error() {
        let kind = VerifyErrorKind::TrustPolicyInvalid(ocx_trust::TrustPolicyError::IdentityUnset {
            scope: "ghcr.io/acme/*".into(),
        });
        assert_eq!(kind.exit_code(), ExitCode::ConfigError);
    }

    /// `--key awskms://alias/release` and `key = "awskms://alias/release"` in a
    /// `[[trust.policy]]` signer are one refusal through two doors: both build
    /// [`KeyRefError::UnsupportedBackend`]. The flag door has always answered 85
    /// `unsupported_key_backend`; the config door flattened onto 78
    /// `config_error`, telling a fleet script "your config is malformed" for a
    /// backend that is simply not built yet.
    ///
    /// The second half is the discriminator, and the reason both halves live in
    /// one test: a trust-policy refusal that is NOT the backend verdict must
    /// still be 78 `trust_policy_invalid`, or the guard has reclassified the
    /// whole family instead of carving out one case.
    #[test]
    fn an_unsupported_key_backend_named_in_a_trust_policy_maps_to_85_not_78() {
        let kms = VerifyErrorKind::TrustPolicyInvalid(ocx_trust::TrustPolicyError::KeyReferenceInvalid {
            scope: "ghcr.io/acme/*".into(),
            source: KeyRefError::UnsupportedBackend {
                scheme: ocx_trust::key_ref::Scheme::AwsKms,
            },
        });
        assert_eq!(
            kms.exit_code(),
            ExitCode::UnsupportedKeyBackend,
            "the same 85 the `--key awskms://…` door already answers"
        );
        assert_eq!(kms.kind_detail(), "unsupported_key_backend");

        let unrelated = VerifyErrorKind::TrustPolicyInvalid(ocx_trust::TrustPolicyError::IssuerUnset {
            scope: "ghcr.io/acme/*".into(),
        });
        assert_eq!(
            unrelated.exit_code(),
            ExitCode::ConfigError,
            "a trust-policy error that is not the backend verdict stays 78"
        );
        assert_eq!(unrelated.kind_detail(), "trust_policy_invalid");
    }

    /// `--key /nope` is one refusal through two doors, exactly as the
    /// backend case above: sign reads the same reference through
    /// `KeyBackendError::Io` and exits 74 `io_error`, while verify read it
    /// through `read_key_file` and flattened onto 78 `config_error`. Same flag,
    /// same value, two codes — and 78 is a category error here, because the
    /// "scope" the message names is the literal string `--key`, not a file.
    ///
    /// All four rows together, because each is only meaningful against the
    /// others: a classifier that answered one code for the whole key family
    /// would satisfy any one of them alone. One rule, keyed on *what* was
    /// unusable and never on which command asked — the path, the file's bytes,
    /// or the config text.
    #[test]
    fn a_key_failure_is_classified_by_what_was_unusable() {
        use ocx_trust::{KeyFault, TrustPolicyError};

        let malformed = |fault| {
            VerifyErrorKind::TrustPolicyInvalid(TrustPolicyError::KeyMalformed {
                scope: "--key".into(),
                reason: "not a PEM-encoded public key".into(),
                fault,
            })
        };

        let unreadable = VerifyErrorKind::TrustPolicyInvalid(TrustPolicyError::KeyUnreadable {
            scope: "--key".into(),
            path: std::path::PathBuf::from("/nope"),
            source: std::io::Error::from(std::io::ErrorKind::NotFound),
        });
        assert_eq!(
            unreadable.exit_code(),
            ExitCode::IoError,
            "the same 74 `ocx package sign --key /nope` already answers"
        );
        assert_eq!(unreadable.kind_detail(), "key_unreadable");

        // A directory, or a character device: the path named something that is
        // not a readable regular file, which is the same 74 `--config` already
        // promises for a path that "exists but cannot be read".
        assert_eq!(malformed(KeyFault::Path).exit_code(), ExitCode::IoError);
        assert_eq!(malformed(KeyFault::Path).kind_detail(), "key_unreadable");

        // A regular file read in full whose bytes are not a key — 65, what sign
        // answers for the same file.
        assert_eq!(malformed(KeyFault::FileBytes).exit_code(), ExitCode::DataError);
        assert_eq!(malformed(KeyFault::FileBytes).kind_detail(), "key_malformed");

        // An inline `key_pem`: no path and no file, so the config text is the
        // thing that is wrong and 78 stays right.
        assert_eq!(malformed(KeyFault::ConfigText).exit_code(), ExitCode::ConfigError);
        assert_eq!(malformed(KeyFault::ConfigText).kind_detail(), "trust_policy_invalid");
    }

    #[test]
    fn invalid_endpoint_url_maps_to_usage_error() {
        use ocx_oci::endpoint::UrlRejection;
        // Verify side borrows its own InvalidEndpointUrl variant so the exit-code
        // classification is independent of the sign side.
        let kind = VerifyErrorKind::InvalidEndpointUrl {
            endpoint: "--rekor-url".into(),
            reason: UrlRejection::new("URL must use HTTPS"),
        };
        assert_eq!(kind.exit_code(), ExitCode::UsageError);
    }

    #[test]
    fn trust_root_load_maps_to_config_error() {
        // Every TrustRootLoadReason variant EXCEPT `TrustRootUnreadable` produces
        // ConfigError. ADR §C-S1-2: trust root failures are configuration-layer,
        // not runtime faults — but a path the operator typed is a filesystem
        // failure, which is the carve-out the sibling test below pins.
        //
        // The list is the whole enum minus that one variant, deliberately: the
        // comment used to say "every" over a list that was missing two, so a
        // variant added without an arm would have been invisible here.
        //
        // Asset-read failures carry a boxed source — construct one via a synthetic
        // io::Error so the source-carrying branch is also covered.
        let asset_read_source: Box<dyn std::error::Error + Send + Sync> =
            Box::new(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "synthetic"));
        let reasons: Vec<TrustRootLoadReason> = vec![
            TrustRootLoadReason::EmbeddedAssetMissing,
            TrustRootLoadReason::AssetReadFailed {
                source: asset_read_source,
            },
            TrustRootLoadReason::TufFetchFailed { status: 503 },
            TrustRootLoadReason::TufFetchTimeout,
            TrustRootLoadReason::PemParseFailed {
                detail: "unexpected block label".into(),
            },
            TrustRootLoadReason::NoCtLogKey,
            TrustRootLoadReason::NoCertificateBlocks,
            TrustRootLoadReason::AmbiguousTrustRootConfig,
            TrustRootLoadReason::OfflineTrustMaterialUnavailable,
        ];
        for reason in reasons {
            let kind = VerifyErrorKind::TrustRootLoad(reason);
            assert_eq!(kind.exit_code(), ExitCode::ConfigError, "variant: {kind:?}");
        }
    }

    /// C-012/C-013. A trust-root path the operator typed exits 74, matching
    /// `--key file:<missing>`; the two sites that are not file reads keep 78.
    ///
    /// Both halves in one test, because the interesting failure is not "74 is
    /// wrong" but "both are 78 again" — a regression that a 74-only assertion
    /// catches and a 78-only assertion does not, and vice versa. The 78 half is
    /// `AssetReadFailed`, raised by `TrustRoot::load_embedded` when the TUF
    /// fetch produces no root and by `Verifier::new` when the assembled root is
    /// unusable. Neither opens a file the operator named.
    #[test]
    fn an_unreadable_trust_root_file_maps_to_io_error_while_the_tuf_sites_keep_config_error() {
        let missing: Box<dyn std::error::Error + Send + Sync> =
            Box::new(std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"));
        let unreadable = VerifyErrorKind::TrustRootLoad(TrustRootLoadReason::TrustRootUnreadable { source: missing });
        assert_eq!(unreadable.exit_code(), ExitCode::IoError);
        assert_eq!(unreadable.kind_detail(), "trust_root_unreadable");

        let tuf: Box<dyn std::error::Error + Send + Sync> =
            Box::new(std::io::Error::other("TUF trust-root fetch failed"));
        let not_a_file_read = VerifyErrorKind::TrustRootLoad(TrustRootLoadReason::AssetReadFailed { source: tuf });
        assert_eq!(not_a_file_read.exit_code(), ExitCode::ConfigError);
        assert_eq!(not_a_file_read.kind_detail(), "trust_root_load");
    }

    #[test]
    fn attestation_not_found_maps_to_not_found() {
        // 79, the same code `NoSignaturesFound` uses and for the same reason:
        // the scan completed and found nothing to check. Never 65 — "we looked
        // and there is no attestation" is not "an attestation failed to verify",
        // and a gate script that treats the two alike either blocks every
        // unattested artifact or accepts every broken one.
        assert_eq!(VerifyErrorKind::AttestationNotFound.exit_code(), ExitCode::NotFound);
    }

    #[test]
    fn key_reference_slugs_match_the_sign_side_exactly() {
        // The invariant that makes `error.kind` readable by a script that does
        // not know which verb failed. Asserted against the sign-side function
        // rather than against a literal, so the two can never drift apart while
        // both still "pass their own table".
        use ocx_sign::sign::SignErrorKind;
        use ocx_trust::key_ref::KeyRef;

        for value in ["awskms://alias/release", "vault://secret/cosign"] {
            let rejection = || KeyRef::parse(value).expect_err("not a usable key reference");
            assert_eq!(
                VerifyErrorKind::from(rejection()).kind_detail(),
                SignErrorKind::from(rejection()).kind_detail(),
                "one failure must read as one word on both paths: {value}"
            );
        }
    }

    // ── moved from ocx_lib::oci::verify::pipeline with the impl ──

    // ── moved from ocx_trust with the impl ──

    /// `ocx_sign` spells `OCX_KEY_PASSWORD` for itself, because the crate map
    /// puts it below `ocx_config`. This is the pin that keeps the two spellings
    /// one value: it lives in the application layer, the only tier allowed to
    /// name both, and it compares the literals rather than restating either.
    #[test]
    fn the_signing_key_password_variable_is_spelled_once() {
        assert_eq!(
            ocx_sign::sign::key_backend::OCX_KEY_PASSWORD,
            ocx_config::env::keys::OCX_KEY_PASSWORD,
            "the signing-side constant drifted from the env vocabulary it mirrors"
        );
        assert_eq!(ocx_sign::sign::key_backend::OCX_KEY_PASSWORD, "OCX_KEY_PASSWORD");
    }

    /// A malformed project `ocx.toml` is an operator's typo, and must exit 78
    /// `trust_policy_invalid` — the same code the identical malformation in
    /// `config.toml` has always produced.
    ///
    /// The load-bearing half is the *classifier*, not the variant name. A bare
    /// `toml::de::Error` is a foreign type with no rung in the downcast ladder,
    /// so bubbling it fell through to exit 1 `internal` and reported the typo as
    /// an ocx bug. So the exit code is taken through the ladder over the wrapper
    /// its one caller builds, not through a direct `exit_code()` call: exit 1 is
    /// produced by the ladder finding nothing, which only the ladder can show.
    #[test]
    fn a_malformed_ocx_toml_classifies_as_a_trust_policy_refusal_not_an_internal_error() {
        let error = ocx_trust::policies_from_ocx_toml(
            "[[trust.policy]]\nscope = \"ghcr.io/acme/*\n",
            std::path::Path::new("/project"),
        )
        .expect_err("a document that is not valid TOML must be refused");
        assert!(
            matches!(error, ocx_trust::TrustPolicyError::DocumentInvalid { .. }),
            "the refusal must be typed, not a bare toml::de::Error: {error:?}"
        );
        let kind = VerifyErrorKind::TrustPolicyInvalid(error);
        assert_eq!(kind.kind_detail(), "trust_policy_invalid");
        assert_eq!(
            crate::exit::classify_library_error(&VerifyError::new(sign_id(), kind)),
            ExitCode::ConfigError,
            "exit 78 must come out of the ladder, not out of a direct exit_code() call"
        );
    }

    // ── moved from ocx_lib::oci::attest::pipeline with the impl ──

    // ── moved from ocx_lib::oci::sign::pipeline with the impl ──

    /// The other half of the deferral contract: an `Internal` whose cause no
    /// classifier recognizes must still exit 1, via `classify_error`'s
    /// fall-through rather than an assertion at the wrapper.
    #[test]
    fn unclassifiable_internal_still_exits_failure_through_sign() {
        let kind = SignErrorKind::Internal("something no classifier knows".into());
        assert_eq!(
            crate::exit::classify_library_error(&SignError::new(sign_id(), kind)),
            ExitCode::Failure
        );
    }

    // ── moved from ocx_lib::oci::verify::pipeline with the impl ──

    // fixture from ocx_lib (oci/sign/pipeline)
    fn sign_id() -> Identifier {
        Identifier::parse("registry.example/pkg:1.0").expect("parse test identifier")
    }

    // ── moved from ocx_lib::oci::attest::pipeline with the impl ──

    /// A registry fault during attach must reach the operator as the registry's
    /// own exit code, not the catch-all 1 — the same deferral contract the sign
    /// pipeline holds.
    #[test]
    fn registry_faults_keep_their_own_exit_codes_through_attest() {
        let identifier = Identifier::parse("registry.example/pkg:1.0").expect("identifier");
        let cases = [
            (
                ClientError::RegistryTransient(Box::new(std::io::Error::other("503 from registry"))),
                ExitCode::TempFail,
            ),
            (
                ClientError::Authentication(Box::new(std::io::Error::other("401 from registry"))),
                ExitCode::AuthError,
            ),
        ];
        for (client_error, expected) in cases {
            let rendered = client_error.to_string();
            let err = SignError::new(identifier.clone(), ocx_sign::sign::map_client_error(client_error));
            assert_eq!(
                crate::exit::classify_library_error(&err),
                expected,
                "client error: {rendered}"
            );
        }
    }

    // ── moved from ocx_lib::oci::sign::error with the impl ──

    #[test]
    fn sign_error_classify_delegates_to_kind() {
        let err = SignError::new(id(), SignErrorKind::TransparencyLogUnavailable);
        assert_eq!(err.classify(), Some(ExitCode::TransparencyLogUnavailable));
    }

    // ── moved from ocx_lib::oci::sign::pipeline with the impl ──

    /// A registry fault during signing must reach the operator as the registry's
    /// own exit code, not the catch-all 1.
    ///
    /// Failure this pins: `map_client_error` sinks every non-referrers
    /// `ClientError` into `SignErrorKind::Internal`, and `SignError::classify`
    /// used to answer `Some(Failure)` unconditionally -- so the outer wrapper
    /// short-circuited its own cause and a CI pipeline written to the documented
    /// contract (`case $? in 75) retry;; 80) refresh-creds;;`) never fired.
    /// Both halves are asserted together on purpose: a test that constructed
    /// `Internal` by hand would still pass if `map_client_error` stopped
    /// producing it.
    #[test]
    fn registry_faults_keep_their_own_exit_codes_through_sign() {
        let cases = [
            (
                ClientError::RegistryTransient(Box::new(std::io::Error::other("503 from registry"))),
                ExitCode::TempFail,
            ),
            (
                ClientError::Authentication(Box::new(std::io::Error::other("401 from registry"))),
                ExitCode::AuthError,
            ),
            (
                ClientError::Registry(Box::new(std::io::Error::other("registry said no"))),
                ExitCode::Unavailable,
            ),
        ];
        for (client_error, expected) in cases {
            let rendered = client_error.to_string();
            let err = SignError::new(sign_id(), ocx_sign::sign::map_client_error(client_error));
            assert_eq!(
                crate::exit::classify_library_error(&err),
                expected,
                "client error: {rendered}"
            );
        }
    }

    // ── moved from ocx_lib::oci::verify::error with the impl ──

    #[test]
    fn attestation_failures_map_to_data_error() {
        // 65 across the whole family: shape failures, binding failures and
        // resource-limit trips alike. The bytes arrived and did not hold up, so
        // a retry re-fetches the same bytes — none of these is transient, and
        // none may reach a code a caller retries on.
        for kind in attestation_data_error_kinds() {
            assert_eq!(kind.exit_code(), ExitCode::DataError, "variant: {kind:?}");
        }
    }

    #[test]
    fn verify_error_classify_delegates_to_kind() {
        let err = VerifyError::new(id(), VerifyErrorKind::IssuerMismatch);
        assert_eq!(err.classify(), Some(ExitCode::PermissionDenied));
    }

    // ── moved from ocx_lib::oci::verify::pipeline with the impl ──

    /// A registry fault during verification must reach the operator as the
    /// registry's own exit code, not the catch-all 1.
    ///
    /// Failure this pins: `map_client_error` sinks every client error it does
    /// not name into `VerifyErrorKind::Internal`, and `VerifyError::classify`
    /// used to answer `Some(Failure)` unconditionally -- so the outer wrapper
    /// short-circuited its own cause, and `ocx package verify` against a
    /// registry returning 503 exited 1 with `"kind":"internal"`. Both halves are
    /// asserted together on purpose: a test that constructed `Internal` by hand
    /// would still pass if `map_client_error` stopped producing it.
    #[test]
    fn registry_faults_keep_their_own_exit_codes_through_verify() {
        let cases = [
            (
                ClientError::RegistryTransient(Box::new(std::io::Error::other("503 from registry"))),
                ExitCode::TempFail,
            ),
            (
                ClientError::Authentication(Box::new(std::io::Error::other("401 from registry"))),
                ExitCode::AuthError,
            ),
            (
                ClientError::Registry(Box::new(std::io::Error::other("registry said no"))),
                ExitCode::Unavailable,
            ),
        ];
        for (client_error, expected) in cases {
            let rendered = client_error.to_string();
            let err = VerifyError::new(verify_id(), ocx_sign::verify::pipeline::map_client_error(client_error));
            assert_eq!(
                crate::exit::classify_library_error(&err),
                expected,
                "client error: {rendered}"
            );
        }
    }

    /// The other half of the deferral contract: an `Internal` whose cause no
    /// classifier recognizes must still exit 1, via `classify_error`'s
    /// fall-through rather than an assertion at the wrapper.
    #[test]
    fn unclassifiable_internal_still_exits_failure_through_verify() {
        let kind = VerifyErrorKind::Internal("something no classifier knows".into());
        assert_eq!(
            crate::exit::classify_library_error(&VerifyError::new(verify_id(), kind)),
            ExitCode::Failure
        );
    }

    // fixture from ocx_lib (oci/sign/error)
    fn id() -> Identifier {
        Identifier::parse("registry.example/pkg:1.0").expect("parse test identifier")
    }

    // fixture from ocx_lib (oci/verify/error)
    /// Every attestation variant, so the family below is a full enumeration
    /// rather than whichever ones came to mind. `BuilderMismatch` appears with
    /// `found: Some(..)` here and with `found: None` in the slug table, so both
    /// arms of its format expression are constructed somewhere.
    fn attestation_data_error_kinds() -> Vec<VerifyErrorKind> {
        vec![
            VerifyErrorKind::PredicateTypeMismatch {
                expected: "https://slsa.dev/provenance/v1".into(),
                actual: "https://spdx.dev/Document".into(),
            },
            VerifyErrorKind::StatementSubjectMismatch {
                expected: "sha256:aaaa".into(),
                actual: "sha256:bbbb".into(),
            },
            VerifyErrorKind::StatementSubjectAbsent,
            VerifyErrorKind::StatementSubjectWeakAlgorithm {
                algorithms: vec!["sha1".into()],
            },
            VerifyErrorKind::BuilderMismatch {
                expected: "https://github.com/acme/.github/workflows/release.yml".into(),
                found: Some("https://github.com/evil/.github/workflows/release.yml".into()),
            },
            VerifyErrorKind::StatementTypeUnsupported {
                statement_type: "https://in-toto.io/Statement/v0.1".into(),
            },
            VerifyErrorKind::PayloadTypeUnsupported {
                payload_type: "application/json".into(),
            },
            VerifyErrorKind::MultipleSignatures { count: 2 },
            VerifyErrorKind::MultipleAttestations {
                predicate_types: vec!["https://spdx.dev/Document".into()],
                referrer_digests: vec!["sha256:aaaa".into(), "sha256:bbbb".into()],
            },
            VerifyErrorKind::UnsupportedTlogEntryKind {
                kind: "dsse".into(),
                version: "0.0.1".into(),
            },
            VerifyErrorKind::TlogBindingMismatch,
            VerifyErrorKind::CertificateValidityWindow {
                integrated_time: "2026-01-01T00:00:00Z".into(),
                not_before: "2026-02-01T00:00:00Z".into(),
                not_after: "2026-02-01T00:10:00Z".into(),
            },
            VerifyErrorKind::AttestationTooLarge {
                limit: 1024,
                actual: 2048,
            },
            VerifyErrorKind::AttestationPayloadTooLarge {
                limit: 1024,
                actual: 4096,
            },
            VerifyErrorKind::TooManyAttestations { limit: 32 },
            VerifyErrorKind::AttestationBudgetExhausted { limit: 65_536 },
        ]
    }

    // fixture from ocx_lib (oci/verify/pipeline)
    fn verify_id() -> Identifier {
        Identifier::parse("registry.example/pkg:1.0").expect("parse test identifier")
    }

    // recovered from ocx_lib::oci::sign::error
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
        use ocx_exit::ErrorCategory;
        use ocx_trust::key_ref::KeyRef;

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

    // recovered from ocx_lib::oci::verify::error
    #[test]
    fn verify_key_ref_unsupported_scheme_exits_85_with_its_own_category() {
        // T-16 / C-015, the verify twin. Verify parses `--key` on its own path,
        // so it must reach 85 without borrowing the sign-side error -- which is
        // exactly what this asserts, end to end through the production chain:
        // real parser, real `From` impl, real `classify()`, real
        // `from_exit_code`.
        //
        // The serialized category is asserted, not just the number. An arm
        // rewritten to `ErrorCategory::Internal` still exits 85 while the
        // envelope says `"internal"`, and a test that checked only the number
        // would pass through that.
        use ocx_exit::ErrorCategory;
        use ocx_trust::key_ref::KeyRef;

        let rejected = KeyRef::parse("awskms://alias/release").expect_err("awskms has no implementation");
        let error = VerifyError::new(id(), VerifyErrorKind::from(rejected));

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

    // recovered from ocx_lib::oci::verify::error
    #[test]
    fn verify_kind_detail_values_are_stable() {
        // C-S1-1 frozen contract: these strings ship in JSON envelopes and consumer
        // scripts dispatch on them. A rename or typo here is a user-visible breaking
        // change. The exhaustive match in `kind_detail()` ensures a new variant forces
        // a new arm there; this table ensures the *string value* for each arm is pinned.
        use VerifyErrorKind::*;
        use ocx_oci::endpoint::UrlRejection;
        use ocx_trust::key_ref::Scheme;

        // Construct one representative instance per variant.
        // `TrustRootLoad` carries a `TrustRootLoadReason`; use the simplest variant.
        // `InvalidEndpointUrl` carries a `UrlRejection` borrowed from the sign module.
        let pairs: &[(&'static str, VerifyErrorKind)] = &[
            ("no_signatures_found", NoSignaturesFound),
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
            ("no_usable_bundle", NoUsableBundle),
            ("candidate_limit_exhausted", CandidateLimitExhausted { unexamined: 2 }),
            ("identity_mismatch", IdentityMismatch),
            ("unsigned_rejected_by_policy", UnsignedRejectedByPolicy),
            ("issuer_mismatch", IssuerMismatch),
            ("cert_chain_invalid", CertChainInvalid),
            ("signature_invalid", SignatureInvalid),
            ("subject_digest_mismatch", SubjectDigestMismatch),
            ("rekor_set_invalid", RekorSetInvalid),
            ("transparency_body_mismatch", TransparencyBodyMismatch),
            ("rekor_inclusion_proof_absent", RekorInclusionProofAbsent),
            ("rekor_set_absent_tsa_present", RekorSetAbsentTsaPresent),
            ("transparency_log_unavailable", TransparencyLogUnavailable),
            ("bundle_parse_failed", BundleParseFailed),
            (
                "forbidden_registry_target",
                ForbiddenRegistryTarget {
                    reason: "host resolves into a forbidden range".into(),
                },
            ),
            ("trust_root_unavailable", TrustRootUnavailable),
            ("no_identity_provided", NoIdentityProvided),
            (
                "trust_policy_invalid",
                TrustPolicyInvalid(ocx_trust::TrustPolicyError::IdentityUnset {
                    scope: "ghcr.io/acme/*".into(),
                }),
            ),
            (
                "trust_root_load",
                TrustRootLoad(TrustRootLoadReason::EmbeddedAssetMissing),
            ),
            (
                "trust_root_unreadable",
                TrustRootLoad(TrustRootLoadReason::TrustRootUnreadable {
                    source: Box::new(std::io::Error::new(std::io::ErrorKind::NotFound, "no such file")),
                }),
            ),
            (
                "invalid_endpoint_url",
                InvalidEndpointUrl {
                    endpoint: "--rekor-url".into(),
                    reason: UrlRejection::new("URL must use HTTPS"),
                },
            ),
            ("attestation_not_found", AttestationNotFound),
            (
                "predicate_type_mismatch",
                PredicateTypeMismatch {
                    expected: "https://slsa.dev/provenance/v1".into(),
                    actual: "https://spdx.dev/Document".into(),
                },
            ),
            (
                "statement_subject_mismatch",
                StatementSubjectMismatch {
                    expected: "sha256:aaaa".into(),
                    actual: "sha256:bbbb".into(),
                },
            ),
            ("statement_subject_absent", StatementSubjectAbsent),
            (
                "statement_subject_weak_algorithm",
                StatementSubjectWeakAlgorithm {
                    algorithms: vec!["sha1".into()],
                },
            ),
            (
                "builder_mismatch",
                BuilderMismatch {
                    expected: "https://github.com/acme/.github/workflows/release.yml".into(),
                    found: None,
                },
            ),
            (
                "statement_type_unsupported",
                StatementTypeUnsupported {
                    statement_type: "https://in-toto.io/Statement/v0.1".into(),
                },
            ),
            (
                "payload_type_unsupported",
                PayloadTypeUnsupported {
                    payload_type: "application/json".into(),
                },
            ),
            (
                "simple_signing_claim_unsupported",
                SimpleSigningClaimUnsupported {
                    claim_type: "cosign container image attestation".into(),
                },
            ),
            ("multiple_signatures", MultipleSignatures { count: 2 }),
            (
                "multiple_attestations",
                MultipleAttestations {
                    predicate_types: vec!["https://spdx.dev/Document".into()],
                    referrer_digests: vec!["sha256:aaaa".into(), "sha256:bbbb".into()],
                },
            ),
            (
                "unsupported_tlog_entry_kind",
                UnsupportedTlogEntryKind {
                    kind: "dsse".into(),
                    version: "0.0.1".into(),
                },
            ),
            ("tlog_binding_mismatch", TlogBindingMismatch),
            (
                "certificate_validity_window",
                CertificateValidityWindow {
                    integrated_time: "2026-01-01T00:00:00Z".into(),
                    not_before: "2026-02-01T00:00:00Z".into(),
                    not_after: "2026-02-01T00:10:00Z".into(),
                },
            ),
            (
                "sbom_media_type_unsupported",
                SbomMediaTypeUnsupported {
                    media_type: "application/octet-stream".into(),
                },
            ),
            (
                "attestation_too_large",
                AttestationTooLarge {
                    limit: 1024,
                    actual: 2048,
                },
            ),
            (
                "attestation_payload_too_large",
                AttestationPayloadTooLarge {
                    limit: 1024,
                    actual: 4096,
                },
            ),
            ("too_many_attestations", TooManyAttestations { limit: 32 }),
            (
                "attestation_budget_exhausted",
                AttestationBudgetExhausted { limit: 65_536 },
            ),
            (
                "unsupported_key_backend",
                UnsupportedKeyBackend(KeyRefError::UnsupportedBackend { scheme: Scheme::AwsKms }),
            ),
            ("key_reference_invalid", KeyReferenceInvalid(KeyRefError::Empty)),
            ("internal", Internal(Box::new(std::io::Error::other("test")))),
        ];

        // What this pins, exactly: a row deleted from the table above without
        // the count being lowered. It does NOT force a row for a *new* variant
        // -- `pairs` is an array literal, so `len()` is a compile-time constant
        // and both sides move together if the author simply bumps the number.
        // `kind_detail`'s exhaustive match forces the new *arm*; nothing yet
        // forces the new *row*, which is why the table once sat at 19 rows
        // against 22 arms -- three slugs on the production path with no pin at
        // all. Closing that gap needs variant enumeration.
        assert_eq!(
            pairs.len(),
            46,
            "a row was removed from the table above; restore it rather than lowering this count"
        );

        for (expected, kind) in pairs {
            assert_eq!(kind.kind_detail(), *expected, "kind_detail() drift for {kind:?}",);
        }
    }
}
