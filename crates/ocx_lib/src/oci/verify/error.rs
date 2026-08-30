// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Verify error types (three-layer: [`VerifyError`] + [`VerifyErrorKind`]).
//!
//! Variant inventory is the ADR-canonical set per C-S1-2. Each variant maps
//! to a distinct exit code via [`ClassifyErrorKind`].

use crate::cli::{ClassifyErrorKind, ClassifyExitCode, ExitCode};
use crate::oci::Identifier;
use crate::oci::endpoint::UrlRejection;
use crate::oci::sign::KeyRefError;

/// Top-level verify error carrying the identifier being verified + the kind.
///
/// The `Display` is the identifier alone: `kind` is `#[source]`, and every
/// render site uses the chain-walking `{err:#}` form, which appends the source
/// itself. Interpolating `{kind}` here as well printed the whole sentence twice.
#[derive(Debug, thiserror::Error)]
#[error("{identifier}")]
pub struct VerifyError {
    /// Identifier being verified when the failure occurred.
    pub identifier: Identifier,
    /// Discriminant kind of the failure.
    #[source]
    pub kind: VerifyErrorKind,
}

impl VerifyError {
    /// Build a [`VerifyError`] from an identifier + kind.
    pub fn new(identifier: Identifier, kind: VerifyErrorKind) -> Self {
        Self { identifier, kind }
    }
}

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

/// Discriminant kind for [`VerifyError`].
///
/// Canonical ADR names per C-S1-2: `IdentityMismatch`, `IssuerMismatch`,
/// `BundleParseFailed`, `RekorSetInvalid`, etc.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum VerifyErrorKind {
    /// No referrers found for target manifest.
    ///
    /// Exit 79 (`NotFound`). Publisher has not signed, or signed a different platform.
    #[error("no signatures found for target")]
    NoSignaturesFound,

    /// The identifier did not resolve to a manifest for the requested platform.
    ///
    /// Exit 79 (`NotFound`), the same code as [`Self::NoSignaturesFound`] — but
    /// never the same slug. "This package is unsigned" and "this package is not
    /// here" are opposite conclusions for someone deciding whether to trust an
    /// artifact, and reporting the first for the second is how a typo becomes a
    /// belief about supply-chain posture.
    #[error("no manifest for platform {platform}")]
    TargetNotFound { platform: String },

    /// `--platform` was given but the reference resolved to a single manifest.
    ///
    /// Exit 79 (`NotFound`) and a slug of its own, for the reason
    /// [`Self::TargetNotFound`] states: "this package ships no such platform"
    /// and "this reference has no platforms to choose from" have different
    /// remedies — drop the flag, rather than go looking for a build that was
    /// never missing.
    #[error("--platform {platform} was given but the reference resolved to a single manifest, not an index")]
    TargetNotAnIndex { platform: String },

    /// Referrer(s) found but none has a recognized Sigstore bundle artifactType.
    ///
    /// Exit 79. May be a legacy tag-based signature (Slice 2) or a non-Sigstore attestation.
    #[error("no usable Sigstore bundle among referrers")]
    NoUsableBundle,

    /// The examination cap was reached with candidates still unexamined and
    /// none of the examined candidates passed.
    ///
    /// Exit 65 (`DataError`). Fail-closed: the candidate order is by digest
    /// (no trust significance), so a valid signature may sort past the cap. This
    /// is reported distinctly instead of an examined candidate's error — which
    /// would misleadingly attribute the failure to a specific (unrelated)
    /// referrer. Not 79: candidates exist, so "no signatures found" would be
    /// wrong; the operator must reduce the referrer count or raise the cap.
    #[error("signature candidate limit reached: {unexamined} referrer(s) beyond the examination cap left unchecked")]
    CandidateLimitExhausted {
        /// Number of candidate referrers not examined before the cap was hit.
        unexamined: usize,
    },

    /// Cert SAN does not match `--certificate-identity`.
    ///
    /// Exit 77 (`PermissionDenied`).
    #[error("certificate identity mismatch")]
    IdentityMismatch,

    /// Cert issuer does not match `--certificate-oidc-issuer`.
    ///
    /// Exit 77 (`PermissionDenied`).
    #[error("certificate OIDC issuer mismatch")]
    IssuerMismatch,

    /// An SBOM was attached with no signature, and this run demands one.
    ///
    /// Exit 77 (`PermissionDenied`), the same code as an identity mismatch and
    /// for the same reason: the policy names who must have signed, and a raw
    /// attachment has no signer at all. `DataError` would be wrong — the
    /// document may be perfectly well-formed; it is the trust class that the
    /// policy refuses.
    #[error(
        "SBOM referrer is attached without a signature, and verification is required; \
         pass --no-verify to list it unverified"
    )]
    UnsignedRejectedByPolicy,

    /// Cert chain does not verify against TUF root.
    ///
    /// Exit 65 (`DataError`). TUF root out of date, or cert is forged.
    #[error("certificate chain does not verify against trust root")]
    CertChainInvalid,

    /// Signature does not verify over subject digest.
    ///
    /// Exit 65 (`DataError`). Strongest possible failure — bundle contents tampered.
    #[error("signature does not verify over subject digest")]
    SignatureInvalid,

    /// The registry served subject-manifest bytes that do not hash to the
    /// digest the index resolved.
    ///
    /// Exit 65 (`DataError`). Verification needs the signed artifact's bytes,
    /// not just its digest, so the pipeline fetches the subject manifest and
    /// re-hashes it. A mismatch means the registry served something other than
    /// the resolved manifest -- corrupt or hostile. Never retryable.
    #[error("registry served a subject manifest that does not match its digest")]
    SubjectDigestMismatch,

    /// Rekor SET does not verify against Rekor public key.
    ///
    /// Exit 65 (`DataError`). A cryptographically invalid SET is a data integrity
    /// failure — the bundle has been tampered with. This is distinct from
    /// [`Self::TransparencyLogUnavailable`] (service down, retry may help): no amount of
    /// retrying will fix a tampered SET, and callers must not treat this as a
    /// transient failure.
    #[error("Rekor SET does not verify")]
    RekorSetInvalid,

    /// The Rekor transparency-log entry body does not bind to this bundle.
    ///
    /// Exit 65 (`DataError`). The SET verifies over the log entry body, but that
    /// body's hashed subject digest, signature, or certificate does not match the
    /// bundle's — a previously-valid SET/body spliced onto a different subject
    /// (GHSA-whqx-f9j3-ch6m class). Like [`Self::SignatureInvalid`] this is a
    /// tampered-bundle failure, not a service fault: retrying never heals it.
    ///
    /// **Currently unproduced.** The check is `sigstore`'s since the verifier
    /// took over bundle content verification, and it models the failure in an
    /// enum whose payload type it does not export — so the splice arrives here
    /// as [`Self::SignatureInvalid`]. Same exit code (65), different slug. The
    /// variant stays because the slug is a frozen contract (C-S1-1) and
    /// removing it would break a consumer matching on it.
    #[error("Rekor transparency-log body does not bind to the bundle")]
    TransparencyBodyMismatch,

    /// Bundle carries no Merkle inclusion proof.
    ///
    /// Exit 65. The Signed Entry Timestamp is only a *promise* to include; the
    /// inclusion proof is the evidence that the entry is in a tree whose root
    /// the log signed. `sigstore`'s own online branch refuses a bundle without
    /// one, and ocx runs its verifier with `offline: true`, so this check is
    /// what keeps the two branches at parity instead of silently weaker.
    #[error("bundle carries no Rekor Merkle inclusion proof (re-sign against a log that returns one)")]
    RekorInclusionProofAbsent,

    /// Rekor v2 transition: bundle has no SET but has an RFC 3161 TSA timestamp.
    ///
    /// Exit 83. v1 cannot verify TSA; full Rekor v2 support deferred until
    /// sigstore-rs ships a v2 client.
    #[error("Rekor SET absent but TSA timestamp present (Rekor v2 transition)")]
    RekorSetAbsentTsaPresent,

    /// Rekor unavailable during verify.
    ///
    /// Exit 83. Distinct from [`Self::RekorSetInvalid`] — retry is appropriate.
    #[error("Rekor transparency log unavailable")]
    TransparencyLogUnavailable,

    /// Bundle parse failed (not v0.3, corrupted JSON).
    ///
    /// Exit 65 (`DataError`).
    #[error("bundle parse failed")]
    BundleParseFailed,

    /// Trust root could not be loaded (embedded asset missing, TUF fetch failed).
    ///
    /// Exit 78 (`ConfigError`).
    #[error("trust root unavailable")]
    TrustRootUnavailable,

    /// Trust root PEM failed to load (malformed PEM, no certificate blocks,
    /// TUF fetch failed, etc.).
    ///
    /// Exit 78 (`ConfigError`). The reason is encoded as a typed discriminant
    /// (`TrustRootLoadReason`) so callers can distinguish actionable failure
    /// modes without parsing stderr.
    #[error("trust root load failed: {0}")]
    TrustRootLoad(TrustRootLoadReason),

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

    /// User-supplied Sigstore endpoint URL failed SSRF/scheme validation.
    ///
    /// Surfaces at the boundary where `--rekor-url` is parsed by `ocx package
    /// verify`. Exit 64 (`UsageError`) — a malformed flag value is a CLI
    /// misuse, not a runtime fault. The `endpoint` field carries the flag
    /// name (e.g. `--rekor-url`) so the envelope `error.detail` is
    /// programmatically dispatchable.
    #[error("invalid {endpoint} URL: {reason}")]
    InvalidEndpointUrl {
        /// Flag name the URL was supplied via (e.g. `--rekor-url`).
        endpoint: String,
        /// Structured rejection reason from [`crate::oci::endpoint::validate_sigstore_url`].
        #[source]
        reason: UrlRejection,
    },

    /// No signing identity to verify against: neither the
    /// `--certificate-identity` + `--certificate-oidc-issuer` flags nor a
    /// `[[trust.policy]]` whose scope covers the target supplied one.
    ///
    /// Exit 64 (`UsageError`) — mirrors the prior "omitted required flag"
    /// behavior. Verification is meaningless without knowing whose signature to
    /// trust.
    #[error(
        "no trusted identity: pass --certificate-identity with --certificate-oidc-issuer, \
         or add a matching [trust.policy]"
    )]
    NoIdentityProvided,

    /// A `[[trust.policy]]` entry is malformed: an empty or absent `signers`
    /// array, an incomplete keyless signer (identity or issuer unset, both or
    /// neither identity form set), an `identity_regexp` that does not compile,
    /// or a key signer whose material is unreadable, unparseable, or names a
    /// backend this build cannot resolve.
    ///
    /// Exit 78 (`ConfigError`).
    #[error(transparent)]
    TrustPolicyInvalid(#[from] crate::trust::TrustPolicyError),

    /// The attestation scan ended with zero verified matches.
    ///
    /// Exit 79 (`NotFound`). Either no referrer is an attestation, or none
    /// whose *signed* predicateType matches the requested `--type`. Narrowing
    /// happens on the verified payload after fetch-and-parse: a referrer
    /// annotation never excludes a candidate, because an annotation is
    /// unsigned and a hostile publisher controls it.
    #[error("no attestation found for target")]
    AttestationNotFound,

    /// The signed predicateType is not the one requested, or disagrees with
    /// the referrer's annotation.
    ///
    /// Exit 65 (`DataError`).
    #[error("predicate type mismatch: expected {expected}, found {actual}")]
    PredicateTypeMismatch {
        /// predicateType the caller requested, or the referrer annotation claimed.
        expected: String,
        /// predicateType the signed Statement actually carries.
        actual: String,
    },

    /// No subject in the signed Statement binds the target digest.
    ///
    /// Exit 65 (`DataError`). The attestation verifies cryptographically but
    /// attests to a *different* artifact — the splice this check exists for.
    #[error("statement subject does not bind the target digest: expected {expected}, found {actual}")]
    StatementSubjectMismatch {
        /// Digest of the artifact being verified.
        expected: String,
        /// Subject digests the Statement actually carries, capped at
        /// `attest::statement::MAX_REPORTED_SUBJECTS` with an `and N more`
        /// tail — the Statement's subject count is attacker-chosen.
        actual: String,
    },

    /// The signed Statement carries zero subjects.
    ///
    /// Exit 65 (`DataError`). A subject-less Statement binds nothing, so it
    /// can never be evidence about this artifact. Distinct from
    /// [`Self::StatementSubjectMismatch`]: there is nothing to compare.
    #[error("statement carries no subject")]
    StatementSubjectAbsent,

    /// A subject's DigestSet carries no `sha256` entry.
    ///
    /// Exit 65 (`DataError`). Matching on a weaker algorithm would let a
    /// collision stand in for the binding, so an unusable DigestSet is
    /// refused rather than matched on what it does carry.
    // The {:?} interpolation is deliberate: Debug-escaping the registry-sourced
    // strings IS the CWE-150 terminal-injection protection. Do not "clean up" to {}.
    #[error("statement subject carries no sha256 digest (found: {algorithms:?})")]
    StatementSubjectWeakAlgorithm {
        /// Digest algorithms present on the subject, capped at
        /// `attest::statement::MAX_REPORTED_SUBJECTS` — a hostile Statement
        /// carries hundreds of thousands of them, and this field crosses into
        /// `--json` unrendered.
        algorithms: Vec<String>,
    },

    /// A policy `builder` pin did not match the provenance predicate.
    ///
    /// Exit 65 (`DataError`). Covers all three shapes of failure — the
    /// builder identity is absent, unparseable, or simply different. A pin
    /// that cannot be evaluated is a refusal, never a skip: silently passing
    /// an unpinnable predicate is how a policy stops being a policy.
    #[error(
        "builder identity mismatch: policy pins {expected}, provenance names {}",
        found.as_deref().unwrap_or("none")
    )]
    BuilderMismatch {
        /// Builder identity the trust policy pins.
        expected: String,
        /// Builder identity found in the predicate, if one could be read.
        found: Option<String>,
    },

    /// The Statement's `_type` is outside `ACCEPTED_STATEMENT_TYPES`.
    ///
    /// Exit 65 (`DataError`).
    #[error("unsupported in-toto statement type: {statement_type}")]
    StatementTypeUnsupported {
        /// The `_type` value the Statement declared.
        statement_type: String,
    },

    /// The DSSE envelope's `payloadType` is not `application/vnd.in-toto+json`.
    ///
    /// Exit 65 (`DataError`). Checked before the payload is parsed: the
    /// declared type is what says how to read the bytes.
    #[error("unsupported DSSE payload type: {payload_type}")]
    PayloadTypeUnsupported {
        /// The `payloadType` value the envelope declared.
        payload_type: String,
    },

    /// A cosign simplesigning payload's `critical.type` is not
    /// [`SIMPLESIGNING_CLAIM_TYPE`](crate::oci::simplesigning::SIMPLESIGNING_CLAIM_TYPE).
    ///
    /// Exit 65 (`DataError`). `critical` is by definition the part a verifier
    /// must understand, so a payload declaring another claim type is not an
    /// image signature and must never be read as one — refused rather than
    /// skipped, or a registry could relabel a signature into "none found".
    // The {:?} interpolation is deliberate: Debug-escaping the registry-sourced
    // string IS the CWE-150 terminal-injection protection. Do not "clean up" to {}.
    #[error("unsupported simplesigning claim type: {claim_type:?}")]
    SimpleSigningClaimUnsupported {
        /// The `critical.type` value the payload declared.
        claim_type: String,
    },

    /// The bundle's DSSE envelope carries other than exactly one signature.
    ///
    /// Exit 65 (`DataError`). Verifying one signature out of several would
    /// report "verified" for an envelope whose other signatures nobody checked.
    #[error("DSSE envelope carries {count} signatures, expected exactly 1")]
    MultipleSignatures {
        /// Number of signatures on the envelope.
        count: usize,
    },

    /// More than one verified attestation matched.
    ///
    /// Exit 65 (`DataError`). `ocx package sbom --output` writes one document;
    /// picking one of several verified candidates would make which SBOM a
    /// consumer receives depend on referrer ordering.
    // The {:?} interpolation is deliberate: Debug-escaping the registry-sourced
    // strings IS the CWE-150 terminal-injection protection. Do not "clean up" to {}.
    #[error(
        "multiple attestations match the target: {referrer_digests:?}; {}",
        narrow_by_type_hint(.predicate_types)
    )]
    MultipleAttestations {
        /// Every distinct predicateType across the matches, sorted and
        /// deduplicated. All of them, not the first: a mixed-type match set
        /// named by one type tells the operator something untrue about the
        /// other candidates, and `--type` is unusable advice without the list
        /// of values it accepts here.
        predicate_types: Vec<String>,
        /// Digests of the referrers that matched.
        referrer_digests: Vec<String>,
    },

    /// The transparency-log entry's `kindVersion` is outside `ACCEPTED_TLOG_KINDS`.
    ///
    /// Exit 65 (`DataError`). Each kind has its own canonicalization, so an
    /// unrecognized one cannot be re-derived and compared at all.
    #[error("unsupported transparency-log entry kind: {kind} v{version}")]
    UnsupportedTlogEntryKind {
        /// The entry `kind` (e.g. `hashedrekord`).
        kind: String,
        /// The entry `version` within that kind.
        version: String,
    },

    /// The canonicalized log-entry body does not match the received envelope.
    ///
    /// Exit 65 (`DataError`). Deliberately *not* named `EnvelopeHashMismatch`:
    /// verify never recomputes an envelope hash, so a name promising that
    /// comparison would describe a check that does not exist. What is compared
    /// is the body's `payloadHash` and `signatures[]` against the envelope
    /// actually received.
    #[error("transparency-log entry does not bind to the received envelope")]
    TlogBindingMismatch,

    /// The log entry's `integratedTime` falls outside the leaf certificate's
    /// validity window.
    ///
    /// Exit 65 (`DataError`). CVE-2024-55655: without this check an expired
    /// certificate's signature stays acceptable forever, because the log entry
    /// alone does not prove the signature was made while the cert was valid.
    /// All three fields are RFC 3339 with an explicit `Z`.
    #[error(
        "transparency-log integrated time {integrated_time} is outside the certificate validity window {not_before} to {not_after}"
    )]
    CertificateValidityWindow {
        /// `integratedTime` from the transparency-log entry.
        integrated_time: String,
        /// Leaf certificate `notBefore`.
        not_before: String,
        /// Leaf certificate `notAfter`.
        not_after: String,
    },

    /// An **unsigned** SBOM referrer's payload layer declared a media type
    /// outside the SBOM set.
    ///
    /// Exit 65 (`DataError`). An unsigned referrer records what it carries in
    /// its `artifactType` and its layer's `mediaType`, and nothing signs either
    /// — so a layer typed outside the set is the one structural claim the read
    /// path can check, and an unreadable blob under an SBOM `artifactType` is
    /// refused rather than listed as an SBOM.
    ///
    /// Distinct from [`Self::PayloadTypeUnsupported`], which is the DSSE
    /// envelope's `payloadType` inside a *signed* bundle: same shape of
    /// complaint, different document, and a script that conflated them would
    /// draw the wrong conclusion about whether a signature was involved.
    #[error("unsupported SBOM payload media type: {media_type}")]
    SbomMediaTypeUnsupported {
        /// The layer `mediaType` the referrer declared.
        media_type: String,
    },

    /// The attestation envelope exceeded `MAX_ATTESTATION_ENVELOPE_BYTES`.
    ///
    /// Exit 65 (`DataError`).
    #[error("attestation envelope is {actual} bytes, over the {limit}-byte limit")]
    AttestationTooLarge {
        /// The configured ceiling, in bytes.
        limit: u64,
        /// Estimated from the encoded length (conservative ceiling), not counted bytes.
        actual: u64,
    },

    /// The Statement payload (estimated pre-decode from the base64 length) exceeded `MAX_STATEMENT_PAYLOAD_BYTES`.
    ///
    /// Exit 65 (`DataError`). Separate from [`Self::AttestationTooLarge`]
    /// because base64 in the envelope and the decoded payload are two
    /// different sizes, and the decode is where the expansion happens.
    #[error("attestation payload is {actual} bytes, over the {limit}-byte limit")]
    AttestationPayloadTooLarge {
        /// The configured ceiling, in bytes.
        limit: u64,
        /// Bytes actually counted before the limit tripped.
        actual: u64,
    },

    /// The referrer list held more than `MAX_ATTESTATION_CANDIDATES` entries.
    ///
    /// Exit 65 (`DataError`). Fail-closed, like
    /// [`Self::CandidateLimitExhausted`] on the signature path: candidates
    /// exist, so reporting "not found" would misreport a possibly-attested
    /// artifact.
    #[error("more than {limit} attestation candidates for target")]
    TooManyAttestations {
        /// The configured candidate ceiling.
        limit: usize,
    },

    /// Cumulative attestation bytes exceeded `MAX_TOTAL_ATTESTATION_BYTES`.
    ///
    /// Exit 65 (`DataError`). Per-envelope caps bound one candidate; this
    /// bounds the scan, which is what a thousand small envelopes attack.
    #[error("attestation fetch exceeded the {limit}-byte total budget")]
    AttestationBudgetExhausted {
        /// The configured total-bytes ceiling.
        limit: u64,
    },

    /// A `--key` reference named a key backend OCX recognises but has not
    /// implemented (`awskms://`, `gcpkms://`, `azurekms://`, `hashivault://`,
    /// `k8s://`).
    ///
    /// Exit 85 (`UnsupportedKeyBackend`). Verify parses `--key` on its own
    /// path, so it reaches 85 on its own rather than borrowing the sign-side
    /// error: one vocabulary, two taxonomies. The slug is byte-identical to
    /// `SignErrorKind`'s so a script reads one word for one failure.
    ///
    /// Remediation: pass a file key, or wait for the backend. Never reported as
    /// "no such file or directory" -- the refusal happens at the parse
    /// boundary, before anything treats the reference as a path.
    ///
    /// `transparent` rather than a wrapping message: the wrapped
    /// [`KeyRefError`] already names the scheme, and a prefix here would render
    /// the sentence twice under `{err:#}`. Transparent forwards `source()`
    /// *past* the value it wraps, which is harmless here -- `KeyRefError` is a
    /// leaf with no source of its own, and `exit_code()` answers for this
    /// variant directly instead of delegating to the chain walker.
    #[error(transparent)]
    UnsupportedKeyBackend(KeyRefError),

    /// A `--key` reference could not be parsed: an unrecognised scheme token,
    /// or nothing following the scheme.
    ///
    /// Exit 64 (`UsageError`). Remediation: fix the reference. Same
    /// `transparent` reasoning as [`Self::UnsupportedKeyBackend`]; the two are
    /// separate variants because their exit codes and their remedies differ,
    /// and `From<KeyRefError>` is the single place that decides which applies.
    #[error(transparent)]
    KeyReferenceInvalid(KeyRefError),

    /// Catch-all for verify-side failures outside the codes above (index
    /// resolution, digest parse, malformed URL join).
    ///
    /// Exit 1 (`Failure`). Carries the underlying error via `#[source]` so
    /// `classify_error` chain-walking and `{err:#}` diagnostics preserve the
    /// cause — never erase it with `.to_string()`.
    #[error("internal verification error")]
    Internal(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// The disambiguation advice carried by [`VerifyErrorKind::MultipleAttestations`].
///
/// `--type` narrows a scan by predicate type, so it is a remedy only when the
/// matches disagree about type. Two CycloneDX SBOMs attested by two CI runs
/// are the other case, and sending that operator round `--type cyclonedx`
/// returns them exactly where they started — so the single-type set says so
/// instead of naming a flag that cannot help.
///
/// `{:?}` on every interpolated value is deliberate and load-bearing: these
/// strings are read out of a registry-served payload and rendered to a
/// terminal, and Debug-escaping them IS the CWE-150 protection.
fn narrow_by_type_hint(predicate_types: &[String]) -> String {
    match predicate_types {
        [single] => format!("every match carries {single:?}, so --type cannot narrow further"),
        many => format!("narrow with --type to one of {many:?}"),
    }
}

/// Typed discriminant for [`VerifyErrorKind::TrustRootLoad`].
///
/// Each variant maps to a distinct user-facing remediation; replacing the
/// previous free-form `String reason` with this enum lets callers (and
/// integration tests) pattern-match on the failure mode without string
/// matching, and prevents accidental introduction of paths or other
/// sensitive content into the reason text.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TrustRootLoadReason {
    /// `TrustRoot::load_embedded` invoked but the compile-time TUF asset is
    /// not present (Slice 1: not yet shipped).
    #[error("embedded trust-root asset is not bundled in this build")]
    EmbeddedAssetMissing,

    /// I/O error reading a trust-root asset (filesystem or embedded source).
    ///
    /// Not a file an operator named — the two sites that raise it are
    /// `TrustRoot::load_embedded` (the TUF fetch did not produce a root) and
    /// `Verifier::new` (the assembled root is unusable). Both stay
    /// `ConfigError` (78). A path the operator typed raises
    /// [`Self::TrustRootUnreadable`] instead.
    #[error("trust-root asset read failed")]
    AssetReadFailed {
        /// Underlying I/O / source error.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// A trust-root file an operator named could not be read.
    ///
    /// The doors are `--sigstore-trusted-root`, `OCX_SIGSTORE_TRUSTED_ROOT`,
    /// `[trust.sigstore] trusted_root`, and the
    /// `$OCX_HOME/sigstore/trusted-root.json` convention path when it is
    /// present but unusable.
    ///
    /// Split from [`Self::AssetReadFailed`] because it exits 74 `io_error`
    /// rather than 78 `config_error`: a path the operator typed that is
    /// missing, is not a regular file, or is past the read ceiling is a
    /// filesystem failure, and `--key file:<missing>` has always answered 74
    /// for the identical shape. Its own `kind_detail` slug follows, so the
    /// exit code and the word in the JSON envelope tell one story — the
    /// `key_unreadable` precedent.
    ///
    /// The message interpolates its source, which is where the path is: the
    /// verify error's own `Display` renders this reason and stops, so a
    /// message that named no file left an operator with exit 74 and nothing to
    /// act on — the shape `AssetReadFailed` still has, and the reason the
    /// exit-code table's row for this flag can assert the path at all. A
    /// trusted-root path is not a credential; `--key file:<missing>` has always
    /// printed its own.
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
        /// Short detail (e.g., `"unexpected block label"`). Never embed file
        /// paths or other sensitive content here.
        detail: String,
    },

    /// The trust root carries Fulcio anchors but no CT log key.
    ///
    /// Sigstore certificates carry an embedded SCT that the verifier checks
    /// against the CT log's key, so anchors alone cannot verify anything. A
    /// trusted-root document that declares only a certificate authority hits
    /// this; the remedy is one that carries the log keys alongside the anchors.
    #[error(
        "trust root carries no CT log key: supply a trusted-root JSON via --sigstore-trusted-root \
         (see `cosign trusted-root create`, or test/sigstore/generate-trusted-root.py for a self-hosted stack)"
    )]
    NoCtLogKey,

    /// The trusted-root document carried zero certificate-authority anchors.
    #[error("trust root carries no certificate authority anchors")]
    NoCertificateBlocks,

    /// `[trust.sigstore]` declared both `trusted_root` and `trusted_root_json`.
    ///
    /// One trust root, two spellings: taking either would silently discard the
    /// other, and which one wins is not something an operator can predict from
    /// the file. The message names both keys and asks for one.
    #[error(
        "[trust.sigstore] declares both trusted_root and trusted_root_json: keep one \
         (trusted_root_json is what `ocx config push` publishes; trusted_root names a local file)"
    )]
    AmbiguousTrustRootConfig,

    /// Offline verify found no usable trust material: no `--sigstore-trusted-root` /
    /// cached trust root supplying a pinned Rekor key, and the online
    /// fetch/embedded fallback is forbidden offline. The message names the remedy.
    #[error(
        "offline verify has no pinned Rekor key: supply --sigstore-trusted-root, or run an online verify first to populate the trust-root cache"
    )]
    OfflineTrustMaterialUnavailable,
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
            Self::TrustPolicyInvalid(crate::trust::TrustPolicyError::KeyUnreadable { .. })
            | Self::TrustPolicyInvalid(crate::trust::TrustPolicyError::KeyMalformed {
                fault: crate::trust::KeyFault::Path,
                ..
            }) => ExitCode::IoError,
            // A regular file that read fine and holds something that is not a
            // key: the bytes are the problem, which is the 65 `ocx package sign`
            // answers for the same file. An inline `key_pem` falls through to
            // 78 below — there the config text itself is what is wrong.
            Self::TrustPolicyInvalid(crate::trust::TrustPolicyError::KeyMalformed {
                fault: crate::trust::KeyFault::FileBytes,
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
            Self::InvalidEndpointUrl { .. } | Self::NoIdentityProvided | Self::KeyReferenceInvalid(_) => {
                ExitCode::UsageError
            }
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
            Self::TrustPolicyInvalid(crate::trust::TrustPolicyError::KeyUnreadable { .. })
            | Self::TrustPolicyInvalid(crate::trust::TrustPolicyError::KeyMalformed {
                fault: crate::trust::KeyFault::Path,
                ..
            }) => "key_unreadable",
            Self::TrustPolicyInvalid(crate::trust::TrustPolicyError::KeyMalformed {
                fault: crate::trust::KeyFault::FileBytes,
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

/// Select the verify-side variant a `--key` parse failure belongs to.
///
/// Byte-identical split to the sign-side twin, and deliberately duplicated
/// rather than shared: the two taxonomies are separate enums by design, and a
/// shared helper returning "the kind" could only do so by erasing which one.
/// The match is exhaustive with no wildcard -- `KeyRefError` is
/// `#[non_exhaustive]`, but that binds downstream crates only, so in the crate
/// that defines it a new rejection reason is a compile error until it is
/// classified here.
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
    //! on `$?` (79 = not signed, 77 = wrong signer, 83 = Rekor down, 84 = no
    //! referrers API). Any change to these tests is a user-visible contract
    //! change.
    use super::*;

    fn id() -> Identifier {
        Identifier::parse("registry.example/pkg:1.0").expect("parse test identifier")
    }

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
        let kind = VerifyErrorKind::TrustPolicyInvalid(crate::trust::TrustPolicyError::IdentityUnset {
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
        let kms = VerifyErrorKind::TrustPolicyInvalid(crate::trust::TrustPolicyError::KeyReferenceInvalid {
            scope: "ghcr.io/acme/*".into(),
            source: KeyRefError::UnsupportedBackend {
                scheme: crate::oci::sign::Scheme::AwsKms,
            },
        });
        assert_eq!(
            kms.exit_code(),
            ExitCode::UnsupportedKeyBackend,
            "the same 85 the `--key awskms://…` door already answers"
        );
        assert_eq!(kms.kind_detail(), "unsupported_key_backend");

        let unrelated = VerifyErrorKind::TrustPolicyInvalid(crate::trust::TrustPolicyError::IssuerUnset {
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
        use crate::trust::{KeyFault, TrustPolicyError};

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
    fn verify_error_classify_delegates_to_kind() {
        let err = VerifyError::new(id(), VerifyErrorKind::IssuerMismatch);
        assert_eq!(err.classify(), Some(ExitCode::PermissionDenied));
    }

    #[test]
    fn verify_error_source_chain_exposes_kind() {
        use std::error::Error;
        let err = VerifyError::new(id(), VerifyErrorKind::BundleParseFailed);
        let source = err.source().expect("VerifyError has source");
        assert_eq!(format!("{source}"), "bundle parse failed");
    }

    #[test]
    fn invalid_endpoint_url_maps_to_usage_error() {
        use crate::oci::endpoint::UrlRejection;
        // Verify side borrows its own InvalidEndpointUrl variant so the exit-code
        // classification is independent of the sign side.
        let kind = VerifyErrorKind::InvalidEndpointUrl {
            endpoint: "--rekor-url".into(),
            reason: UrlRejection {
                reason: "URL must use HTTPS".into(),
            },
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
    fn attestation_not_found_maps_to_not_found() {
        // 79, the same code `NoSignaturesFound` uses and for the same reason:
        // the scan completed and found nothing to check. Never 65 — "we looked
        // and there is no attestation" is not "an attestation failed to verify",
        // and a gate script that treats the two alike either blocks every
        // unattested artifact or accepts every broken one.
        assert_eq!(VerifyErrorKind::AttestationNotFound.exit_code(), ExitCode::NotFound);
    }

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
    fn key_ref_unsupported_scheme_exits_85_with_its_own_category() {
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
        use crate::cli::ErrorCategory;
        use crate::oci::sign::KeyRef;

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

    #[test]
    fn key_reference_that_is_not_a_backend_is_a_usage_error() {
        // The other half of the `From` split. Same two codes, same two slugs as
        // the sign side -- one vocabulary, two taxonomies.
        use crate::oci::sign::KeyRef;

        for value in ["vault://secret/cosign", "file:"] {
            let kind = VerifyErrorKind::from(KeyRef::parse(value).expect_err("not a usable key reference"));
            assert_eq!(kind.exit_code(), ExitCode::UsageError, "value: {value}");
            assert_eq!(kind.kind_detail(), "key_reference_invalid", "value: {value}");
        }
    }

    #[test]
    fn key_reference_slugs_match_the_sign_side_exactly() {
        // The invariant that makes `error.kind` readable by a script that does
        // not know which verb failed. Asserted against the sign-side function
        // rather than against a literal, so the two can never drift apart while
        // both still "pass their own table".
        use crate::oci::sign::{KeyRef, SignErrorKind};

        for value in ["awskms://alias/release", "vault://secret/cosign"] {
            let rejection = || KeyRef::parse(value).expect_err("not a usable key reference");
            assert_eq!(
                VerifyErrorKind::from(rejection()).kind_detail(),
                SignErrorKind::from(rejection()).kind_detail(),
                "one failure must read as one word on both paths: {value}"
            );
        }
    }

    #[test]
    fn kind_detail_values_are_stable() {
        // C-S1-1 frozen contract: these strings ship in JSON envelopes and consumer
        // scripts dispatch on them. A rename or typo here is a user-visible breaking
        // change. The exhaustive match in `kind_detail()` ensures a new variant forces
        // a new arm there; this table ensures the *string value* for each arm is pinned.
        use crate::oci::endpoint::UrlRejection;
        use crate::oci::sign::Scheme;
        use VerifyErrorKind::*;

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
                TrustPolicyInvalid(crate::trust::TrustPolicyError::IdentityUnset {
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
                    reason: UrlRejection {
                        reason: "URL must use HTTPS".into(),
                    },
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
