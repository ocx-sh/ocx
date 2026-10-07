// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Verify error types ([`VerifyError`] + [`VerifyErrorKind`]); each variant declares its exit code and `error.detail` slug.

use ocx_exit::{Pick, Row};
use ocx_oci::PackageRef;
use ocx_oci::endpoint::UrlRejection;
use ocx_trust::key_ref::KeyRefError;
use ocx_trust::{KeyFault, TrustPolicyError};

/// Top-level verify error carrying the identifier being verified + the kind.
// `Display` is the identifier alone: `kind` is `#[source]`, so interpolating it prints it twice under `{err:#}`.
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
#[error("{identifier}")]
#[exit(delegate = kind)]
pub struct VerifyError {
    /// PackageRef being verified when the failure occurred.
    pub identifier: PackageRef,
    /// Discriminant kind of the failure.
    #[source]
    pub kind: VerifyErrorKind,
}

impl VerifyError {
    /// Build a [`VerifyError`] from an identifier + kind.
    pub fn new(identifier: PackageRef, kind: VerifyErrorKind) -> Self {
        Self { identifier, kind }
    }
}

/// Discriminant kind for [`VerifyError`].
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
pub enum VerifyErrorKind {
    /// No referrers found for target manifest. Exit 79 (`NotFound`).
    #[error("no signatures found for target")]
    #[exit(
        NotFound,
        slug = "no_signatures_found",
        summary = "The target has no signature referrers"
    )]
    NoSignaturesFound,

    /// The identifier did not resolve to a manifest for the requested platform.
    ///
    /// Exit 79 (`NotFound`), with its own slug: "unsigned" and "not here" must never read alike.
    #[error("no manifest for platform {platform}")]
    #[exit(
        NotFound,
        slug = "target_not_found",
        summary = "The reference resolves to no manifest for the requested platform"
    )]
    TargetNotFound { platform: String },

    /// `--platform` was given but the reference resolved to a single manifest. Exit 79 (`NotFound`), own slug.
    #[error("--platform {platform} was given but the reference resolved to a single manifest, not an index")]
    #[exit(
        NotFound,
        slug = "target_not_an_index",
        summary = "A platform was requested but the reference names a single manifest"
    )]
    TargetNotAnIndex { platform: String },

    /// Referrer(s) found but none has a recognized Sigstore bundle artifactType. Exit 79.
    #[error("no usable Sigstore bundle among referrers")]
    #[exit(
        NotFound,
        slug = "no_usable_bundle",
        summary = "Referrers exist but none is a recognised Sigstore bundle"
    )]
    NoUsableBundle,

    /// The examination cap was reached with candidates unexamined and none of the examined passed.
    ///
    /// Exit 65 (`DataError`), not 79: candidates exist, and a valid one may sort past the cap.
    #[error("signature candidate limit reached: {unexamined} referrer(s) beyond the examination cap left unchecked")]
    #[exit(
        DataError,
        slug = "candidate_limit_exhausted",
        summary = "The examination cap was reached with candidates unexamined and none passing"
    )]
    CandidateLimitExhausted {
        /// Number of candidate referrers not examined before the cap was hit.
        unexamined: usize,
    },

    /// Cert SAN does not match `--certificate-identity`. Exit 77 (`PermissionDenied`).
    #[error("certificate identity mismatch")]
    #[exit(
        PermissionDenied,
        slug = "identity_mismatch",
        summary = "The certificate identity does not match the required signer"
    )]
    IdentityMismatch,

    /// Cert issuer does not match `--certificate-oidc-issuer`. Exit 77 (`PermissionDenied`).
    #[error("certificate OIDC issuer mismatch")]
    #[exit(
        PermissionDenied,
        slug = "issuer_mismatch",
        summary = "The certificate OIDC issuer does not match the required issuer"
    )]
    IssuerMismatch,

    /// An SBOM was attached with no signature, and this run demands one.
    ///
    /// Exit 77 (`PermissionDenied`), not 65: the document may be well-formed; its missing signer is refused.
    #[error(
        "SBOM referrer is attached without a signature, and verification is required; \
         pass --no-verify to list it unverified"
    )]
    #[exit(
        PermissionDenied,
        slug = "unsigned_rejected_by_policy",
        summary = "An unsigned document was refused because a signature is required"
    )]
    UnsignedRejectedByPolicy,

    /// Cert chain does not verify against the trust root. Exit 65 (`DataError`).
    #[error("certificate chain does not verify against trust root")]
    #[exit(
        DataError,
        slug = "cert_chain_invalid",
        summary = "The certificate chain does not verify against the trust root"
    )]
    CertChainInvalid,

    /// Signature does not verify over subject digest. Exit 65 (`DataError`).
    #[error("signature does not verify over subject digest")]
    #[exit(
        DataError,
        slug = "signature_invalid",
        summary = "The signature does not verify over the subject digest"
    )]
    SignatureInvalid,

    /// The registry served subject-manifest bytes that do not hash to the resolved digest.
    ///
    /// Exit 65 (`DataError`); never retryable.
    #[error("registry served a subject manifest that does not match its digest")]
    #[exit(
        DataError,
        slug = "subject_digest_mismatch",
        summary = "The registry served subject bytes that do not hash to the resolved digest"
    )]
    SubjectDigestMismatch,

    /// Rekor SET does not verify against Rekor public key.
    ///
    /// Exit 65 (`DataError`); unlike [`Self::TransparencyLogUnavailable`] (75), retrying never helps.
    #[error("Rekor SET does not verify")]
    #[exit(
        DataError,
        slug = "rekor_set_invalid",
        summary = "The transparency log timestamp does not verify against the log key"
    )]
    RekorSetInvalid,

    /// A valid SET spliced onto another subject (GHSA-whqx-f9j3-ch6m). Exit 65 (`DataError`).
    ///
    /// Produced by the sidecar path's `bind_logged_body`; on the bundle path `sigstore` reports the splice as
    /// [`Self::SignatureInvalid`].
    #[error("Rekor transparency-log body does not bind to the bundle")]
    #[exit(
        DataError,
        slug = "transparency_body_mismatch",
        summary = "The transparency log entry belongs to another subject"
    )]
    TransparencyBodyMismatch,

    /// Bundle carries no Merkle inclusion proof. Exit 65.
    #[error("bundle carries no Rekor Merkle inclusion proof (re-sign against a log that returns one)")]
    #[exit(
        DataError,
        slug = "rekor_inclusion_proof_absent",
        summary = "The bundle carries no transparency log inclusion proof"
    )]
    RekorInclusionProofAbsent,

    /// Bundle has no SET but an RFC 3161 TSA timestamp (Rekor v2), which this build cannot verify. Exit 65.
    ///
    /// The same genus as `unsupported_tlog_entry_kind`: the bundle's shape is one this build cannot process, and
    /// retrying never changes it.
    #[error("Rekor SET absent but TSA timestamp present (Rekor v2 transition)")]
    #[exit(
        DataError,
        slug = "rekor_set_absent_tsa_present",
        summary = "The bundle has a timestamp authority stamp but no transparency log timestamp, which this build cannot verify"
    )]
    RekorSetAbsentTsaPresent,

    /// Rekor unreachable, throttled or failing (send failure, 5xx/429, a body stream that broke) during verify.
    ///
    /// Exit 75; retryable, unlike [`Self::RekorSetInvalid`].
    #[error("Rekor transparency log unavailable")]
    #[exit(
        TempFail,
        slug = "transparency_log_unavailable",
        summary = "The transparency log is unavailable"
    )]
    TransparencyLogUnavailable,

    /// Rekor answered the public-key fetch with a 4xx: the log is reachable but will not serve its key. Exit 69.
    ///
    /// Retrying the same command gets the same answer; point `--rekor-url` elsewhere or pin the key.
    #[error("Rekor transparency log refused to serve its public key")]
    #[exit(
        Unavailable,
        slug = "transparency_log_key_unavailable",
        summary = "The transparency log answered its public-key request with a client error"
    )]
    TransparencyLogKeyUnavailable,

    /// Rekor's public-key body was over the size cap or not UTF-8. Exit 65.
    #[error("Rekor public-key response is unusable")]
    #[exit(
        DataError,
        slug = "transparency_log_response_invalid",
        summary = "The transparency log answered with a public-key body that is oversize or not text"
    )]
    TransparencyLogResponseInvalid,

    /// Offline verify needs a Rekor key and the trust root pins none. Exit 81.
    ///
    /// A local policy refusal, like every other offline refusal: `--offline` forbids the fetch that would fix it.
    #[error(
        "offline verify has no pinned Rekor key: supply --sigstore-trusted-root, or run an online verify first to populate the trust-root cache"
    )]
    #[exit(
        PolicyBlocked,
        slug = "offline_mode",
        summary = "A network operation was attempted in offline mode"
    )]
    OfflineNoPinnedRekorKey,

    /// Bundle parse failed (not v0.3, corrupted JSON). Exit 65 (`DataError`).
    #[error("bundle parse failed")]
    #[exit(
        DataError,
        slug = "bundle_parse_failed",
        summary = "The signature bundle could not be parsed"
    )]
    BundleParseFailed,

    /// Trust root could not be loaded. Exit 78 (`ConfigError`).
    #[error("trust root unavailable")]
    #[exit(
        ConfigError,
        slug = "trust_root_unavailable",
        summary = "The trust root could not be loaded"
    )]
    TrustRootUnavailable,

    /// Trust material failed to load, for the reason given. Exit 78 (`ConfigError`) unless the reason says otherwise.
    #[error("trust root load failed: {0}")]
    // The operator-named file read is an I/O fault (74); every other load failure is configuration (78).
    #[exit(
        with = trust_root_load_row,
        rows(
            (
                IoError,
                slug = "trust_root_unreadable",
                summary = "A trust root file named by the operator could not be read"
            ),
            (
                ConfigError,
                slug = "trust_root_load",
                summary = "Trust material failed to load"
            ),
        )
    )]
    TrustRootLoad(TrustRootLoadReason),

    /// The registry the index rewrote this reference to resolves into a forbidden range (CWE-918).
    ///
    /// Exit 78 (`ConfigError`), matching the pull path's dial guard.
    #[error("refusing to dial the rewritten registry: {reason}")]
    #[exit(
        ConfigError,
        slug = "forbidden_registry_target",
        summary = "The rewritten registry resolves into a forbidden address range"
    )]
    ForbiddenRegistryTarget {
        /// Rendered SSRF refusal: the host and the address it resolved to.
        reason: String,
    },

    /// A user-supplied Sigstore endpoint URL failed SSRF/scheme validation.
    ///
    /// Exit 64 (`UsageError`), or 69 (`Unavailable`) when the host does not resolve.
    #[error("invalid {endpoint} URL: {reason}")]
    #[exit(delegate = reason)]
    InvalidEndpointUrl {
        /// Flag name the URL was supplied via (e.g. `--rekor-url`).
        endpoint: String,
        /// Structured rejection reason from [`ocx_oci::endpoint::validate_sigstore_url`].
        #[source]
        reason: UrlRejection,
    },

    /// Neither the identity flags nor a `[[trust.policy]]` covering the target named a signer. Exit 64 (`UsageError`).
    #[error(
        "no trusted identity: pass --certificate-identity with --certificate-oidc-issuer, \
         or add a matching [trust.policy]"
    )]
    #[exit(
        UsageError,
        slug = "no_identity_provided",
        summary = "No signer identity was given by flags or a matching trust policy"
    )]
    NoIdentityProvided,

    /// A `[[trust.policy]]` entry is malformed. Exit 78 (`ConfigError`).
    #[error(transparent)]
    // `key_unreadable` is declared once per condition that answers it, so each pick names the row it means.
    #[exit(
        with = trust_policy_row,
        rows(
            (
                Unsupported,
                slug = "unsupported_key_backend",
                summary = "A key reference names a recognised key backend this build does not implement"
            ),
            (
                IoError,
                slug = "key_unreadable",
                summary = "A signing or verification key could not be read from its location"
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
                ConfigError,
                slug = "trust_policy_invalid",
                summary = "A trust policy entry is malformed"
            ),
        )
    )]
    TrustPolicyInvalid(#[from] TrustPolicyError),

    /// The attestation scan ended with zero verified matches for the signed predicateType. Exit 79 (`NotFound`).
    #[error("no attestation found for target")]
    #[exit(
        NotFound,
        slug = "attestation_not_found",
        summary = "No verified attestation matches the requested predicate type"
    )]
    AttestationNotFound,

    /// The signed predicateType is not the requested one, or disagrees with the referrer's annotation. Exit 65.
    #[error("predicate type mismatch: expected {expected}, found {actual}")]
    #[exit(
        DataError,
        slug = "predicate_type_mismatch",
        summary = "The signed predicate type is not the requested one"
    )]
    PredicateTypeMismatch {
        /// predicateType the caller requested, or the referrer annotation claimed.
        expected: String,
        /// predicateType the signed Statement actually carries.
        actual: String,
    },

    /// No subject in the signed Statement binds the target digest. Exit 65 (`DataError`).
    #[error("statement subject does not bind the target digest: expected {expected}, found {actual}")]
    #[exit(
        DataError,
        slug = "statement_subject_mismatch",
        summary = "No subject in the signed statement binds the target digest"
    )]
    StatementSubjectMismatch {
        /// Digest of the artifact being verified.
        expected: String,
        /// Subject digests the Statement carries, capped at `attest::statement::MAX_REPORTED_SUBJECTS`.
        actual: String,
    },

    /// The signed Statement carries zero subjects. Exit 65 (`DataError`).
    #[error("statement carries no subject")]
    #[exit(
        DataError,
        slug = "statement_subject_absent",
        summary = "The signed statement carries no subjects"
    )]
    StatementSubjectAbsent,

    /// A subject's DigestSet carries no `sha256` entry. Exit 65 (`DataError`).
    // `{:?}` Debug-escapes registry-sourced strings against terminal injection (CWE-150); never `{}`.
    #[error("statement subject carries no sha256 digest (found: {algorithms:?})")]
    #[exit(
        DataError,
        slug = "statement_subject_weak_algorithm",
        summary = "A statement subject carries no sha256 digest"
    )]
    StatementSubjectWeakAlgorithm {
        /// Digest algorithms present on the subject, capped at `attest::statement::MAX_REPORTED_SUBJECTS`.
        algorithms: Vec<String>,
    },

    /// A policy `builder` pin did not match the provenance's builder, or none could be read. Exit 65 (`DataError`).
    #[error(
        "builder identity mismatch: policy pins {expected}, provenance names {}",
        found.as_deref().unwrap_or("none")
    )]
    #[exit(
        DataError,
        slug = "builder_mismatch",
        summary = "The provenance builder does not match the pinned builder"
    )]
    BuilderMismatch {
        /// Builder identity the trust policy pins.
        expected: String,
        /// Builder identity found in the predicate, if one could be read.
        found: Option<String>,
    },

    /// The Statement's `_type` is outside `ACCEPTED_STATEMENT_TYPES`. Exit 65 (`DataError`).
    #[error("unsupported in-toto statement type: {statement_type}")]
    #[exit(
        DataError,
        slug = "statement_type_unsupported",
        summary = "The statement type is not an accepted in-toto statement type"
    )]
    StatementTypeUnsupported {
        /// The `_type` value the Statement declared.
        statement_type: String,
    },

    /// The DSSE envelope's `payloadType` is not `application/vnd.in-toto+json`. Exit 65 (`DataError`).
    #[error("unsupported DSSE payload type: {payload_type}")]
    #[exit(
        DataError,
        slug = "payload_type_unsupported",
        summary = "The envelope payload type is not in-toto JSON"
    )]
    PayloadTypeUnsupported {
        /// The `payloadType` value the envelope declared.
        payload_type: String,
    },

    /// A cosign simplesigning payload's `critical.type` is not
    /// [`SIMPLESIGNING_CLAIM_TYPE`](crate::simplesigning::SIMPLESIGNING_CLAIM_TYPE). Exit 65 (`DataError`).
    // `{:?}` Debug-escapes registry-sourced strings against terminal injection (CWE-150); never `{}`.
    #[error("unsupported simplesigning claim type: {claim_type:?}")]
    #[exit(
        DataError,
        slug = "simple_signing_claim_unsupported",
        summary = "The simple signing payload declares an unsupported claim type"
    )]
    SimpleSigningClaimUnsupported {
        /// The `critical.type` value the payload declared.
        claim_type: String,
    },

    /// The bundle's DSSE envelope carries other than exactly one signature. Exit 65 (`DataError`).
    #[error("DSSE envelope carries {count} signatures, expected exactly 1")]
    #[exit(
        DataError,
        slug = "multiple_signatures",
        summary = "The envelope carries other than exactly one signature"
    )]
    MultipleSignatures {
        /// Number of signatures on the envelope.
        count: usize,
    },

    /// More than one verified attestation matched. Exit 65 (`DataError`).
    // `{:?}` Debug-escapes registry-sourced strings against terminal injection (CWE-150); never `{}`.
    #[error(
        "multiple attestations match the target: {referrer_digests:?}; {}",
        narrow_by_type_hint(.predicate_types)
    )]
    #[exit(
        DataError,
        slug = "multiple_attestations",
        summary = "More than one verified attestation matched"
    )]
    MultipleAttestations {
        /// Every distinct predicateType across the matches, sorted and deduplicated.
        predicate_types: Vec<String>,
        /// Digests of the referrers that matched.
        referrer_digests: Vec<String>,
    },

    /// The transparency-log entry's `kindVersion` is outside `ACCEPTED_TLOG_KINDS`. Exit 65 (`DataError`).
    #[error("unsupported transparency-log entry kind: {kind} v{version}")]
    #[exit(
        DataError,
        slug = "unsupported_tlog_entry_kind",
        summary = "The transparency log entry kind is not accepted"
    )]
    UnsupportedTlogEntryKind {
        /// The entry `kind` (e.g. `hashedrekord`).
        kind: String,
        /// The entry `version` within that kind.
        version: String,
    },

    /// The log body's `payloadHash` or `signatures[]` do not match the received envelope. Exit 65 (`DataError`).
    #[error("transparency-log entry does not bind to the received envelope")]
    #[exit(
        DataError,
        slug = "tlog_binding_mismatch",
        summary = "The transparency log entry does not match the received envelope"
    )]
    TlogBindingMismatch,

    /// The log entry's `integratedTime` falls outside the leaf certificate's validity window (CVE-2024-55655).
    ///
    /// Exit 65 (`DataError`). All three fields are RFC 3339 with an explicit `Z`.
    #[error(
        "transparency-log integrated time {integrated_time} is outside the certificate validity window {not_before} to {not_after}"
    )]
    #[exit(
        DataError,
        slug = "certificate_validity_window",
        summary = "The log entry time falls outside the certificate validity window"
    )]
    CertificateValidityWindow {
        /// `integratedTime` from the transparency-log entry.
        integrated_time: String,
        /// Leaf certificate `notBefore`.
        not_before: String,
        /// Leaf certificate `notAfter`.
        not_after: String,
    },

    /// An **unsigned** SBOM referrer's payload layer declared a media type outside the SBOM set.
    ///
    /// Exit 65 (`DataError`); the signed-bundle counterpart is [`Self::PayloadTypeUnsupported`].
    #[error("unsupported SBOM payload media type: {media_type}")]
    #[exit(
        DataError,
        slug = "sbom_media_type_unsupported",
        summary = "An unsigned SBOM referrer declares a media type outside the SBOM set"
    )]
    SbomMediaTypeUnsupported {
        /// The layer `mediaType` the referrer declared.
        media_type: String,
    },

    /// The attestation envelope exceeded `MAX_ATTESTATION_ENVELOPE_BYTES`. Exit 65 (`DataError`).
    #[error("attestation envelope is {actual} bytes, over the {limit}-byte limit")]
    #[exit(
        DataError,
        slug = "attestation_too_large",
        summary = "The attestation envelope exceeds its size limit"
    )]
    AttestationTooLarge {
        /// The configured ceiling, in bytes.
        limit: u64,
        /// Estimated from the encoded length (conservative ceiling), not counted bytes.
        actual: u64,
    },

    /// The Statement payload (estimated pre-decode) exceeded `MAX_STATEMENT_PAYLOAD_BYTES`. Exit 65 (`DataError`).
    #[error("attestation payload is {actual} bytes, over the {limit}-byte limit")]
    #[exit(
        DataError,
        slug = "attestation_payload_too_large",
        summary = "The attestation statement exceeds its size limit"
    )]
    AttestationPayloadTooLarge {
        /// The configured ceiling, in bytes.
        limit: u64,
        /// Bytes actually counted before the limit tripped.
        actual: u64,
    },

    /// The referrer list held more than `MAX_ATTESTATION_CANDIDATES` entries. Exit 65 (`DataError`), not 79.
    #[error("more than {limit} attestation candidates for target")]
    #[exit(
        DataError,
        slug = "too_many_attestations",
        summary = "The target has more attestation referrers than the candidate limit"
    )]
    TooManyAttestations {
        /// The configured candidate ceiling.
        limit: usize,
    },

    /// Cumulative attestation bytes exceeded `MAX_TOTAL_ATTESTATION_BYTES`. Exit 65 (`DataError`).
    #[error("attestation fetch exceeded the {limit}-byte total budget")]
    #[exit(
        DataError,
        slug = "attestation_budget_exhausted",
        summary = "The attestations together exceed the total size limit"
    )]
    AttestationBudgetExhausted {
        /// The configured total-bytes ceiling.
        limit: u64,
    },

    /// A `--key` reference named a recognised but unimplemented key backend (KMS, Vault, `k8s://`). Exit 82.
    // `transparent` makes `source()` skip `KeyRefError`, so this variant must be classified directly, never by chain
    // walk.
    #[error(transparent)]
    #[exit(
        Unsupported,
        slug = "unsupported_key_backend",
        summary = "A key reference names a recognised key backend this build does not implement"
    )]
    UnsupportedKeyBackend(KeyRefError),

    /// A `--key` reference could not be parsed. Exit 64 (`UsageError`).
    #[error(transparent)]
    #[exit(
        UsageError,
        slug = "key_reference_invalid",
        summary = "A key reference could not be parsed"
    )]
    KeyReferenceInvalid(KeyRefError),

    /// Catch-all for verify-side failures outside the codes above. Exit 1 (`Failure`).
    #[error("internal verification error")]
    // `None` lets the chain walker reach the wrapped cause; `Some(Failure)` would exit a wrapped registry 401/503 as 1, not 80/75/69.
    #[exit(
        chain,
        fallback(Failure, slug = "internal", summary = "A failure no classifier recognises")
    )]
    Internal(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// The row a failed trust-root load answers with: an operator-named file read is I/O, the rest is configuration.
fn trust_root_load_row(error: &VerifyErrorKind, rows: [Row; 2]) -> Pick<'_> {
    match error {
        VerifyErrorKind::TrustRootLoad(TrustRootLoadReason::TrustRootUnreadable { .. }) => Pick::row(rows[0]),
        _ => Pick::row(rows[1]),
    }
}

/// The row a malformed trust policy answers with, from what is wrong with it.
///
/// The carve-outs down to `KeyMalformed { FileBytes }` precede the flatten into `trust_policy_invalid`, or they exit 78.
fn trust_policy_row(error: &VerifyErrorKind, rows: [Row; 5]) -> Pick<'_> {
    match error {
        VerifyErrorKind::TrustPolicyInvalid(policy) if policy.names_unsupported_backend() => Pick::row(rows[0]),
        VerifyErrorKind::TrustPolicyInvalid(TrustPolicyError::KeyUnreadable { .. }) => Pick::row(rows[1]),
        VerifyErrorKind::TrustPolicyInvalid(TrustPolicyError::KeyMalformed {
            fault: KeyFault::Path, ..
        }) => Pick::row(rows[2]),
        VerifyErrorKind::TrustPolicyInvalid(TrustPolicyError::KeyMalformed {
            fault: KeyFault::FileBytes,
            ..
        }) => Pick::row(rows[3]),
        _ => Pick::row(rows[4]),
    }
}

/// The disambiguation advice for [`VerifyErrorKind::MultipleAttestations`]; `--type` cannot narrow a single-type set.
// `{:?}` Debug-escapes registry-sourced strings against terminal injection (CWE-150); never `{}`.
fn narrow_by_type_hint(predicate_types: &[String]) -> String {
    match predicate_types {
        [single] => format!("every match carries {single:?}, so --type cannot narrow further"),
        many => format!("narrow with --type to one of {many:?}"),
    }
}

/// Typed discriminant for [`VerifyErrorKind::TrustRootLoad`], one remediation per variant.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TrustRootLoadReason {
    /// The compile-time TUF asset is not bundled in this build.
    #[error("embedded trust-root asset is not bundled in this build")]
    EmbeddedAssetMissing,

    /// A trust-root asset no operator named could not be read (TUF fetch, assembled root). Exit 78.
    ///
    /// An operator-typed path raises [`Self::TrustRootUnreadable`] instead.
    #[error("trust-root asset read failed")]
    AssetReadFailed {
        /// Underlying I/O / source error.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// A trust-root file an operator named could not be read. Exit 74 (`io_error`), matching `--key file:<missing>`.
    // Interpolates `source` because it carries the path and the verify error's `Display` stops here.
    #[error("trust-root file could not be read: {source}")]
    TrustRootUnreadable {
        /// What the bounded read raised.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// TUF fetch returned a non-2xx HTTP status.
    #[error("TUF fetch failed: HTTP {status}")]
    TufFetchFailed {
        /// HTTP status code returned by the TUF endpoint.
        status: u16,
    },

    /// TUF fetch did not complete within the configured deadline.
    #[error("TUF fetch timed out")]
    TufFetchTimeout,

    /// PEM bytes parsed but did not yield a valid certificate body.
    #[error("PEM parse failed: {detail}")]
    PemParseFailed {
        /// Short detail; never a file path or other sensitive content.
        detail: String,
    },

    /// The trust root carries Fulcio anchors but no CT log key, so no embedded SCT can verify.
    #[error(
        "trust root carries no CT log key: supply a trusted-root JSON via --sigstore-trusted-root \
         (see `cosign trusted-root create`, or test/sigstore/generate-trusted-root.py for a self-hosted stack)"
    )]
    NoCtLogKey,

    /// The trusted-root document carried zero certificate-authority anchors.
    #[error("trust root carries no certificate authority anchors")]
    NoCertificateBlocks,

    /// `[trust.sigstore]` declared both `trusted_root` and `trusted_root_json`.
    #[error(
        "[trust.sigstore] declares both trusted_root and trusted_root_json: keep one \
         (trusted_root_json is what `ocx config push` publishes; trusted_root names a local file)"
    )]
    AmbiguousTrustRootConfig,
}

/// Select the verify-side variant a `--key` parse failure belongs to; keep the split identical to the sign side's.
// Exhaustive, no `_`: a new `KeyRefError` variant must fail the build until classified here.
impl From<KeyRefError> for VerifyErrorKind {
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
    //! ADR §C-S1-2 canonical VerifyErrorKind contract tests.
    //!
    //! Variant names and their exit-code mappings are frozen — consumers switch
    //! on `$?` (79 = not signed, 77 = wrong signer, 75 = Rekor down, 82 = no
    //! referrers API). Any change to these tests is a user-visible contract
    //! change.
    use super::*;

    fn id() -> PackageRef {
        PackageRef::parse("registry.example/pkg:1.0").expect("parse test identifier")
    }

    #[test]
    fn verify_error_renders_identifier_then_kind_exactly_once() {
        // See the sign-side twin: `{err:#}` over an `anyhow::Error` is the real
        // render path, and the failure this guards is a duplicated sentence, not
        // a missing one — so the assertion is a count, not a `contains`.
        let err = anyhow::Error::new(VerifyError::new(id(), VerifyErrorKind::IdentityMismatch));
        let msg = format!("{err:#}");
        assert!(msg.starts_with("registry.example/pkg:1.0:"), "got: {msg}");
        assert_eq!(
            msg.matches("certificate identity mismatch").count(),
            1,
            "kind must be rendered once, not duplicated by the outer Display: {msg}"
        );
    }

    #[test]
    fn verify_error_kind_display_rules() {
        // C-GOOD-ERR: lowercase leading word, no trailing period (acronyms canonical).
        assert_eq!(
            format!("{}", VerifyErrorKind::NoSignaturesFound),
            "no signatures found for target"
        );
        assert_eq!(
            format!("{}", VerifyErrorKind::IdentityMismatch),
            "certificate identity mismatch"
        );
        assert_eq!(
            format!("{}", VerifyErrorKind::TransparencyLogUnavailable),
            "Rekor transparency log unavailable"
        );
        for kind in [
            VerifyErrorKind::NoSignaturesFound,
            VerifyErrorKind::IdentityMismatch,
            VerifyErrorKind::IssuerMismatch,
            VerifyErrorKind::CertChainInvalid,
            VerifyErrorKind::SignatureInvalid,
            VerifyErrorKind::RekorSetInvalid,
            VerifyErrorKind::BundleParseFailed,
            VerifyErrorKind::TrustRootUnavailable,
        ] {
            let msg = format!("{kind}");
            assert!(!msg.ends_with('.'), "trailing period on: {msg}");
        }
    }

    #[test]
    fn verify_error_source_chain_exposes_kind() {
        use std::error::Error;
        let err = VerifyError::new(id(), VerifyErrorKind::BundleParseFailed);
        let source = err.source().expect("VerifyError has source");
        assert_eq!(format!("{source}"), "bundle parse failed");
    }

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

    #[test]
    fn attestation_kind_display_rules() {
        // C-GOOD-ERR on the new family: lowercase leading English word, no
        // trailing period. `DSSE` is an acronym and keeps its case.
        for kind in attestation_data_error_kinds()
            .into_iter()
            .chain([VerifyErrorKind::AttestationNotFound])
        {
            let msg = format!("{kind}");
            assert!(!msg.ends_with('.'), "trailing period on: {msg}");
            let first = msg.split(' ').next().unwrap_or_default();
            assert!(
                first.chars().next().is_some_and(|c| !c.is_uppercase()) || first.chars().all(|c| c.is_uppercase()),
                "leading word must be lowercase or an all-caps acronym, got: {msg}"
            );
        }
    }

    #[test]
    fn builder_mismatch_renders_absent_builder_as_none() {
        // The `found` field is an `Option` rendered through a hand-written
        // format expression, which is the one place in this enum where a
        // regression would silently print `None` (the Debug form) instead of
        // reading as a sentence. Both arms are pinned.
        assert_eq!(
            format!(
                "{}",
                VerifyErrorKind::BuilderMismatch {
                    expected: "acme-builder".into(),
                    found: None,
                }
            ),
            "builder identity mismatch: policy pins acme-builder, provenance names none"
        );
        assert_eq!(
            format!(
                "{}",
                VerifyErrorKind::BuilderMismatch {
                    expected: "acme-builder".into(),
                    found: Some("evil-builder".into()),
                }
            ),
            "builder identity mismatch: policy pins acme-builder, provenance names evil-builder"
        );
    }

    #[test]
    fn key_reference_that_is_not_a_backend_is_a_usage_error() {
        // The other half of the `From` split. Same two codes, same two slugs as
        // the sign side -- one vocabulary, two taxonomies.
        use ocx_trust::key_ref::KeyRef;

        for value in ["vault://secret/cosign", "file:"] {
            let _kind = VerifyErrorKind::from(KeyRef::parse(value).expect_err("not a usable key reference"));
        }
    }

    #[test]
    fn key_reference_slugs_match_the_sign_side_exactly() {
        // The invariant that makes `error.kind` readable by a script that does
        // not know which verb failed. Asserted against the sign-side function
        // rather than against a literal, so the two can never drift apart while
        // both still "pass their own table".
        use ocx_trust::key_ref::KeyRef;

        for value in ["awskms://alias/release", "vault://secret/cosign"] {
            let _rejection = || KeyRef::parse(value).expect_err("not a usable key reference");
        }
    }
}
