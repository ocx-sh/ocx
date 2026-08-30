// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Verify pipeline — full keyless Sigstore verification state machine.
//!
//! Per
//! [`adr_oci_referrers_signing_v1.md`](../../../../../.claude/artifacts/adr_oci_referrers_signing_v1.md)
//! S1-H: resolve target → list referrers (Referrers API, or the OCI referrers
//! tag-schema fallback when the registry has none) → fetch the subject
//! manifest → try each v0.3 bundle referrer (ANY-of) → hand the bundle to
//! `sigstore`'s verifier → verify the Rekor SET and inclusion proof → match
//! identity + issuer → emit [`VerifyResult`]. The first candidate that fully
//! passes wins; if all fail the aggregate error is returned.
//!
//! **Where the cryptography lives.** Certificate-chain building, SCT
//! verification, the ECDSA signature check, the transparency-log body binding
//! (CVE-2022-36056 / GHSA-whqx class) and the certificate-validity-vs-integrated-time
//! check are all `sigstore::bundle::verify::Verifier`'s. This module owns no
//! X.509, ASN.1 or signature code. What it still owns is the Rekor Signed Entry
//! Timestamp and the Merkle inclusion proof, which `sigstore` 0.14 leaves as
//! `TODO`s (sigstore-rs#285) — those live in [`tlog`], computed by `sigstore`'s
//! own crypto primitives.
//!
//! `Verifier` hashes a *preimage* rather than accepting a digest, so the
//! pipeline fetches the subject manifest bytes and re-hashes them against the
//! digest the index resolved. That is not overhead for its own sake: it is the
//! only way to bind the verification to bytes the registry actually serves.
//!
//! The trust root (Fulcio CAs + CT log keys) is injected via
//! [`VerifyContext::trust_root`] (C-S1-3); the Rekor public key used for SET
//! verification is pinned from it, or fetched from
//! [`VerifyContext::rekor_url`] `/api/v1/log/publicKey`.

use sigstore::bundle::verify::Verifier;
use sigstore::bundle::verify::policy::{PolicyResult, VerificationPolicy};
use sigstore::rekor::apis::configuration::Configuration as RekorConfiguration;
use url::Url;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::attestation_sidecar;
use super::discovery::DiscoveryMethod;
use super::dsse::{self, VerifiedAttestation, VerifiedEnvelope};
use super::error::{TrustRootLoadReason, VerifyError, VerifyErrorKind};
use super::identity::{self, matching_policies, oidc_issuer, parse_certificate, subject_identity};
use super::signing_instant::SigningInstant;
use super::simplesigning_read::{self, SidecarKind, SidecarScan};
use super::tlog;
use super::trust_cache::TrustRootCache;
use super::trust_root::TrustRoot;
use crate::file_structure::StateStore;
use crate::oci::attest::predicate::{PredicateType, sbom_predicate_type_uri};
use crate::oci::attest::{
    COSIGN_SIGN_PREDICATE_TYPE, MAX_ATTESTATION_CANDIDATES, MAX_ATTESTATION_ENVELOPE_BYTES, MAX_TOTAL_ATTESTATION_BYTES,
};
use crate::oci::client::error::ClientError;
use crate::oci::client::{Client, OciTransport, ReferrersListing, sibling_tag_reference};
use crate::oci::index::{Index, IndexOperation};
use crate::oci::referrer::media_types::{
    ANNOTATION_BUNDLE_CONTENT, ANNOTATION_BUNDLE_PREDICATE_TYPE, BUNDLE_CONTENT_DSSE, COSIGN_SBOM_ARTIFACT_TYPE,
    COSIGN_SIG_ARTIFACT_TYPE, SIGSTORE_BUNDLE_V03,
};
use crate::oci::resolve_target::{ResolveTargetError, SignTarget, resolve_sign_target};
use crate::oci::sign::bundle::{MAX_BUNDLE_SIZE_BYTES, parse_bundle};
use crate::oci::sign::{KeyBackendKind, SignatureFormat};
use crate::oci::{Digest, Identifier, ImageManifest, Platform, native};
use crate::trust::PolicyBackend;
use sigstore_protobuf_specs::dev::sigstore::bundle::v1::{Bundle, bundle, verification_material};
use sigstore_protobuf_specs::dev::sigstore::rekor::v1::InclusionProof as ProtoInclusionProof;

pub(super) const ACCEPTED_MANIFEST_TYPES: &[&str] = &[
    crate::oci::OCI_IMAGE_MEDIA_TYPE,
    "application/vnd.docker.distribution.manifest.v2+json",
];

/// Maximum accepted size of a referrer manifest, in bytes.
///
/// A Sigstore-signature referrer manifest is an OCI image manifest carrying a
/// config + one bundle layer + a subject descriptor — a few hundred bytes. The
/// declared descriptor size (untrusted) is rejected up front when over-cap, and
/// the actual fetched body is re-checked against this cap after the read (a
/// registry can lie about the size) — see [`pull_referrer_manifest_capped`].
/// 256 KiB is generous headroom.
pub(super) const MAX_REFERRER_MANIFEST_BYTES: u64 = 256 * 1024;

/// Maximum number of signature referrers examined during an ANY-of verify.
///
/// Bounds the work a hostile registry can force by listing many candidate
/// referrers; combined with the per-item size caps this bounds total download.
pub(super) const MAX_SIGNATURE_CANDIDATES: usize = 8;

/// Cross-candidate byte budget over referrer-manifest descriptor sizes.
///
/// Belt to [`MAX_SIGNATURE_CANDIDATES`]: a registry cannot force unbounded
/// aggregate manifest download by listing many candidates each just under the
/// per-item cap. Each candidate's bundle blob is separately capped at
/// [`MAX_BUNDLE_SIZE_BYTES`].
const MAX_TOTAL_REFERRER_BYTES: u64 = 4 * 1024 * 1024;

/// Hard backstop on how many listed referrers one scan will iterate, whatever
/// the per-mode candidate cap is.
///
/// A candidate discriminated as the other content kind costs a manifest and a
/// bundle fetch without consuming a candidate slot — which is the point, since
/// otherwise attestations crowd a signature out of the scan — so the candidate
/// cap alone no longer bounds the loop. This bounds the number of iterations
/// regardless of what each one costs, well above any legitimate referrer count
/// for one subject.
const MAX_REFERRER_LISTING_ITERATION: usize = 256;

/// How many answers a scan is looking for.
///
/// Kept apart from [`VerifyContentMode`] so "which content kind" and "how many
/// answers" stay two questions: the signature scan is ANY-of because *is this
/// signed* has one answer, and the attestation scan is collect-all because
/// *which SBOMs does this carry* does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScanArity {
    /// Stop at the first candidate that fully passes.
    FirstMatch,
    /// Examine every candidate the caps allow.
    All,
}

/// What kind of signed content a verify run is looking for.
///
/// Selects the caps before the first fetch and gates which bundle content a
/// candidate may carry. A candidate's own kind is unknowable until its bundle
/// is parsed, so deriving the bounds from it would be circular — see
/// `adr_sbom_attestations.md` D-d.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyContentMode {
    /// An artifact message signature — what `ocx package verify` has always
    /// looked for, and the mode every existing caller passes.
    Signature,
    /// An in-toto attestation carried in a DSSE envelope.
    Attestation {
        /// Narrows the search to one predicate type; `None` accepts any.
        predicate_type: Option<PredicateType>,
    },
}

/// Whether a run demands cryptographic verification, or merely reads.
///
/// Resolved once per invocation from the flags and the trust policies, and
/// carried into the pipeline rather than re-derived: "is there a policy" is a
/// question about configuration, and the pipeline must not be able to answer it
/// differently from the CLI that reported the mode to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationMode {
    /// Every document must carry a signature this run can verify against the
    /// resolved policies. An unsigned attachment is refused, never listed.
    Demand,
    /// Nothing is verified and no cryptography runs. Every document — raw
    /// attachment or bundle payload — is read and reported as unverified.
    ///
    /// This is what makes `ocx package sbom` usable with no Sigstore setup at
    /// all. It is not a relaxation of `Demand`: no key, certificate or log
    /// entry is consulted, so nothing here may ever be presented as verified.
    Permissive,
}

/// The untrusted-byte bounds one verify run enforces, chosen by content mode.
///
/// Three integers, fixed per mode: nothing here will grow a heap field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ContentCaps {
    /// Per-candidate bundle-blob cap, on the declared size and on bytes read.
    bundle_bytes: usize,
    /// Candidates examined before the scan stops.
    candidates: usize,
    /// Cross-candidate budget, charged from bytes actually read.
    total_bytes: u64,
}

impl VerifyContentMode {
    /// The bounds this mode enforces, resolved before the first fetch.
    ///
    /// `Signature` returns exactly the shipped numbers; an attestation bundle
    /// is a different artifact class (a whole SBOM, not a 500-byte signature)
    /// and gets the `MAX_ATTESTATION_*` bounds instead. Hoisting the larger
    /// numbers into the shared path would silently relax `ocx package verify`.
    fn caps(&self) -> ContentCaps {
        match self {
            Self::Signature => ContentCaps {
                bundle_bytes: MAX_BUNDLE_SIZE_BYTES,
                candidates: MAX_SIGNATURE_CANDIDATES,
                total_bytes: MAX_TOTAL_REFERRER_BYTES,
            },
            Self::Attestation { .. } => ContentCaps {
                bundle_bytes: MAX_ATTESTATION_ENVELOPE_BYTES,
                candidates: MAX_ATTESTATION_CANDIDATES,
                total_bytes: MAX_TOTAL_ATTESTATION_BYTES as u64,
            },
        }
    }
}

/// Context passed into [`VerifyPipeline::run`] — all external dependencies.
pub struct VerifyContext<'a> {
    /// Target identifier (`registry/repo:tag[@digest]`).
    pub identifier: &'a Identifier,
    /// Platform to narrow into, when one was requested (C-010).
    ///
    /// `None` acts on **whatever the reference resolved to**, index or bare
    /// manifest; `Some(..)` narrows into an index and is an error when the
    /// resolved object is not one. The branch is on what resolution returned,
    /// never on the reference's form — see [`resolve_sign_target`].
    pub platform: Option<&'a Platform>,
    /// Resolved ANY-of trust policies the signing certificate must satisfy: a
    /// single exact pair when `--certificate-identity`/`--certificate-oidc-issuer`
    /// are supplied (flag mode), or the scope-matched `[[trust.policy]]` set
    /// (policy mode). See `crate::trust`.
    pub policies: &'a [crate::trust::CompiledPolicy],
    /// When true, bypass the referrers-capability cache.
    pub no_cache: bool,
    /// Index for resolving tag → digest.
    pub index: &'a Index,
    /// Trust root (Fulcio CA certs + optional pinned Rekor key); C-S1-3 seam.
    pub trust_root: &'a TrustRoot,
    /// Rekor URL (C-S1-3 injection seam). Default: `https://rekor.sigstore.dev`.
    pub rekor_url: &'a Url,
    /// State store owning the referrers-capability and trust-root cache layouts.
    pub state: &'a StateStore,
    /// When true, no Sigstore trust-services network: the Rekor key must come
    /// from the (pinned/cached) trust root, never a fetch. The artifact registry
    /// is still used (verify inherently reads the signature from where it lives).
    /// On a successful online run the trust material is cached for later offline
    /// verifies. See `adr_offline_verify_trust_cache.md`.
    pub offline: bool,
    /// Which content kind to look for. `Signature` is today's behaviour.
    pub content: VerifyContentMode,
    /// Whether this run demands verification. Inert for
    /// [`VerifyContentMode::Signature`], whose entire purpose is to verify:
    /// [`VerifyPipeline::run`] never reads it.
    pub verification: VerificationMode,
    /// The cosign wire shape this run pins (D9).
    ///
    /// `None` prefers a bundle and falls back to a simplesigning sidecar only
    /// when the bundle shape is **absent** — no candidate matched and none was
    /// refused, so a fetched-and-rejected bundle fails closed with its own exit
    /// code instead of promoting the weaker shape. `Some(..)` pins: the other shape
    /// is never discovered — not discovered and then ignored — so a pinned
    /// `simplesigning` against a subject carrying only a bundle answers "no
    /// signatures found" rather than silently verifying the bundle.
    pub signature_format: Option<SignatureFormat>,
    /// Accept a keyless simplesigning sidecar that carries **no**
    /// transparency-log evidence (`--allow-unlogged-signature`).
    ///
    /// Off by default, and off is the contract: a cosign `sha256-<hex>.sig`
    /// whose layer has no `dev.sigstore.cosign/bundle` annotation proves nothing
    /// about *when* its short-lived Fulcio certificate was used, so it is
    /// refused. The opt-out exists for air-gapped CI, where the entry cannot be
    /// fetched or was never written; it is the counterpart to cosign's
    /// `--insecure-ignore-tlog` and, like it, is the caller saying they accept
    /// a signature nothing timestamps. Inert on every other path — a bundle's
    /// transparency evidence is mandatory under keyless and optional under a
    /// key regardless of this flag.
    pub allow_unlogged_signature: bool,
    /// Widen the scan from ANY-of first-match to every candidate the caps allow.
    ///
    /// Set by the callers that render `signatures[]`; left `false` by the
    /// install-time auto-verify hook, which asks only "is this signed" and pays
    /// no crypto for candidates nobody reads. It widens the **report**, never
    /// the verdict: [`VerifyPipeline::run`]'s answer is the first candidate that
    /// fully passed under either setting.
    pub report_all: bool,
}

/// Result emitted by a successful verify pipeline run.
#[derive(Debug)]
pub struct VerifyResult {
    /// Digest of the subject manifest that was verified.
    pub subject_digest: Digest,
    /// What carried this signature — and **not always a manifest**.
    ///
    /// [`Self::signature_format`] is the discriminator, and
    /// [`Self::discovery_method`] is not. A [`SignatureFormat::Bundle`] result
    /// names the OCI referrer manifest the listing pointed at; every
    /// [`SignatureFormat::Simplesigning`] result names the **layer** blob,
    /// because one layer is one signature there and the manifest digest would
    /// name all of them at once (`simplesigning_read`'s [`SidecarScan::refused`]
    /// states the same deviation for its own half).
    ///
    /// The door cannot stand in for the shape, because a cosign sidecar is
    /// reachable through more than one: `scan_simplesigning` hands
    /// `read_sidecar_manifest` the *listing's* `via`, so a `sha256-<hex>.sig`
    /// referrer the Referrers API served reports a layer digest while
    /// `discovery_method` reads `referrers_api`. A consumer keyed on the door
    /// would address that layer as a manifest and 404 on the one case the rule
    /// exists to catch.
    ///
    /// So: `GET /v2/<name>/manifests/<digest>` only under
    /// `signature_format == "bundle"`; under `"simplesigning"` it is a blob.
    ///
    /// [`SidecarScan::refused`]: super::simplesigning_read::SidecarScan::refused
    pub referrer_digest: Digest,
    /// What produced the signature: a Fulcio certificate, or a pinned key.
    pub key_backend: KeyBackendKind,
    /// Cert SAN that signed the subject. **Absent under a key** — a key
    /// signature carries no certificate, so there is no identity to read, and
    /// an empty string here would report "signed by nobody" as if it were a
    /// fact about the certificate rather than the absence of one.
    pub certificate_identity: Option<String>,
    /// Cert OIDC issuer URL. Absent under a key, for the same reason.
    pub certificate_oidc_issuer: Option<String>,
    /// Rekor integrated time (UTC epoch seconds) of the signature entry.
    ///
    /// Absent when the bundle carries no transparency entry — legal under a key
    /// (D10), impossible under keyless.
    pub signed_at: Option<u64>,
    /// Which cosign wire shape carried this signature.
    pub signature_format: SignatureFormat,
    /// Which discovery door this signature came through.
    pub discovery_method: DiscoveryMethod,
    /// Rekor log index of the entry this signature's transparency evidence was
    /// **verified** against, and the primary dedup key when present.
    ///
    /// Only ever set from an entry that passed [`verify_rekor_set`] — a
    /// synthesised entry (the one `simplesigning_read` builds so `sigstore`'s
    /// `CheckedBundle` has the one it demands) must never reach this field,
    /// because a reported log index is read as a claim that the signature is in
    /// the log at that position.
    pub rekor_log_index: Option<u64>,
}

/// A verified signature plus the material D6's dedup key falls back on.
///
/// The signature bytes travel beside [`VerifyResult`] rather than on it: they
/// are scan bookkeeping, and a reported result carrying raw signature bytes
/// would invite a consumer to treat them as evidence of something the result
/// already states.
#[derive(Debug)]
pub struct VerifiedSignature {
    /// The verification facts, as reported.
    pub result: VerifyResult,
    /// The raw signature bytes this candidate carried.
    pub signature: Vec<u8>,
}

/// D6's dedup key: the Rekor log index when present, otherwise the material.
///
/// Two doors onto one signature (an OCI 1.1 referrer and the cosign sidecar
/// tag; the Referrers API and the fallback tag) must contribute one row to
/// `signatures[]`, not two. The fallback branch is not academic: key mode
/// defaults to no Rekor upload, so the log index is absent in exactly the case
/// where double-discovery is most likely.
#[derive(Debug, PartialEq, Eq)]
enum SignatureKey {
    /// The transparency log's own identifier for this entry.
    RekorLogIndex(u64),
    /// No verified transparency evidence: the signature bytes, the subject they
    /// bind, and the wire shape that carried them.
    Material {
        signature: Vec<u8>,
        subject_digest: String,
        signature_format: SignatureFormat,
    },
}

impl VerifiedSignature {
    /// This candidate's dedup key.
    fn dedup_key(&self) -> SignatureKey {
        match self.result.rekor_log_index {
            Some(log_index) => SignatureKey::RekorLogIndex(log_index),
            None => SignatureKey::Material {
                signature: self.signature.clone(),
                subject_digest: self.result.subject_digest.to_string(),
                signature_format: self.result.signature_format,
            },
        }
    }
}

/// One verified attestation plus the verification facts about the candidate it
/// came from.
///
/// [`VerifiedAttestation`] alone cannot populate the report: `referrer_digest`,
/// `certificate_identity`, `certificate_oidc_issuer` and `signed_at` all live on
/// [`VerifyResult`], and the default `ocx package sbom` listing promises all
/// four. [`VerifyResult`] carries no attestation field for the mirror reason —
/// an `Option` there plus a `Vec` here would be two contracts disagreeing about
/// how many attestations a subject can have (D-d).
#[derive(Debug)]
pub struct AttestationMatch {
    /// Verification facts about the referrer this attestation came from.
    pub verify: VerifyResult,
    /// The attestation itself, as the publisher signed it.
    pub attestation: VerifiedAttestation,
}

/// A candidate that was examined and refused, kept so a caller can report it.
///
/// A scan that returns matches has still usually looked at candidates that
/// failed, and dropping those makes "3 attestations" indistinguishable from
/// "3 attestations, 2 refused" — the second is the one worth acting on.
#[derive(Debug)]
pub struct RefusedCandidate {
    /// The referrer's digest, verbatim as the registry listed it.
    pub referrer_digest: String,
    /// Why this candidate was refused.
    pub reason: VerifyErrorKind,
}

/// Everything an attestation scan found: what verified, and what did not.
///
/// Refusals travel beside the matches rather than failing the scan. Failing
/// closed on any candidate error would hand a single malformed referrer the
/// power to hide every valid attestation on the subject — the DoS the
/// per-candidate independence exists to prevent.
#[derive(Debug)]
pub struct AttestationScan {
    /// Every candidate that verified, in listing order.
    pub matches: Vec<AttestationMatch>,
    /// Every **unsigned** SBOM referrer found, in digest order.
    ///
    /// A separate list rather than an `Option` on [`AttestationMatch`], because
    /// the two are not the same claim wearing different clothes: a verified
    /// match carries a predicateType read out of a signed payload and a subject
    /// digest a signature *proved*, and an unverified one carries a media type
    /// the registry asserted and a subject it merely claims. Keeping them apart
    /// makes a caller that ignores this field under-report rather than
    /// mis-report, and leaves the verified path's shape untouched.
    pub unverified: Vec<UnverifiedSbom>,
    /// Every candidate that was examined and refused, in listing order.
    pub refused: Vec<RefusedCandidate>,
    /// The platform manifest `--platform` narrowed to, when this scan also read
    /// the enclosing index behind it (**C-011**).
    ///
    /// A *fact about the scan*, not a preference: it says which of the two
    /// subjects read was the per-platform one, and the shadowing decision keys
    /// on it. `None` whenever a single subject was read — nothing was narrowed,
    /// or membership could not be proved — which is what makes "nothing is
    /// superseded" true by construction rather than by a caller remembering to
    /// check.
    pub platform_subject: Option<Digest>,
}

/// An SBOM attached without a signature: the document itself as the referrer
/// payload, typed by its own media type.
///
/// Nothing here is proven — the registry served the bytes and said what they
/// are, and there is no certificate, envelope or log entry to check any of it
/// against. That is the whole content of the type, and why every consumer must
/// label it as unverified rather than fold it into a listing beside signed
/// documents.
#[derive(Debug)]
pub struct UnverifiedSbom {
    /// Digest of the OCI referrer manifest carrying the document.
    pub referrer_digest: Digest,
    /// The subject the referrer is attached to — the digest this scan
    /// resolved, which the referrer claims rather than proves.
    pub subject_digest: Digest,
    /// The predicateType URI the referrer's `artifactType` stands for, so a
    /// consumer compares one vocabulary across both trust classes.
    pub predicate_type: String,
    /// The document, verbatim as the registry served it. Not a `RawValue`:
    /// `text/spdx` is tag-value text, so an SBOM payload is not always JSON.
    pub document: Vec<u8>,
}

/// Verify pipeline entry point.
pub struct VerifyPipeline;

impl VerifyPipeline {
    /// Run the verify pipeline against a [`VerifyContext`].
    ///
    /// The registry transport is derived from `client` internally, so the
    /// public API never exposes `&dyn OciTransport` (ADR Amendment 1, Option 3).
    ///
    /// Returns every signature that verified, in scan order and deduplicated by
    /// D6's key. **The verdict is the first element** and is the same under
    /// either [`VerifyContext::report_all`] setting — widening the arity appends
    /// candidates behind the answer, it never changes it.
    pub async fn run(client: &Client, ctx: VerifyContext<'_>) -> Result<Vec<VerifyResult>, VerifyError> {
        let identifier = ctx.identifier.clone();
        Self::run_inner(client, ctx)
            .await
            .map_err(|kind| VerifyError::new(identifier, kind))
    }

    /// Collect **every** verified attestation on the target, bounded by the
    /// attestation-mode caps.
    ///
    /// The two content modes share [`Self::verify_one_referrer`] and differ only
    /// here: [`Self::run`] is ANY-of — the first fully-passing candidate wins,
    /// which is the right answer to "is this artifact signed" — while this is
    /// collect-all, because first-match is the wrong answer to "which SBOMs does
    /// this artifact have". Under an `identity_regexp` policy, or across a
    /// signing-identity rotation where old and new coexist as two ANY-of entries
    /// by design, first-match would let the *registry's listing order* pick which
    /// document a consumer reads.
    ///
    /// Candidates that failed are returned alongside the ones that passed
    /// ([`AttestationScan::refused`]) rather than failing the scan, so a caller
    /// can report "N verified, M refused" instead of silently under-reporting.
    ///
    /// # Errors
    ///
    /// [`VerifyErrorKind::AttestationNotFound`] (79) when the scan ends with no
    /// match, the most actionable per-candidate failure when one was recorded,
    /// and the fail-closed cap refusals when a bound truncated the scan — an
    /// incomplete list is a wrong answer to a question about *every* attestation.
    pub async fn run_attestations(client: &Client, ctx: VerifyContext<'_>) -> Result<AttestationScan, VerifyError> {
        let identifier = ctx.identifier.clone();
        Self::run_attestations_inner(client, ctx)
            .await
            .map_err(|kind| VerifyError::new(identifier, kind))
    }

    async fn run_attestations_inner(
        client: &Client,
        ctx: VerifyContext<'_>,
    ) -> Result<AttestationScan, VerifyErrorKind> {
        let target = Self::resolve_target(client, &ctx).await?;
        let mut budget = ScanBudget::new(ctx.content.caps());
        // **C-011.** The enclosing index is a second *subject to read*, not a
        // fallback to try when the first one is empty. A signature run is ANY-of,
        // so [`Self::scan_with_index_fallback`] can stop at the first answer; an
        // attestation run is collect-all, and "which SBOMs does this carry" is
        // answered wrongly by either subject alone — cosign attests a
        // multi-platform tag at the index while OCX pins a platform manifest, so
        // reading only one of them hides documents that are genuinely attached.
        //
        // The gate is the same one, and it is the whole gate: no enclosing index
        // (nothing was narrowed, or it was unfetchable), or a subject the index
        // does not list, and the second pass never runs.
        let index_target = target.index_signature_subject().map(|index_digest| ScanTarget {
            image: target.image.clone(),
            subject_digest: index_digest.clone(),
            // The index is the subject of this pass. No further indirection: an
            // index listing an index would need its own membership proof, which
            // nothing here has.
            enclosing_index: None,
            index_members: Vec::new(),
        });
        // Reported so the shadowing decision has a fact to key on rather than a
        // guess. `None` whenever only one subject was read — which is what makes
        // "nothing was narrowed, so nothing is superseded" true by construction.
        let platform_subject = index_target.as_ref().map(|_| target.subject_digest.clone());
        let passes = || std::iter::once(&target).chain(index_target.as_ref());

        if ctx.verification == VerificationMode::Permissive {
            // One budget across every pass, and no signed pass at all: with
            // nothing being verified, a bundle referrer is read for its payload
            // exactly as a raw attachment is read for its bytes, and both list as
            // unverified. Reading them in one digest-ordered pass per subject is
            // what makes the two kinds unable to starve each other — there is no
            // second allowance for volume of one kind to spend on behalf of the
            // other, and no second allowance a second subject buys either.
            let mut unverified = Vec::new();
            let mut refused = Vec::new();
            for pass in passes() {
                let (found, pass_refused) = Self::scan_unverified(client, &ctx, pass, &mut budget).await?;
                unverified.extend(found);
                refused.extend(pass_refused);
            }
            if unverified.is_empty() {
                // Same ladder the signed pass ends on: a refusal that was
                // recorded is more actionable than "none found", which would
                // send a publisher looking for an attach that did happen.
                return Err(best_failure(refused).unwrap_or(VerifyErrorKind::AttestationNotFound));
            }
            return Ok(AttestationScan {
                matches: Vec::new(),
                unverified,
                refused,
                platform_subject,
            });
        }

        // Demand. Raw attachments are refused wholesale and *without a fetch*,
        // so untrusted volume cannot spend one byte or one candidate slot of
        // the budget the signed pass needs — the starvation question is closed
        // structurally here rather than by rationing a shared allowance.
        let mut matches = Vec::new();
        let mut signed_refused = Vec::new();
        let mut unsigned_refused = Vec::new();
        // The first pass's verdict, kept for the empty-scan ladder below. The
        // platform manifest is the object the user named, so its answer is the
        // more actionable one when neither subject carries anything.
        let mut scan_failure = None;
        // The `.sbom` probe's own fault, kept for the same ladder and for the
        // reason `refuse_unsigned` defers it: it may not fail a run that
        // verifies, and it may not be silently spent as "nothing attached".
        let mut sidecar_fault = None;
        for pass in passes() {
            let (pass_refused, pass_fault) = Self::refuse_unsigned(client, &ctx, pass).await?;
            unsigned_refused.extend(pass_refused);
            sidecar_fault = sidecar_fault.or(pass_fault);
            match Self::scan(client, &ctx, pass, ScanArity::All, &mut budget).await {
                Ok(outcome) => {
                    for (verify, attestation) in outcome.matches {
                        // `verify_one_referrer` returns `Some` for every candidate
                        // it verified in attestation mode, so `None` here would
                        // mean the mode and the outcome had drifted apart. Fail
                        // closed rather than report a match with nothing in it.
                        let attestation = attestation.ok_or(VerifyErrorKind::AttestationNotFound)?;
                        matches.push(AttestationMatch { verify, attestation });
                    }
                    signed_refused.extend(outcome.refused);
                }
                // This subject carries nothing signed. Not fatal on its own —
                // the other subject may well carry the document — so it is
                // remembered and only spent if every pass comes back empty.
                //
                // `finish_scan` consumed this pass's per-candidate refusals to
                // build the kind, so they do not reach the report. That loss is
                // bounded to the case where this pass verified nothing *and*
                // another one did — which before C-011 was not a listing at all
                // but an outright failure carrying exactly this kind.
                Err(kind @ (VerifyErrorKind::AttestationNotFound | VerifyErrorKind::NoSignaturesFound)) => {
                    scan_failure.get_or_insert(kind);
                }
                Err(kind) => return Err(kind),
            }
        }

        if matches.is_empty() {
            // If unsigned attachments were refused on the way, that refusal is
            // the actionable answer — "this SBOM is attached without a signature
            // and you demanded one" tells an operator what to do, where "none
            // found" states something false about the subject.
            //
            // A `.sbom` probe fault sits between the two: less actionable than
            // a refusal that names a real attachment, more actionable than a
            // "not found" that would state something this run never got to
            // check.
            let kind = scan_failure.unwrap_or(VerifyErrorKind::AttestationNotFound);
            return Err(best_failure(unsigned_refused).or(sidecar_fault).unwrap_or(kind));
        }

        let mut refused = signed_refused;
        refused.extend(unsigned_refused);
        Ok(AttestationScan {
            matches,
            // Nothing unverified is ever listed under `Demand`: an unsigned
            // attachment was refused above, and every signed match is in
            // `matches`.
            unverified: Vec::new(),
            refused,
            platform_subject,
        })
    }

    /// Read every SBOM the target carries **without verifying any of it**:
    /// raw attachments (`cosign attach sbom` / `oras attach`) and the payloads
    /// of Sigstore bundles alike.
    ///
    /// Reached only under [`VerificationMode::Permissive`]. No cryptography
    /// runs, by construction rather than by omission: no certificate, no
    /// transparency-log entry and no trust root is consulted on this path, so
    /// nothing it returns may ever be presented as verified. What it does
    /// enforce is structure — the caller's [`ScanBudget`], and each payload's
    /// own claims about itself, which is all that is checkable without a key.
    ///
    /// Both referrer kinds are read in one digest-ordered pass over one
    /// budget. That is what stops a registry starving one kind with volume of
    /// the other: there is no second allowance to spend.
    ///
    /// Reachable only from [`Self::run_attestations`]. `ocx package verify
    /// --attestation` goes through [`Self::run`], which never calls this, so a
    /// document read here can never become a *verification* candidate — the
    /// separation is structural, not a filter someone has to remember.
    async fn scan_unverified(
        client: &Client,
        ctx: &VerifyContext<'_>,
        target: &ScanTarget,
        budget: &mut ScanBudget,
    ) -> Result<(Vec<UnverifiedSbom>, Vec<RefusedCandidate>), VerifyErrorKind> {
        let VerifyContentMode::Attestation { .. } = &ctx.content else {
            // Unreachable through the public API; a signature run always
            // verifies and never reaches this pass.
            return Ok((Vec::new(), Vec::new()));
        };
        let transport = client.transport();
        let ScanTarget {
            image, subject_digest, ..
        } = target;

        // The Unsupported verdict no longer refuses the operation: the OCI referrers
        // tag-schema fallback (`list_referrers_with_fallback` /
        // `append_referrer_fallback_index`) serves a registry without the Referrers
        // API. See `adr_oci_referrers_signing_v1.md`, Amendment 10 — the fallback
        // index is a mutable tag anyone with push access authors, and the residual
        // attack surface that reverses S1-F is recorded there.
        //
        // One unfiltered listing rather than one request per artifact type. The
        // client-side filter below is the real one either way: the OCI spec
        // permits a registry to ignore the server-side `artifactType`
        // parameter, so a filtered listing would still have to be re-filtered
        // here — at several times the requests.
        let ReferrersListing {
            descriptors: listed,
            via,
        } = transport
            .list_referrers_with_fallback(image, subject_digest, None)
            .await
            .map_err(map_client_error)?;
        // D-5 reports this on signatures[].discovery_method. Logged rather than
        // dropped here: a listing served by the mutable fallback tag is a
        // materially weaker provenance claim than one the registry computed.
        tracing::debug!("unverified SBOM referrers discovered via {via}");
        // An absent `artifactType` is dropped, unlike in the signed scan where
        // it is kept. The asymmetry is the point: there the bundle parse
        // downstream fail-closes on a non-bundle, so keeping an untyped
        // candidate costs a fetch and admits nothing. Here the artifactType is
        // the only statement of what a raw payload is, so an untyped referrer
        // is not an SBOM referrer — treating it as one would list an arbitrary
        // blob under a predicate type nothing ever claimed.
        // A prefilter, and only that: it decides which candidates are worth a
        // request and which decode each one needs. `--type` is deliberately
        // *not* applied here even though the listing appears to carry the
        // answer, because the listing's `artifactType` is unchecked against the
        // manifest it points at — narrowing on it would drop a referrer whose
        // layer is the requested type, and admit one whose layer is not.
        // Neither shape is narrowed before its payload's own claim is read.
        let mut candidates: Vec<(crate::oci::Descriptor, UnverifiedPayload)> = listed
            .into_iter()
            .filter_map(|descriptor| {
                let artifact_type = descriptor.artifact_type.as_deref()?;
                if is_unsigned_sbom_artifact_type(artifact_type) {
                    return Some((descriptor, UnverifiedPayload::Raw));
                }
                (artifact_type == SIGSTORE_BUNDLE_V03).then_some((descriptor, UnverifiedPayload::Bundle))
            })
            .collect();
        // Digest order, for the reason `order_candidates` sorts: a total order
        // the registry does not choose, so the listing is reproducible.
        candidates.sort_by(|(left, _), (right, _)| left.digest.cmp(&right.digest));

        let total_candidates = candidates.len();
        let mut found = Vec::new();
        let mut refused = Vec::new();
        let mut processed = 0usize;
        let mut first_unexamined = None;
        for (descriptor, payload) in candidates {
            if !budget.may_examine() {
                first_unexamined = Some(descriptor.digest.clone());
                break;
            }
            budget.examined();
            processed = processed.saturating_add(1);
            match Self::read_unverified_referrer(transport, ctx, budget, target, &descriptor, payload).await {
                // Every layer the referrer carries. An empty `Vec` is a `--type`
                // narrowing miss: this candidate is fine, it simply is not the
                // document that was asked for. It spent a slot and records no
                // failure, exactly as the signed scan's `TypeNarrowed` does
                // (S-017).
                Ok(sboms) => found.extend(sboms),
                Err(reason) => refused.push(RefusedCandidate {
                    referrer_digest: descriptor.digest.clone(),
                    reason,
                }),
            }
        }

        // A truncated permissive pass is reported as a refusal, not raised as
        // an error — the one place this pass's posture differs from the signed
        // one's, and deliberately so. The signed pass fails closed because a
        // partial answer about *signed* documents understates what a publisher
        // vouched for. Here nothing is vouched for by anyone, and raising would
        // let a registry turn a working listing into a hard failure by the
        // cheapest means available: attach enough junk. The refusal travels out
        // beside the results, turns the CLI summary to `partial_failure`, and
        // names the first referrer that was not looked at.
        if let Some(stop) = budget.stop {
            refused.push(RefusedCandidate {
                referrer_digest: first_unexamined.unwrap_or_default(),
                reason: truncation_failure(budget.caps, stop, total_candidates.saturating_sub(processed)),
            });
        }

        // The `sha256-<hex>.sbom` sidecar tag — spec §WP5's SBOM half, and the
        // third cosign shape that no listing can reach. Measured against cosign
        // v3.1.1: `cosign attach sbom <ref>` writes a manifest carrying neither
        // `artifactType` nor `subject`, and the Referrers API returns an empty
        // index for the subject afterwards, so the tag is the whole discovery
        // story exactly as it is for `.att`.
        //
        // Read after the referrers pass and unconditionally, not as a fallback
        // for having found nothing. `.att`'s door is gated on `matches
        // .is_empty()` because a signature scan is ANY-of and stops at the first
        // answer; this one is collect-all — `ocx package sbom` must report every
        // document the subject carries — so an SBOM found through the Referrers
        // API must not hide one attached through the tag.
        //
        // Skipped once a bound has already stopped the pass: the refusal above
        // has said the listing is partial, and opening one more door would spend
        // budget the caller was just told had run out.
        if budget.stop.is_none() {
            budget.examined();
            match Self::read_sbom_sidecar_tag(transport, ctx, budget, target).await {
                // Every layer the tag carries (#386). No tag, or a `--type`
                // narrowing miss, is an empty `Vec` — neither is a failure.
                Ok(sboms) => found.extend(sboms),
                // `referrer_digest` is the tag's manifest digest when the read
                // got far enough to learn it, and empty when it did not — the
                // same convention the truncation row above uses.
                Err((referrer_digest, reason)) => refused.push(RefusedCandidate {
                    referrer_digest: referrer_digest.map(|d| d.to_string()).unwrap_or_default(),
                    reason,
                }),
            }
        }
        Ok((found, refused))
    }

    /// Read the documents behind cosign's `sha256-<hex>.sbom` sidecar tag —
    /// **every** layer, not the first.
    ///
    /// An empty `Vec` covers both "the subject carries no such tag" — the
    /// overwhelmingly common case, and a 404 says exactly that — and a `--type`
    /// narrowing that matched nothing.
    ///
    /// The error carries the manifest digest when one was learned, because the
    /// caller has no descriptor to name the refusal with: this door is addressed
    /// by tag, and the digest only exists once the registry has answered.
    ///
    /// # Why every layer, when cosign writes one
    ///
    /// Measured against cosign v3.1.1, a second `cosign attach sbom` against the
    /// same subject *replaces* the tag's manifest, so cosign itself never writes
    /// a second layer. That is a fact about one producer, and this reader does
    /// not get to assume its producer: the tag is generic OCI, addressed by
    /// name, and a registry can serve any manifest under it. Reading
    /// `layers.first()` therefore did not mean "cosign wrote one document" — it
    /// meant every document past the first was **silently dropped** from
    /// `ocx package sbom`, which is a collect-all report (#386).
    ///
    /// One layer's refusal refuses the whole tag, and deliberately: the answer
    /// this feeds is "every document the subject carries", and a short list
    /// presented as complete is the worse failure. That is also exactly what the
    /// first-layer reader did when the first layer was the bad one.
    ///
    /// The tag spends one candidate slot however many layers it holds — it is
    /// one manifest fetch, and the slot cap bounds discovery breadth — while
    /// every layer's bytes are charged to the byte budget by
    /// [`Self::read_unverified_layer`], because layers are transfer.
    async fn read_sbom_sidecar_tag(
        transport: &dyn OciTransport,
        ctx: &VerifyContext<'_>,
        budget: &mut ScanBudget,
        target: &ScanTarget,
    ) -> Result<Vec<UnverifiedSbom>, (Option<Digest>, VerifyErrorKind)> {
        let ScanTarget {
            image, subject_digest, ..
        } = target;
        let Some((manifest_bytes, referrer_digest)) = pull_sbom_sidecar_manifest(transport, image, subject_digest)
            .await
            .map_err(|kind| (None, kind))?
        else {
            return Ok(Vec::new());
        };
        budget.charge(manifest_bytes.len() as u64);

        let read = async {
            // A plain image manifest, never a `ReferrerManifest`: cosign's
            // `.sbom` manifest declares neither `artifactType` nor `subject`, so
            // the referrer type's required fields would reject cosign's own
            // bytes outright.
            let manifest: ImageManifest =
                serde_json::from_slice(&manifest_bytes).map_err(|_| VerifyErrorKind::BundleParseFailed)?;
            // A zero-layer manifest is unusable, and stays the same refusal it
            // was when the reader took the first layer: the tag exists and holds
            // nothing readable, which is not the same answer as "no tag".
            if manifest.layers.is_empty() {
                return Err(VerifyErrorKind::NoUsableBundle);
            }
            // `Raw`: the layer *is* the document. A `.sbom` tag never carries a
            // Sigstore bundle — cosign's signed path writes a referrer, not this
            // tag — and passing `Bundle` here would ask `parse_bundle` to read an
            // SBOM.
            Self::read_every_layer(
                transport,
                ctx,
                budget,
                target,
                &referrer_digest,
                &manifest.layers,
                UnverifiedPayload::Raw,
            )
            .await
        };
        read.await.map_err(|kind| (Some(referrer_digest), kind))
    }

    /// Fetch one referrer and turn it into an unverified document: its
    /// manifest, then its payload blob, then whatever decode its shape calls
    /// for.
    ///
    /// The caps are the signed path's, reached through the same two helpers, so
    /// a document read here is bounded by exactly the numbers an attestation
    /// envelope is. Bytes are charged on the failure paths too — a rejected
    /// read still cost up to the cap, because the bounded read stops at cap + 1
    /// rather than at zero. That charge is deliberately pessimistic and cannot
    /// be tightened: `pull_blob_capped` drops the buffer on the error
    /// path, and a registry declaring one byte while streaming the cap would
    /// otherwise be charged one byte.
    ///
    /// **Every** layer, for the reason [`Self::read_sbom_sidecar_tag`] reads
    /// every layer of the sidecar tag: an OCI 1.1 referrer manifest is an
    /// ordinary image manifest, whose `layers` array the spec bounds below at
    /// one and not above, so "the payload is the first layer" was a statement
    /// about OCX's own writer and not about the bytes a registry serves. Both
    /// doors funnel into the same [`Self::read_unverified_layer`], so a reader
    /// that fixed one and not the other would judge one shape two ways.
    ///
    /// An empty `Vec` is a `--type` narrowing miss, not a failure — see the
    /// caller. One candidate slot per referrer however many layers it holds;
    /// every layer's bytes are charged.
    async fn read_unverified_referrer(
        transport: &dyn OciTransport,
        ctx: &VerifyContext<'_>,
        budget: &mut ScanBudget,
        target: &ScanTarget,
        descriptor: &crate::oci::Descriptor,
        payload: UnverifiedPayload,
    ) -> Result<Vec<UnverifiedSbom>, VerifyErrorKind> {
        let referrer_digest =
            Digest::try_from(descriptor.digest.as_str()).map_err(|e| VerifyErrorKind::Internal(Box::new(e)))?;

        // Cheap reject of a self-declared over-cap descriptor before any fetch;
        // the actual body length is re-checked after the read, since a registry
        // can lie about the declared size.
        if descriptor.size < 0 || descriptor.size as u64 > MAX_REFERRER_MANIFEST_BYTES {
            return Err(VerifyErrorKind::BundleParseFailed);
        }
        // `clone_with_digest` drops the tag, so this stays digest-only — a
        // `repo:tag@digest` reference keys a different registry path and 404s.
        let referrer_ref = target.image.clone_with_digest(descriptor.digest.clone());
        let referrer_bytes = match pull_referrer_manifest_capped(transport, &referrer_ref).await {
            Ok(bytes) => bytes,
            Err(kind) => {
                budget.charge(MAX_REFERRER_MANIFEST_BYTES);
                return Err(kind);
            }
        };
        budget.charge(referrer_bytes.len() as u64);

        let manifest: crate::oci::referrer::ReferrerManifest =
            serde_json::from_slice(&referrer_bytes).map_err(|_| VerifyErrorKind::BundleParseFailed)?;
        if manifest.layers.is_empty() {
            return Err(VerifyErrorKind::NoUsableBundle);
        }
        Self::read_every_layer(
            transport,
            ctx,
            budget,
            target,
            &referrer_digest,
            &manifest.layers,
            payload,
        )
        .await
    }

    /// Read each of one manifest's payload layers into a document, dropping the
    /// ones `--type` narrows out.
    ///
    /// The shared body of the two multi-layer readers above. One layer's
    /// refusal refuses the manifest: the pass this feeds reports *every*
    /// document a subject carries, so a partial list returned as a complete one
    /// is the failure worth avoiding.
    async fn read_every_layer(
        transport: &dyn OciTransport,
        ctx: &VerifyContext<'_>,
        budget: &mut ScanBudget,
        target: &ScanTarget,
        referrer_digest: &Digest,
        layers: &[crate::oci::Descriptor],
        payload: UnverifiedPayload,
    ) -> Result<Vec<UnverifiedSbom>, VerifyErrorKind> {
        let mut documents = Vec::with_capacity(layers.len());
        for layer in layers {
            if let Some(document) =
                Self::read_unverified_layer(transport, ctx, budget, target, referrer_digest.clone(), layer, payload)
                    .await?
            {
                documents.push(document);
            }
        }
        Ok(documents)
    }

    /// Turn one already-located payload layer into an unverified document.
    ///
    /// Split out of [`Self::read_unverified_referrer`] rather than duplicated,
    /// because the `.sbom` sidecar tag reaches layers of the same *kind*
    /// through a different door: an OCI 1.1 referrer is addressed by digest and
    /// parses as a [`ReferrerManifest`](crate::oci::referrer::ReferrerManifest),
    /// while cosign's `sha256-<hex>.sbom` manifest declares neither
    /// `artifactType` nor `subject` and so parses only as a plain image
    /// manifest. Both doors walk **all** of their manifest's layers through
    /// [`Self::read_every_layer`], and everything after "here is the layer" —
    /// the media-type gate, the `--type` narrowing, the size cap and the blob
    /// read — is this one function, so the two cannot drift into judging the
    /// same bytes differently.
    ///
    /// `Ok(None)` is a `--type` narrowing miss, not a failure.
    async fn read_unverified_layer(
        transport: &dyn OciTransport,
        ctx: &VerifyContext<'_>,
        budget: &mut ScanBudget,
        target: &ScanTarget,
        referrer_digest: Digest,
        layer: &crate::oci::Descriptor,
        payload: UnverifiedPayload,
    ) -> Result<Option<UnverifiedSbom>, VerifyErrorKind> {
        let caps = budget.caps;
        // For a raw attachment the layer's media type is *the* claim about what
        // the document is, and the only one checkable without a key. It is
        // therefore both the gate and the label: outside the SBOM set the
        // referrer is refused, since otherwise it could declare
        // `application/vnd.cyclonedx+json` in the listing, carry an executable,
        // and be presented as an SBOM. Inside the set it names the
        // predicateType reported for the bytes that were actually served —
        // never the listing's echo, which nothing checks against this manifest,
        // and which a registry can therefore use to label SPDX bytes CycloneDX.
        // A bundle needs no such gate: its predicateType is inside the payload
        // and the parse below fail-closes on anything that is not a bundle.
        let raw_predicate_type = match payload {
            UnverifiedPayload::Raw => match sbom_predicate_type_uri(&layer.media_type) {
                Some(uri) => Some(uri),
                None => {
                    return Err(VerifyErrorKind::SbomMediaTypeUnsupported {
                        media_type: layer.media_type.clone(),
                    });
                }
            },
            UnverifiedPayload::Bundle => None,
        };
        // `--type` narrows on the value that will be *reported*, which is the
        // layer's. Applied here rather than after the blob pull only because
        // the answer is already known: a narrowed-out raw referrer costs one
        // manifest request and no payload bytes. A bundle is narrowed after its
        // parse, in `extract_bundle_payload`, for the same reason in reverse —
        // its predicateType is not knowable until then.
        if let Some(uri) = raw_predicate_type
            && let VerifyContentMode::Attestation {
                predicate_type: Some(requested),
            } = &ctx.content
            && requested.uri() != uri
        {
            return Ok(None);
        }
        if layer.size < 0 || layer.size as u64 > caps.bundle_bytes as u64 {
            return Err(VerifyErrorKind::AttestationTooLarge {
                limit: caps.bundle_bytes as u64,
                actual: layer.size.max(0) as u64,
            });
        }
        let blob_digest = Digest::try_from(layer.digest.as_str()).map_err(|_| VerifyErrorKind::BundleParseFailed)?;
        let bytes = match pull_blob_capped(transport, &target.image, &blob_digest, caps.bundle_bytes).await {
            Ok(bytes) => {
                budget.charge(bytes.len() as u64);
                bytes
            }
            Err(kind) => {
                budget.charge(caps.bundle_bytes as u64);
                return Err(kind);
            }
        };

        let (predicate_type, document) = match raw_predicate_type {
            // The registry served these bytes under this media type. That is
            // the whole of it, which is why the caller labels the result
            // unverified.
            Some(uri) => (uri.to_owned(), bytes),
            None => match Self::extract_bundle_payload(ctx, bytes, target).await? {
                Some(extracted) => extracted,
                None => return Ok(None),
            },
        };

        Ok(Some(UnverifiedSbom {
            referrer_digest,
            subject_digest: target.subject_digest.clone(),
            predicate_type,
            document,
        }))
    }

    /// Read a Sigstore bundle's DSSE payload with **nothing verified**: the
    /// predicateType and the predicate document, as the envelope states them.
    ///
    /// Deliberately the existing structural parse chain and not a second one:
    /// `parse_bundle` then [`dsse::verify_envelope`], the same two calls the
    /// signed path makes before it verifies anything. `verify_envelope`'s name
    /// says verify, but its own doc calls it "the structural half: everything
    /// provable without a verifying key" — it consults no certificate, no key
    /// and no log. Reusing it is what keeps the caps custody, the `_type`
    /// allowlist and the subject binding identical across the two modes; a
    /// separate reader would be a second parser to keep in step.
    ///
    /// What this must never do is carry signer identity out. The bundle has a
    /// certificate in it and it would be one line to read a SAN off it, and
    /// that line would put an unverified string in the column an operator reads
    /// as provenance. [`UnverifiedSbom`] has nowhere to put it, which is the
    /// enforcement.
    ///
    /// `Ok(None)` is a `--type` narrowing miss.
    async fn extract_bundle_payload(
        ctx: &VerifyContext<'_>,
        bundle_bytes: Vec<u8>,
        target: &ScanTarget,
    ) -> Result<Option<(String, Vec<u8>)>, VerifyErrorKind> {
        let VerifyContentMode::Attestation { predicate_type } = &ctx.content else {
            return Ok(None);
        };
        // Off the runtime worker for the same reason the signed path's parse
        // is: serde passes over up to `MAX_ATTESTATION_ENVELOPE_BYTES` (32 MiB)
        // of registry-supplied JSON with no await between them (ASYNC-01).
        let cap = ctx.content.caps().bundle_bytes;
        let subject_digest = target.subject_digest.clone();
        let requested = predicate_type.clone();
        tokio::task::spawn_blocking(move || {
            let bundle = parse_bundle(&bundle_bytes, cap).ok_or(VerifyErrorKind::BundleParseFailed)?;
            match dsse::verify_envelope(&bundle, &subject_digest, requested.as_ref()) {
                Ok(verified) => Ok(Some((
                    verified.attestation.predicate_type,
                    verified.attestation.predicate.get().as_bytes().to_vec(),
                ))),
                // Only reachable when a type was requested; the candidate is
                // sound, it is simply not the document that was asked for.
                Err(VerifyErrorKind::PredicateTypeMismatch { .. }) if requested.is_some() => Ok(None),
                Err(kind) => Err(kind),
            }
        })
        .await
        .map_err(|error| {
            tracing::warn!("unverified bundle read task panicked: {error}");
            VerifyErrorKind::Internal(Box::new(error))
        })?
    }

    /// List the raw SBOM attachments on the target and refuse every one,
    /// **without fetching any of them**.
    ///
    /// Under [`VerificationMode::Demand`] a raw attachment can never be an
    /// answer, so there is nothing to learn from its bytes — and not fetching
    /// is what makes untrusted volume unable to spend the budget the signed
    /// pass needs. The rows are capped at the mode's candidate count for the
    /// same reason: a registry listing ten thousand attachments must not be
    /// able to turn a refusal into ten thousand report rows.
    async fn refuse_unsigned(
        client: &Client,
        ctx: &VerifyContext<'_>,
        target: &ScanTarget,
    ) -> Result<(Vec<RefusedCandidate>, Option<VerifyErrorKind>), VerifyErrorKind> {
        let VerifyContentMode::Attestation { .. } = &ctx.content else {
            return Ok((Vec::new(), None));
        };
        let transport = client.transport();
        let ScanTarget {
            image, subject_digest, ..
        } = target;

        // The Unsupported verdict no longer refuses the operation: the OCI referrers
        // tag-schema fallback (`list_referrers_with_fallback` /
        // `append_referrer_fallback_index`) serves a registry without the Referrers
        // API. See `adr_oci_referrers_signing_v1.md`, Amendment 10 — the fallback
        // index is a mutable tag anyone with push access authors, and the residual
        // attack surface that reverses S1-F is recorded there.
        let ReferrersListing {
            descriptors: listed,
            via,
        } = transport
            .list_referrers_with_fallback(image, subject_digest, None)
            .await
            .map_err(map_client_error)?;
        // D-5 reports this on signatures[].discovery_method. Every row this pass
        // emits is a refusal, so the discovery method is diagnostic here rather
        // than reported — but it is still the difference between a
        // registry-computed listing and a tag anyone with push access authored.
        tracing::debug!("unsigned SBOM attachments discovered via {via}");

        let mut digests: Vec<String> = listed
            .into_iter()
            .filter(|descriptor| {
                descriptor
                    .artifact_type
                    .as_deref()
                    .is_some_and(is_unsigned_sbom_artifact_type)
            })
            .map(|descriptor| descriptor.digest)
            .collect();
        // The `.sbom` sidecar tag is refused by the same rule, and it is the one
        // place this pass reads a manifest at all.
        //
        // **The cost is unconditional**, not a rare path: this pass runs before
        // the signed scan on every target of every `Demand` run, so a fully
        // signed subject pays one extra manifest GET per target for a tag it
        // almost never has. That is the price of the door being tag-addressed —
        // a listing cannot report a tag, so the only way to name one is to ask
        // for it. Deferring it until the signed scan came back empty would make
        // it free on the common path and inconsistent with the rows above, which
        // are reported *beside* a verified attestation; an unsigned referrer
        // surfacing while an unsigned sidecar stayed hidden is the worse
        // outcome, so the request stays.
        //
        // Reading a manifest here does not reopen what this pass exists to
        // close. The rule is that *untrusted volume* must not spend the signed
        // pass's budget, and this door is one tag rather than a listing — a
        // registry cannot multiply it, which is exactly what it can do to the
        // referrers the loop above deliberately refuses without fetching.
        //
        // Only the digest is kept: the manifest's layers say nothing a `Demand`
        // run is allowed to act on.
        //
        // Where that digest ends up is the published contract, and it is not
        // "always": it reaches `refused[].referrer_digest` when the subject
        // *also* carries something that verified, and is collapsed away by
        // `best_failure` — which maps a candidate to its `reason` alone — when
        // the refusal is instead promoted to the run's own error. Documented in
        // `command-line.md`'s `package sbom` exit-code table, on the
        // `unsigned_rejected_by_policy` row — cited by row rather than by line
        // because that file is edited from several directions at once and a
        // stale line number still resolves, to the wrong thing. That is the
        // identical path an unsigned
        // *referrer* takes, through this same `digests` vec, and the parity is
        // the point: a tag-addressed refusal and a listing-addressed one are
        // reported the same way or the sidecar becomes a special case.
        //
        // A fault on this one probe is **deferred, not propagated**. This pass
        // can only ever add a refusal — nothing it returns can make a run
        // succeed — and it runs unconditionally, before the signed scan, on
        // every target of every `Demand` run. Propagating would let a transient
        // fault on a tag the subject almost never has fail a `--verify` that
        // the signed pass was about to pass. It is not dropped either: the
        // caller spends it if nothing verified, where "I could not finish
        // looking" is the honest answer and must not read as "nothing is
        // attached" — the same rule the `.sig` door in [`Self::scan`] states,
        // applied at the one point where that door's "we only reach here with
        // nothing verified" premise does not hold.
        let mut sidecar_fault = None;
        match pull_sbom_sidecar_manifest(transport, image, subject_digest).await {
            Ok(Some((_, referrer_digest))) => digests.push(referrer_digest.to_string()),
            Ok(None) => {}
            Err(kind) => sidecar_fault = Some(kind),
        }
        // Digest order, for the reason `order_candidates` sorts: a total order
        // the registry does not choose, so the report is reproducible.
        digests.sort();
        Ok((
            digests
                .into_iter()
                .take(ctx.content.caps().candidates)
                .map(|referrer_digest| RefusedCandidate {
                    referrer_digest,
                    reason: VerifyErrorKind::UnsignedRejectedByPolicy,
                })
                .collect(),
            sidecar_fault,
        ))
    }

    async fn run_inner(client: &Client, ctx: VerifyContext<'_>) -> Result<Vec<VerifyResult>, VerifyErrorKind> {
        let target = Self::resolve_target(client, &ctx).await?;
        let mut budget = ScanBudget::new(ctx.content.caps());
        // Q3: arity is the caller's, not the mode's. `FirstMatch` returns the
        // moment a candidate passes; `All` keeps going so `signatures[]` can
        // list what else the subject carries. Either way the head of `matches`
        // is the first candidate that fully passed, which is the verdict.
        let arity = if ctx.report_all {
            ScanArity::All
        } else {
            ScanArity::FirstMatch
        };
        let found = Self::scan_with_index_fallback(client, &ctx, &target, arity, &mut budget).await?;
        let results: Vec<VerifyResult> = found.matches.into_iter().map(|(verify, _)| verify).collect();
        if results.is_empty() {
            return Err(VerifyErrorKind::NoSignaturesFound);
        }
        Ok(results)
    }

    /// Scan the pinned subject, and — only when C-008's membership proof holds
    /// — the enclosing index behind it.
    ///
    /// A subject signed the cosign way carries its signature on the *index*:
    /// `cosign verify <tag>` resolves a multi-platform tag to the index digest
    /// and signs there, while OCX pins a platform manifest. So a second look,
    /// addressed at the index, is what makes such a signature reachable at all.
    ///
    /// Its own function rather than an arm inside [`Self::run_inner`] so the
    /// **gate** is testable: this is the one place that decides whether an
    /// index's signatures may count for a child, and a caller that read
    /// `enclosing_index` directly would have no check standing between it and
    /// "assume membership".
    async fn scan_with_index_fallback(
        client: &Client,
        ctx: &VerifyContext<'_>,
        target: &ScanTarget,
        arity: ScanArity,
        budget: &mut ScanBudget,
    ) -> Result<ScanOutcome, VerifyErrorKind> {
        let subject_failure = match Self::scan(client, ctx, target, arity, budget).await {
            Ok(found) => return Ok(found),
            Err(kind) => kind,
        };
        // `index_signature_subject` is the whole gate: no enclosing index, or a
        // subject the index does not list, and the fall-through never runs, so
        // the index's signatures are not considered at all. "Cannot prove
        // membership" is never "assume membership".
        let Some(index_digest) = target.index_signature_subject() else {
            return Err(subject_failure);
        };
        let index_target = ScanTarget {
            image: target.image.clone(),
            subject_digest: index_digest.clone(),
            // The index is the subject of this pass. No further indirection: an
            // index listing an index would need its own membership proof, which
            // nothing here has.
            enclosing_index: None,
            index_members: Vec::new(),
        };
        // One budget across both passes, the same way an attestation run spends
        // one set of bounds over its two: a registry cannot buy a second
        // allowance by stuffing the platform manifest with candidates, and
        // exhausting the budget refuses rather than admits.
        //
        // The platform manifest's own verdict is the more actionable one when
        // the index carries nothing either — it is the object the user named.
        Self::scan(client, ctx, &index_target, arity, budget)
            .await
            .map_err(|_| subject_failure)
    }

    /// Resolve the target once: the SSRF floor on the trust services, the
    /// per-platform subject digest, and the registry reference every
    /// referrer-facing call is addressed with.
    ///
    /// Extracted from [`Self::scan`] because an attestation run makes two
    /// passes over the same subject — signed referrers, then unsigned ones —
    /// and resolving twice would repeat an index select, a physical rewrite and
    /// a dial-time DNS lookup to re-derive an answer that cannot have changed.
    async fn resolve_target(client: &Client, ctx: &VerifyContext<'_>) -> Result<ScanTarget, VerifyErrorKind> {
        // 0. SSRF floor for the trust services (CWE-918). The CLI boundary
        //    validated the URL as a *string*; this is where we find out where it
        //    actually resolves, before anything dials it. Skipped under
        //    `--offline`, which reaches no trust service at all -- resolving
        //    there would make an air-gapped verify depend on DNS.
        if !ctx.offline {
            let trusted = ctx.index.trusted_hosts_for(ctx.identifier.registry());
            crate::oci::endpoint::resolve_sigstore_url(ctx.rekor_url, trusted)
                .await
                .map_err(|error| VerifyErrorKind::InvalidEndpointUrl {
                    endpoint: "--rekor-url".into(),
                    reason: crate::oci::endpoint::UrlRejection::from(error),
                })?;
        }

        // 1. Resolve the reference's manifest **through the index chain**, not
        //    through the registry transport. The transport path would bypass
        //    `guard_local_physical` and the mirror map, and would break
        //    `--offline` — an installed package resolves from the local index
        //    with no network at all.
        //
        //    One fetch answers three questions at once: which digest the
        //    reference names, whether that object is an index, and — when it is
        //    — which children it lists. The children are what C-008's
        //    membership test reads; before this they were fetched inside
        //    `Index::select` and thrown away, which is why an index signature
        //    could not be attributed to a pinned platform manifest at all.
        let Some((resolved_digest, manifest)) = ctx
            .index
            .fetch_manifest(ctx.identifier, IndexOperation::Resolve)
            .await
            .map_err(|e| VerifyErrorKind::Internal(Box::new(e)))?
        else {
            return Err(VerifyErrorKind::TargetNotFound {
                platform: platform_label(ctx.platform),
            });
        };
        // Every digest the index lists — attestation and referrer entries
        // included, not only the platform candidates. The index digest binds
        // each descriptor it carries, so a signature over the index covers each
        // of them; narrowing this list to `--platform` candidates would refuse
        // membership for a child the index demonstrably lists.
        let index_members: Vec<Digest> = match &manifest {
            crate::oci::Manifest::ImageIndex(index) => index
                .manifests
                .iter()
                .filter_map(|entry| Digest::try_from(entry.digest.clone()).ok())
                .collect(),
            crate::oci::Manifest::Image(_) => Vec::new(),
        };
        // `None` for a bare image manifest — the resolution reached the
        // acted-on object directly and there is no index to narrow into.
        let children: Option<Vec<(Platform, Digest)>> = match &manifest {
            crate::oci::Manifest::ImageIndex(index) => Some(
                index
                    .manifests
                    .iter()
                    .filter_map(|entry| {
                        // The one shared eligibility rule, same as
                        // `Index::fetch_candidates`: an entry naming no platform
                        // (or one OCX cannot represent) is a referrer entry, not
                        // something `--platform` can ever mean.
                        let platform = Platform::candidate_from_descriptor(entry)?;
                        Some((platform, Digest::try_from(entry.digest.clone()).ok()?))
                    })
                    .collect(),
            ),
            crate::oci::Manifest::Image(_) => None,
        };
        // 2. C-010's `--platform` optionality rule, from the one module that
        //    owns it — shared with sign and attest so the three cannot diverge.
        //    It *selects* a child and reports the index it was reached through;
        //    it makes no validity decision and tests no membership. That test is
        //    below, and is this pipeline's.
        let SignTarget {
            subject_digest,
            enclosing_index,
        } = resolve_sign_target(&resolved_digest, children.as_deref(), ctx.platform)
            .map_err(map_resolve_target_error)?;
        let resolved = ctx.identifier.clone_with_digest(subject_digest.clone());
        // Index indirection: a logical name (`ocx.sh/<ns>/<pkg>`) may point at a
        // different physical registry, so every transport-facing call below —
        // capability probe, referrer listing, referrer manifest + bundle blob
        // pulls — targets the physical address. `Ok(None)` = no rewrite, same
        // contract the pull path's `resolve_transport_pinned` reads. Trust
        // policy scope matching stays on the LOGICAL identifier (`ctx.policies`
        // are resolved from it by the caller): only registry traffic moves. The
        // SSRF floor on the returned host is enforced upstream in the shared
        // index choke point (`ChainedIndex::guard_local_physical`).
        let physical = ctx
            .index
            .physical_reference(&resolved)
            .await
            .map_err(|e| VerifyErrorKind::Internal(Box::new(e)))?
            .unwrap_or_else(|| resolved.clone());
        // The pre-flight above had to tolerate a DNS lookup failure -- it runs on
        // every resolve, including ones that never fetch, so it cannot fail on a
        // missing resolver. Its own contract says the tolerance is safe only
        // because the dial site re-validates fail-closed, which is here: a
        // request is now imminent, and the shared client carries no SSRF
        // resolver of its own. Without this the sign/verify paths held only the
        // tolerant half of that split (CWE-918). Same call the pull path makes.
        ctx.index
            .guard_physical_dial(&resolved, &physical)
            .await
            .map_err(|error| VerifyErrorKind::ForbiddenRegistryTarget {
                reason: error.to_string(),
            })?;
        // The mirror map applies on top: `transport_reference` is the read seam
        // every registry-facing reference must come from (T-arch-G1), so a
        // `[mirrors]` entry for the physical host redirects this traffic too.
        let image = client.transport_reference(&physical);
        Ok(ScanTarget {
            image,
            subject_digest,
            enclosing_index,
            index_members,
        })
    }

    /// The scan both entry points share: list the target's signature-bundle
    /// referrer candidates and verify them under the requested content mode.
    ///
    /// `arity` is passed rather than derived from `ctx.content` so the two facts
    /// stay separable — "which content kind" and "how many answers" are
    /// different questions, and a test can vary either alone. `budget` is the
    /// caller's so an attestation run's two passes spend one set of bounds
    /// between them rather than one set each.
    ///
    async fn scan(
        client: &Client,
        ctx: &VerifyContext<'_>,
        target: &ScanTarget,
        arity: ScanArity,
        budget: &mut ScanBudget,
    ) -> Result<ScanOutcome, VerifyErrorKind> {
        let transport = client.transport();
        let ScanTarget {
            image, subject_digest, ..
        } = target;

        // C-007 / D9. The pin decides what is **discovered**, never what is
        // ignored after discovery: a pinned `simplesigning` against a subject
        // carrying only a bundle must answer 79, and the only way to keep that
        // true by construction is to never build the bundle candidate.
        //
        // Simplesigning is signature-shaped only. A sidecar layer carries a
        // `SimpleSigningClaim`, not a DSSE statement, so it can never satisfy an
        // attestation run's `AttestationMatch`. `.att` is the mirror image and
        // gets its own gate below; `.sbom` is neither, and takes no gate here at
        // all — its layer is an SBOM document, so it can only ever satisfy the
        // permissive listing pass, where `scan_unverified` reads it. No
        // `--signature-format` pin reaches that pass, because nothing on it is a
        // signature to pin the format of.
        let discover_bundles = ctx.signature_format != Some(SignatureFormat::Simplesigning);
        let discover_simplesigning = ctx.signature_format != Some(SignatureFormat::Bundle)
            && matches!(ctx.content, VerifyContentMode::Signature);
        // Spec §WP5's `.att` half, and the mirror image of the line above: an
        // `.att` layer is a DSSE envelope, so it can only ever satisfy an
        // *attestation* run, exactly as a simplesigning claim can only ever
        // satisfy a signature one. Gated on the same `--signature-format` pin,
        // which means the same thing on both: `bundle` is the modern shape only.
        //
        // It is discovered by TAG and by nothing else. Measured against cosign
        // v3.1.1: `attest` writes a `SIGSTORE_BUNDLE_V03` referrer (already a
        // candidate above), the registry-without-referrers fallback writes a
        // `sha256-<hex>` index of the same, and the `.att` manifest itself
        // carries neither `artifactType` nor `subject` — so no listing can
        // reach it and there is no cosign attestation artifact type to filter
        // on. See `super::attestation_sidecar`'s module doc.
        let discover_attestation_sidecar = ctx.signature_format != Some(SignatureFormat::Bundle)
            && matches!(ctx.content, VerifyContentMode::Attestation { .. });

        // 2. List signature referrers (the Referrers API, or the fallback tag
        //    when the registry has none), then re-filter client-side into the
        //    two shapes — the OCI spec permits a registry to ignore the
        //    server-side artifactType filter, so the client-side pass is the
        //    real one either way.
        //
        //    The server-side hint is kept only while the bundle shape is the one
        //    thing being looked for; once a simplesigning referrer is also a
        //    candidate, one unfiltered listing beats one request per artifact
        //    type (the same reasoning `scan_unverified` states).
        //
        //    The bundle re-filter drops only referrers that declare a
        //    *different* explicit artifactType. A referrer with no artifactType
        //    (absent in the listing, or a transport that does not echo it) is
        //    kept: the bundle parse downstream fail-closes on a non-bundle, so
        //    tolerating an absent type here cannot admit a forged signature —
        //    but rejecting it would drop a genuine server-matched referrer
        //    (regression class: a registry that matched server-side but omits
        //    the per-descriptor artifactType echo).
        let server_filter = (!discover_simplesigning).then_some(SIGSTORE_BUNDLE_V03);
        let ReferrersListing {
            descriptors: referrers,
            via,
        } = Self::list_signature_referrers(transport, image, subject_digest, server_filter).await?;
        // Reported on `signatures[].discovery_method`. Carried out of the
        // listing rather than dropped at it: a candidate reached through the
        // mutable fallback tag and one the registry itself computed are not the
        // same provenance claim, and the report says which.
        tracing::debug!("signature referrers discovered via {via}");
        let mut candidates: Vec<crate::oci::Descriptor> = Vec::new();
        let mut sidecar_referrers: Vec<crate::oci::Descriptor> = Vec::new();
        for descriptor in referrers {
            let artifact_type = descriptor.artifact_type.as_deref();
            let bundle_shaped = artifact_type.is_none_or(|declared| declared == SIGSTORE_BUNDLE_V03);
            let sidecar_shaped = artifact_type
                .is_some_and(|declared| declared == COSIGN_SIG_ARTIFACT_TYPE || declared == COSIGN_SBOM_ARTIFACT_TYPE);
            if discover_bundles && bundle_shaped {
                candidates.push(descriptor);
            } else if discover_simplesigning && sidecar_shaped {
                sidecar_referrers.push(descriptor);
            }
        }
        if candidates.is_empty()
            && sidecar_referrers.is_empty()
            && !discover_simplesigning
            && !discover_attestation_sidecar
        {
            // Before the trust-root gate below: a subject with no bundle
            // referrer at all is "not signed", and a missing trust root is not
            // the thing to report about it. The caller promotes any refusal it
            // recorded over this kind. Only reachable once the sidecar-tag door
            // is closed too — otherwise there is still somewhere left to look.
            return Err(VerifyErrorKind::NoSignaturesFound);
        }
        order_candidates(&mut candidates, &ctx.content);

        // Refused up front rather than at the first signature check: a keyless
        // trust root is a configuration mistake with a fixed remedy, and it
        // would otherwise surface as an opaque SCT failure per candidate.
        // `sigstore` builds an empty CT keyring without complaint.
        //
        // Two conditions, both narrowing and neither weakening.
        //
        // *There is a bundle candidate.* A missing trust root is not the thing
        // to report about a subject nothing was found for — "not signed" is —
        // and with only the sidecar-tag door left there is nothing yet known to
        // verify against.
        //
        // *A keyless signature could satisfy this run.* The CT log key is
        // evidence for the keyless path alone: `cosign verify --key cosign.pub`
        // requires no trust root at all, so a run whose applicable policies are
        // all `PolicyBackend::Key` cannot want this remedy and must not be
        // refused by it. Nothing is relaxed for keyless — a keyless candidate
        // under an empty CT keyring is still refused, by `sigstore`'s own SCT
        // check inside `verifier.verify`. This gate only decides whether that
        // refusal is reported once, up front, with the fix in it.
        let keyless_reachable = ctx.policies.is_empty()
            || ctx.policies.iter().any(|policy| {
                policy
                    .backends
                    .iter()
                    .any(|backend| matches!(backend, PolicyBackend::Keyless(_)))
            });
        if !candidates.is_empty() && keyless_reachable && ctx.trust_root.ctfe_key_map().is_empty() {
            return Err(VerifyErrorKind::TrustRootLoad(TrustRootLoadReason::NoCtLogKey));
        }
        // Built once per run: compiling the trust root into a certificate pool
        // and a CT keyring is per-material work, not per-candidate. The Rekor
        // configuration is inert -- `sigstore` 0.14 never dials it, and ocx does
        // its own Rekor work in `tlog` -- so the default opens no connection.
        let verifier = Verifier::new(RekorConfiguration::default(), ctx.trust_root.clone()).map_err(|e| {
            VerifyErrorKind::TrustRootLoad(TrustRootLoadReason::AssetReadFailed {
                source: Box::new(std::io::Error::other(format!("trust root unusable: {e}"))),
            })
        })?;

        // 3. Verify each candidate independently. `FirstMatch` is the ANY-of
        //    signature scan — the first candidate that fully passes crypto +
        //    identity/policy wins, which fixes key rotation (a valid later
        //    signature is no longer masked by an earlier one) and the
        //    malformed-first-referrer DoS. `All` is the attestation scan, where
        //    letting the registry's listing order pick one of several verified
        //    documents would be the defect. Both are bounded by the mode's
        //    candidate count, its total-bytes budget, and the listing backstop.
        //    After all fail, return the most actionable error deterministically —
        //    a fail-closed availability outcome, not forgery.
        // Captured before `candidates` and `sidecar_referrers` are consumed
        // below; read by the `.att` door's empty-scan verdict at the end.
        let anything_listed = !candidates.is_empty() || !sidecar_referrers.is_empty();
        let mut total_candidates = candidates.len();
        // Every refusal is kept, not folded into one "best" as it arrives: the
        // aggregate error is derived from this at the end (`best_failure`), and
        // a scan that *does* find matches still carries the refusals out so the
        // caller can report them.
        let mut refused: Vec<RefusedCandidate> = Vec::new();
        let mut matches: Vec<(VerifyResult, Option<VerifiedAttestation>)> = Vec::new();
        // One memo for every door this scan opens (#374, #319). Born here
        // because "per run" is what bounds it: a longer-lived cache would
        // outlive the trust root its answers were resolved against.
        let rekor_keys = RekorKeyMemo::default();
        // D6's dedup set. A `Vec` rather than a hash set on purpose:
        // `MAX_SIGNATURE_CANDIDATES` bounds it at single digits, so the linear
        // scan is cheaper than hashing signature bytes.
        // ponytail: O(n^2) over <= 16 candidates; switch to a set if the cap grows.
        let mut seen: Vec<SignatureKey> = Vec::new();
        // Resolved from the requested mode, before the first fetch (D-d).
        let caps = budget.caps;
        // The signed artifact's bytes, not just its digest: `Verifier` hashes a
        // preimage. Fetched once per run, after the referrer listing so an
        // unsigned artifact costs no extra request — and skipped entirely when
        // no bundle candidate exists, since the sidecar path signs a payload
        // blob rather than the subject manifest.
        let subject_bytes = if candidates.is_empty() {
            Vec::new()
        } else {
            pull_subject_manifest_verified(transport, image, subject_digest).await?
        };
        for descriptor in candidates {
            if !budget.may_examine() {
                break;
            }
            // Cheap reject of a self-declared over-cap descriptor before any
            // fetch. The actual body length is re-checked after the read, since
            // the declared size is untrusted (a registry can lie about it).
            if descriptor.size < 0 || descriptor.size as u64 > MAX_REFERRER_MANIFEST_BYTES {
                budget.examined();
                refused.push(RefusedCandidate {
                    referrer_digest: descriptor.digest.clone(),
                    reason: VerifyErrorKind::BundleParseFailed,
                });
                continue;
            }
            // `clone_with_digest` drops the tag, so this stays digest-only — a
            // `repo:tag@digest` reference keys a different registry path and 404s.
            let referrer_ref = image.clone_with_digest(descriptor.digest.clone());
            let referrer_bytes = match pull_referrer_manifest_capped(transport, &referrer_ref).await {
                Ok(bytes) => bytes,
                Err(kind) => {
                    // An over-cap read still cost up to the per-manifest cap; charge it.
                    budget.charge(MAX_REFERRER_MANIFEST_BYTES);
                    budget.examined();
                    refused.push(RefusedCandidate {
                        referrer_digest: descriptor.digest.clone(),
                        reason: kind,
                    });
                    continue;
                }
            };
            budget.charge(referrer_bytes.len() as u64);
            match Self::verify_one_referrer(
                transport,
                ctx,
                &verifier,
                &descriptor,
                referrer_bytes,
                subject_digest,
                &subject_bytes,
                image,
                via,
                budget,
                &rekor_keys,
            )
            .await
            {
                Ok(CandidateOutcome::Verified { verified, attestation }) => {
                    budget.examined();
                    let key = verified.dedup_key();
                    if !seen.contains(&key) {
                        seen.push(key);
                        matches.push((verified.result, attestation));
                    }
                    if arity == ScanArity::FirstMatch && !matches.is_empty() {
                        return Ok(ScanOutcome { matches, refused });
                    }
                }
                // Discriminated as the other content kind after fetch-and-parse.
                // Charged bytes, never a candidate slot.
                Ok(CandidateOutcome::ModeMismatch) => budget.skipped_other_mode(),
                // Verified, just not the predicate type that was asked for. It
                // spent a slot (it was examined in-mode) but records no failure,
                // so a scan that finds only these reports not-found (S-017).
                Ok(CandidateOutcome::TypeNarrowed) => budget.examined(),
                Err(kind) => {
                    budget.examined();
                    refused.push(RefusedCandidate {
                        referrer_digest: descriptor.digest.clone(),
                        reason: kind,
                    });
                }
            }
        }

        // C-007 / D9's fallback: the simplesigning shape is looked at **only**
        // when the bundle shape is ABSENT — no candidate matched *and none was
        // refused*. Absent a pin that is the whole preference rule, and it is
        // also what keeps the happy bundle path at exactly today's request
        // count: the two extra doors below cost nothing until the preferred
        // shape has come up empty.
        //
        // `refused.is_empty()` is the fail-closed half, and it is not a
        // refinement of `matches.is_empty()` — it is a different question. A
        // bundle that was fetched and *cryptographically refused* leaves the
        // match set empty exactly as a missing one does, so the old single
        // condition walked past a rejected signature onto a weaker one and
        // exited 0 with nothing in the report naming what had failed. The
        // trigger set {withheld, corrupted, replaced} splits here: only
        // withheld is an absence; corrupted and replaced are the verifier
        // having looked at a signature and rejected it, and a rejection must
        // carry its own exit code out through `finish_scan`'s aggregate rather
        // than be answered around.
        //
        // Scoped to refusals on purpose. A bundle-shaped candidate discriminated
        // as the *other content kind* (`CandidateOutcome::ModeMismatch` — an
        // attestation under a signature run) records no refusal and still lets
        // the fallback fire: nothing about a signature was rejected there, and
        // gating on `candidates.is_empty()` instead would refuse a subject whose
        // only bundle is an attestation while a perfectly good signature sidecar
        // sits beside it.
        if discover_simplesigning && matches.is_empty() && refused.is_empty() {
            let (sidecar, examined) = Self::scan_simplesigning(
                transport,
                ctx,
                &verifier,
                target,
                sidecar_referrers,
                via,
                budget,
                &rekor_keys,
            )
            .await?;
            total_candidates = total_candidates.saturating_add(examined);
            refused.extend(sidecar.refused);
            if !sidecar.verified.is_empty() {
                cache_sidecar_trust_material(ctx, &rekor_keys).await;
            }
            for signature in sidecar.verified {
                let key = signature.dedup_key();
                if seen.contains(&key) {
                    continue;
                }
                seen.push(key);
                matches.push((signature.result, None));
                if arity == ScanArity::FirstMatch {
                    return Ok(ScanOutcome { matches, refused });
                }
            }
        }

        // The `.att` half of the same fallback rule: one door, one candidate
        // slot, looked at only once nothing bundle-shaped verified. A collect-all
        // run that already has a modern attestation does not pay a request for
        // the legacy tag.
        //
        // `refused.is_empty()` carries the `.sig` gate's fail-closed half onto
        // this door, and for the identical reason: a bundle-shaped attestation
        // that was fetched and *refused* leaves `matches` empty exactly as a
        // missing one does. Without it, breaking the current SLSA provenance
        // bundle — one flipped byte, no forgery — opens this door onto a stale
        // but validly-signed `.att` sidecar, which then passes on its own
        // merits and is reported as the subject's provenance at exit 0. The
        // attacker never signs anything; they choose which signed attestation
        // OCX answers with by corrupting the others. `finish_scan`'s
        // attestation arm already routes empty-matches-plus-refusals to
        // `best_failure`, so the refusal carries its own exit code out.
        //
        // Not a refinement of `matches.is_empty()`, and not covered by
        // `stop_reason()` either: that is a truncation probe, and a
        // cryptographic refusal stamps no bound.
        //
        // It sits *after* `stop_reason()` on purpose. `&&` short-circuits, and
        // `a_spent_last_candidate_slot_is_not_reported_as_a_truncation` is the
        // only test that reds when this probe is replaced by the stamping
        // `may_examine()` — its seed refuses one candidate, so a `refused`
        // conjunct placed first would skip the probe and leave that swap
        // green. Both operands are side-effect-free, so the order costs
        // nothing and buys the mutation back.
        //
        // Whether *anything at all* was there to look at. The early return
        // above answers this before the `.att` door in every other mode; an
        // attestation run has to defer it until the door has been tried, and
        // the answer must still be the same kind — `no_signatures_found` means
        // "nothing to verify anywhere" and is deliberately distinct from
        // `attestation_not_found`, which means candidates *were* examined
        // (S-017; pinned by `test/tests/test_sbom.py`).
        let mut examined_anything = anything_listed;
        // `stop_reason`, never `may_examine`: probing the bounds here must not
        // *stamp* them. A run whose bundle loop spent exactly `caps.candidates`
        // slots leaves `stop` unset and reports its own per-candidate verdict
        // (`identity_mismatch`, say); a stamp from this probe would replace that
        // with a truncation claiming `unexamined == 0` — including on the
        // overwhelmingly common subject that carries no `.att` tag at all.
        if discover_attestation_sidecar && matches.is_empty() && budget.stop_reason().is_none() && refused.is_empty() {
            let predicate_type = match &ctx.content {
                VerifyContentMode::Attestation { predicate_type } => predicate_type.as_ref(),
                // Unreachable: `discover_attestation_sidecar` is this arm's negation.
                VerifyContentMode::Signature => None,
            };
            total_candidates = total_candidates.saturating_add(1);
            budget.examined();
            // The subject manifest's bytes: the keyless arm hands them to the
            // same `Verifier` the bundle path does, and the fetch above was
            // skipped when there were no bundle candidates to verify. One extra
            // manifest GET, and only on a run that already found nothing signed
            // and is about to ask for the legacy tag anyway.
            let subject_bytes = if subject_bytes.is_empty() {
                pull_subject_manifest_verified(transport, image, subject_digest).await?
            } else {
                subject_bytes
            };
            let remaining = caps.total_bytes.saturating_sub(budget.spent);
            // The same gate `scan_simplesigning` builds for the `.sig` door:
            // the crypto and policies a layer is judged against, plus how its
            // `dev.sigstore.cosign/bundle` annotation is checked and whether a
            // keyless layer carrying none may verify at all.
            let verify = simplesigning_read::SidecarVerification {
                verifier: &verifier,
                policies: ctx.policies,
                trust_root: ctx.trust_root,
                rekor_url: ctx.rekor_url,
                offline: ctx.offline,
                allow_unlogged: ctx.allow_unlogged_signature,
                rekor_keys: rekor_keys.clone(),
            };
            if let Some(sidecar) = attestation_sidecar::read_attestation_sidecar_tag(
                transport,
                image,
                subject_digest,
                &subject_bytes,
                &verify,
                predicate_type,
                remaining,
            )
            .await?
            {
                examined_anything = true;
                budget.charge(sidecar.bytes_read);
                if !sidecar.matches.is_empty() {
                    cache_sidecar_trust_material(ctx, &rekor_keys).await;
                }
                // The reader walks its own layers under its own caps, so a bound
                // that stopped it can never show up in this budget's counters.
                // Carried across so `finish_scan`'s fail-closed attestation arm
                // refuses: a truncated scan has looked at fewer attestations
                // than the sidecar carries, and returning the partial list as
                // success is exactly the "every attestation" answer it cannot
                // give. Without it, 32 duplicate layer descriptors ahead of the
                // real one buy a clean exit 0.
                if let Some(stop) = sidecar.stop {
                    budget.record_stop(stop);
                }
                refused.extend(sidecar.refused);
                // The same D6 dedup pass both other doors run. Pushing straight
                // into `matches` let one layer descriptor repeated N times in
                // the sidecar manifest contribute N rows of `signatures[]` off a
                // single blob.
                for found in sidecar.matches {
                    let key = found.verified.dedup_key();
                    if seen.contains(&key) {
                        continue;
                    }
                    seen.push(key);
                    matches.push((found.verified.result, Some(found.attestation)));
                }
            }
        }
        if discover_attestation_sidecar && !examined_anything {
            // No referrer, no legacy sidecar: the same verdict this run got
            // before the `.att` door existed, and the reason the early return
            // above could not simply be widened.
            return Err(VerifyErrorKind::NoSignaturesFound);
        }
        Self::finish_scan(ctx, caps, total_candidates, budget, matches, refused)
    }

    /// The simplesigning half of the merge: both cosign sidecar doors, read
    /// through the one `simplesigning_read` core.
    ///
    /// Returns what the two doors yielded plus the number of candidate slots
    /// they spent, so the caller's `total_candidates` and `budget.considered`
    /// stay in step — `aggregate_failure` reads their difference as truncation.
    ///
    /// A sidecar reachable through *both* doors yields the same layer digest and
    /// the same signature bytes twice; the caller's dedup pass is what makes it
    /// one row of `signatures[]` (S-009).
    #[expect(
        clippy::too_many_arguments,
        reason = "both sidecar doors, their shared budget, and the run-scoped Rekor key memo"
    )]
    async fn scan_simplesigning(
        transport: &dyn OciTransport,
        ctx: &VerifyContext<'_>,
        verifier: &Verifier,
        target: &ScanTarget,
        referrers: Vec<crate::oci::Descriptor>,
        via: DiscoveryMethod,
        budget: &mut ScanBudget,
        rekor_keys: &RekorKeyMemo,
    ) -> Result<(SidecarScan, usize), VerifyErrorKind> {
        let ScanTarget {
            image, subject_digest, ..
        } = target;
        let mut scan = SidecarScan::default();
        let mut examined = 0usize;
        // Built once for both doors: the crypto and policies a layer is judged
        // against, plus how its `dev.sigstore.cosign/bundle` annotation is
        // checked and whether a keyless layer carrying none may verify at all.
        let verify = simplesigning_read::SidecarVerification {
            verifier,
            policies: ctx.policies,
            trust_root: ctx.trust_root,
            rekor_url: ctx.rekor_url,
            offline: ctx.offline,
            allow_unlogged: ctx.allow_unlogged_signature,
            rekor_keys: rekor_keys.clone(),
        };

        // Door 1 — the OCI 1.1 referrer. Its `via` is the listing's, so a
        // sidecar the registry itself computed and one read off the mutable
        // fallback tag stay distinguishable in the report.
        for descriptor in referrers {
            if !budget.may_examine() {
                break;
            }
            examined = examined.saturating_add(1);
            budget.examined();
            if descriptor.size < 0 || descriptor.size as u64 > MAX_REFERRER_MANIFEST_BYTES {
                scan.refused.push(RefusedCandidate {
                    referrer_digest: descriptor.digest.clone(),
                    reason: VerifyErrorKind::BundleParseFailed,
                });
                continue;
            }
            let referrer_ref = image.clone_with_digest(descriptor.digest.clone());
            let manifest_bytes = match pull_referrer_manifest_capped(transport, &referrer_ref).await {
                Ok(bytes) => bytes,
                Err(kind) => {
                    budget.charge(MAX_REFERRER_MANIFEST_BYTES);
                    scan.refused.push(RefusedCandidate {
                        referrer_digest: descriptor.digest.clone(),
                        reason: kind,
                    });
                    continue;
                }
            };
            budget.charge(manifest_bytes.len() as u64);
            match simplesigning_read::read_sidecar_manifest(
                transport,
                image,
                &manifest_bytes,
                subject_digest,
                &verify,
                via,
            )
            .await
            {
                Ok(found) => {
                    scan.verified.extend(found.verified);
                    scan.refused.extend(found.refused);
                }
                Err(reason) => scan.refused.push(RefusedCandidate {
                    referrer_digest: descriptor.digest.clone(),
                    reason,
                }),
            }
        }

        // Door 2 — the `sha256-<hex>.sig` sidecar tag, which needs no Referrers
        // API at all. An absent tag is `Ok(None)`: "no legacy signature", the
        // overwhelmingly common case. Any other transport fault propagates — we
        // only reach here with nothing verified, so "I could not finish looking"
        // is the honest answer and must not read as "not signed".
        if budget.may_examine() {
            examined = examined.saturating_add(1);
            budget.examined();
            if let Some(found) = simplesigning_read::read_sidecar_tag(
                transport,
                image,
                subject_digest,
                SidecarKind::Signature,
                &verify,
                DiscoveryMethod::SidecarTag,
            )
            .await?
            {
                scan.verified.extend(found.verified);
                scan.refused.extend(found.refused);
            }
        }

        Ok((scan, examined))
    }

    /// Turn a finished scan into its answer, or into the one failure that best
    /// describes why there is none.
    fn finish_scan(
        ctx: &VerifyContext<'_>,
        caps: ContentCaps,
        total_candidates: usize,
        budget: &ScanBudget,
        matches: Vec<(VerifyResult, Option<VerifiedAttestation>)>,
        refused: Vec<RefusedCandidate>,
    ) -> Result<ScanOutcome, VerifyErrorKind> {
        let unexamined = total_candidates.saturating_sub(budget.considered);
        match &ctx.content {
            // A `FirstMatch` scan only ever arrives here having found nothing —
            // it returns at its first match. An `All` scan (`report_all`, which
            // is what populates `signatures[]`) runs the loop to the end and
            // arrives here *with* its matches, so this arm cannot be
            // unconditionally `Err` any more: that would discard the answer and
            // report "no signatures found" about a subject that verified.
            VerifyContentMode::Signature if !matches.is_empty() => Ok(ScanOutcome { matches, refused }),
            VerifyContentMode::Signature => {
                // Nothing verified: the aggregate is today's, over the candidates
                // actually looked at rather than the ones that spent a slot. The
                // refusals are consumed to build it — nothing survives this arm
                // to report.
                Err(aggregate_failure(
                    total_candidates,
                    budget.considered,
                    best_failure(refused),
                ))
            }
            // Fail-closed, and before any per-candidate failure: a truncated
            // scan cannot answer a question about *every* attestation, so
            // returning the partial list would understate what the subject
            // carries. Which bound stopped it is the actionable part.
            VerifyContentMode::Attestation { .. } => {
                match budget.stop.map(|stop| truncation_failure(caps, stop, unexamined)) {
                    Some(kind) => Err(kind),
                    // Every refusal recorded here is a real defect in a candidate of
                    // the requested type — a narrowing miss records none — so an
                    // empty result with nothing recorded is genuinely "not found".
                    None if matches.is_empty() => {
                        Err(best_failure(refused).unwrap_or(VerifyErrorKind::AttestationNotFound))
                    }
                    // Matches *and* refusals travel out together. Failing here on a
                    // recorded refusal would let one malformed referrer hide every
                    // valid attestation beside it.
                    None => Ok(ScanOutcome { matches, refused }),
                }
            }
        }
    }

    /// Verify a single signature-referrer candidate end-to-end from its
    /// already-fetched manifest bytes: parse → bundle blob → `sigstore`
    /// verification (chain, SCT, signature, tlog-body binding, validity window)
    /// → Rekor SET + inclusion proof → identity/policy → cache. Returns
    /// [`VerifyResult`] on full success; any failure is one candidate's verdict,
    /// which the ANY-of loop aggregates.
    ///
    /// The referrer manifest is fetched (and its read bounded) by the caller so
    /// the cross-candidate byte budget is charged from bytes actually read.
    /// `budget` carries that same budget: the bundle blob is fetched here, so it
    /// is charged here, on the success and failure paths alike. Charging at the
    /// one site that reads the bytes is what keeps the two paths in step —
    /// leaving it to the caller is how the blob went uncharged while the budget
    /// documented itself as bounding total download. The candidate counters are
    /// the caller's: only it can see which of the three outcomes came back.
    #[expect(
        clippy::too_many_arguments,
        reason = "one candidate, its context, and the run-scoped material"
    )]
    async fn verify_one_referrer(
        transport: &dyn OciTransport,
        ctx: &VerifyContext<'_>,
        verifier: &Verifier,
        descriptor: &crate::oci::Descriptor,
        referrer_bytes: Vec<u8>,
        subject_digest: &Digest,
        subject_bytes: &[u8],
        image: &native::Reference,
        via: DiscoveryMethod,
        budget: &mut ScanBudget,
        rekor_keys: &RekorKeyMemo,
    ) -> Result<CandidateOutcome, VerifyErrorKind> {
        let referrer_digest =
            Digest::try_from(descriptor.digest.as_str()).map_err(|e| VerifyErrorKind::Internal(Box::new(e)))?;

        let referrer_manifest: crate::oci::referrer::ReferrerManifest =
            serde_json::from_slice(&referrer_bytes).map_err(|_| VerifyErrorKind::BundleParseFailed)?;
        let bundle_layer = referrer_manifest
            .layers
            .first()
            .ok_or(VerifyErrorKind::NoUsableBundle)?;
        let bundle_blob_digest =
            Digest::try_from(bundle_layer.digest.as_str()).map_err(|_| VerifyErrorKind::BundleParseFailed)?;

        // Bundle-blob size cap (CWE-400): the bundle-blob digest comes from the
        // untrusted referrer manifest, so digest verification does not bound size.
        // Reject an over-cap descriptor before opening a connection, then bound
        // the actual read so a registry lying about the size still cannot force an
        // unbounded allocation.
        let caps = ctx.content.caps();
        if bundle_layer.size < 0 || bundle_layer.size as u64 > caps.bundle_bytes as u64 {
            // Attestation mode names the bound it tripped (checklist row 15): an
            // SBOM near the ceiling is a real authoring outcome, and "malformed
            // referrer" would send its author looking in the wrong place.
            // Signature mode keeps the kind `ocx package verify` has always
            // reported for this shape.
            return Err(match &ctx.content {
                VerifyContentMode::Signature => VerifyErrorKind::BundleParseFailed,
                VerifyContentMode::Attestation { .. } => VerifyErrorKind::AttestationTooLarge {
                    limit: caps.bundle_bytes as u64,
                    actual: bundle_layer.size.max(0) as u64,
                },
            });
        }
        let bundle_bytes = match pull_blob_capped(transport, image, &bundle_blob_digest, caps.bundle_bytes).await {
            Ok(bytes) => {
                budget.charge(bytes.len() as u64);
                bytes
            }
            Err(kind) => {
                // A rejected read still cost up to the cap — the bounded read
                // stops at cap + 1, not at zero. Same treatment the caller gives
                // an over-cap referrer manifest.
                budget.charge(caps.bundle_bytes as u64);
                return Err(kind);
            }
        };

        // Every structural pass over the candidate's bytes, on one blocking
        // thread. The parse is bounded by the mode's per-candidate cap rather
        // than the signature-bundle constant: the fetch above already accepted
        // up to `caps.bundle_bytes`, and re-capping at 512 KiB here would refuse
        // every SBOM larger than that after paying to download it.
        //
        // Why the whole block and not just the parse: in attestation mode these
        // are serde passes over up to `MAX_ATTESTATION_ENVELOPE_BYTES` (32 MiB)
        // of registry-supplied JSON with no await between them, and
        // `verify_envelope` is the *heavier* half — it clones the payload,
        // re-serializes the envelope and parses it twice more. Tens of
        // milliseconds of pure CPU, once per candidate, up to `caps.candidates`
        // candidates. Left inline any of it starves a runtime worker for the
        // whole pass (ASYNC-01), including the sibling blob pulls of the same
        // scan. Nothing here holds a lock or a guard, and every capture is moved
        // rather than borrowed, so the boundary costs two clones of a mode and a
        // digest.
        let bundle_cap = caps.bundle_bytes;
        let content = ctx.content.clone();
        let target_digest = subject_digest.clone();
        let parsed = tokio::task::spawn_blocking(move || {
            let bundle = parse_bundle(&bundle_bytes, bundle_cap).ok_or(VerifyErrorKind::BundleParseFailed)?;
            let parts = match BundleParts::from_bundle(&bundle, &content) {
                Ok(parts) => parts,
                // `from_bundle` returns this kind for exactly one class of
                // reason: the candidate's content oneof does not answer this
                // mode — it carries the other kind, or no content at all. That
                // is not a failure of the question this run asked, and charging
                // it a candidate slot is how attestations crowd a signature out
                // of the scan.
                Err(VerifyErrorKind::NoUsableBundle) => return Ok(ParsedCandidate::ModeMismatch),
                Err(kind) => return Err(kind),
            };

            // D-d, the structural half: OCX's own checks run BEFORE the
            // delegated call so their precise kinds are the ones a user sees.
            // The delegated call refuses most malformed statements too, but with
            // one generic error — ordering OCX's diagnosis first turns that
            // refusal into redundancy rather than the only report.
            let envelope = match &content {
                // A cosign image signature IS an in-toto Statement (D2), so
                // signature mode asks the same structural questions attestation
                // mode does — the subject binding and the predicateType. The
                // type is fixed rather than caller-chosen here, so a mismatch is
                // a refusal and never the narrowing miss the attestation arm can
                // return: `from_bundle` already routed every other predicateType
                // to the attestation question, so a disagreement at this point
                // means the tolerant router and the strict parser read the same
                // field differently, which is a defect in the candidate.
                VerifyContentMode::Signature => {
                    let cosign_signature = PredicateType::Uri(COSIGN_SIGN_PREDICATE_TYPE.to_owned());
                    Some(dsse::verify_envelope(&bundle, &target_digest, Some(&cosign_signature))?)
                }
                VerifyContentMode::Attestation { predicate_type } => {
                    match dsse::verify_envelope(&bundle, &target_digest, predicate_type.as_ref()) {
                        Ok(verified) => Some(verified),
                        // A narrowing miss, only reachable when a type was
                        // requested: this candidate is sound, it simply is not
                        // the document that was asked for (S-017).
                        Err(VerifyErrorKind::PredicateTypeMismatch { .. }) if predicate_type.is_some() => {
                            return Ok(ParsedCandidate::TypeNarrowed);
                        }
                        Err(kind) => return Err(kind),
                    }
                }
            };
            Ok(ParsedCandidate::Ready(Box::new(ParsedBundle {
                bundle,
                parts,
                envelope,
            })))
        })
        .await
        // A panic in the structural pass is not a verdict about the candidate,
        // so it keeps its own kind — and it is logged, because `Internal` ranks
        // lowest in `failure_rank`, so any sibling refusal would otherwise be
        // the only thing reported. The digest is the parsed one, whose format
        // this function already validated.
        .map_err(|error| {
            tracing::warn!("bundle verification task panicked for referrer {referrer_digest}: {error}");
            VerifyErrorKind::Internal(Box::new(error))
        })??;
        let (bundle, parts, verified_envelope) = match parsed {
            ParsedCandidate::Ready(ready) => {
                let ParsedBundle {
                    bundle,
                    parts,
                    envelope,
                } = *ready;
                (bundle, parts, envelope)
            }
            ParsedCandidate::ModeMismatch => return Ok(CandidateOutcome::ModeMismatch),
            ParsedCandidate::TypeNarrowed => return Ok(CandidateOutcome::TypeNarrowed),
        };

        // Row 7 / D-e, the annotation direction. An annotation may order or
        // pre-filter the scan; it may never decide it, so a candidate whose
        // unsigned `predicateType` annotation disagrees with its signed payload
        // is refused rather than silently relabelled. Reported as a failure
        // (not a narrowing miss) on purpose: a registry that rewrites this one
        // string must not be able to turn a signed SBOM into "none found".
        //
        // The ordering half of that sentence is [`order_candidates`], which
        // demotes a `dev.sigstore.bundle.content` hint naming the other kind to
        // the tail of the scan without ever dropping it. Neither half reads an
        // annotation as an answer: this one only ever refuses, that one only
        // ever reorders.
        if let Some(verified) = verified_envelope.as_ref()
            && let Some(annotated) = referrer_manifest
                .annotations
                .as_ref()
                .and_then(|annotations| annotations.get(ANNOTATION_BUNDLE_PREDICATE_TYPE))
            && annotated != &verified.attestation.predicate_type
        {
            return Err(VerifyErrorKind::PredicateTypeMismatch {
                expected: annotated.clone(),
                actual: verified.attestation.predicate_type.clone(),
            });
        }

        // The two key models diverge here, and only here. Everything above is
        // shared — the referrer parse, the bounded blob read, the DSSE
        // structural pass and the annotation cross-check all ask questions that
        // have nothing to do with what produced the signature. Which arm runs is
        // not a choice this function makes: it is the shape the bundle declared,
        // and `BundleParts` already refused every mixture of the two.
        // Set on both arms from an entry `verify_rekor_set` accepted, and never
        // from anything else.
        let mut rekor_log_index = None;
        let signer = match parts {
            BundleParts::Keyless { leaf_der, tlog } => {
                // Everything an X.509 verifier must do, done by one: the chain is built
                // against the trust root *at the certificate's own issuance time*, the
                // embedded SCT is checked against the CT log keys, the signature is
                // verified over the subject digest with the leaf key, the Rekor entry
                // body is rebuilt from (digest, signature, certificate) and compared to
                // the logged body (CVE-2022-36056 / GHSA-whqx class), and the integrated
                // time is checked to fall inside the certificate's validity window.
                //
                // `offline: true` is deliberate and is not "skip a check": `sigstore`'s
                // online branch does not fetch anything, it *refuses* a bundle carrying
                // no inclusion proof. `verify_rekor_set` below refuses the same bundle
                // (`RekorInclusionProofAbsent`) and additionally verifies the SET and
                // the proof against a key the trust root pinned — so this path is a
                // superset of the online branch, not a relaxation of it. That claim
                // only holds while the proof stays mandatory there; do not make it
                // conditional again.
                verifier
                    .verify(subject_bytes, bundle, &PolicyDeferredToOcx, true)
                    .await
                    .map_err(map_verification_error)?;

                // Verify the Rekor SET against the log's public key — pinned from the
                // trust root when present (offline-capable, closes the TOFU hole),
                // otherwise fetched online. Returns the key PEM used, so a successful
                // online run can cache the trust material for later offline verifies.
                let rekor_key_pem = verify_rekor_set(ctx, &tlog, rekor_keys).await?;
                // Recorded only after the SET and the inclusion proof passed, so
                // a reported log index is always one the log itself vouched for.
                rekor_log_index = Some(tlog.log_index);

                // D-d, the tlog half: checklist row 12, over entry material the two
                // calls above have already SET- and Merkle-checked. Splitting it out of
                // the structural half is what lets the structural kinds be precise while
                // this one still runs against a body known to be the logged one.
                if let Some(verified) = verified_envelope.as_ref() {
                    dsse::verify_tlog_binding(
                        &tlog.canonicalized_body,
                        &verified.attestation.payload,
                        &verified.signatures,
                    )?;
                }

                // Identity + issuer match against the resolved trust policies (ANY-of).
                // The matched subset, not a boolean: the `builder` pin below is ANDed
                // within a policy and ORed across the set, so it is decided from the
                // policies this certificate actually satisfied.
                let matched_policies = matching_policies(&leaf_der, ctx.policies)?;

                // #103. Inert on a non-provenance predicate — which is what makes it
                // inert for an image signature, whose predicate is empty (it is no
                // longer inert by having no envelope to read). A refusal — never a skip
                // — when a pin is in force and the provenance names another builder or
                // none that can be read.
                if let Some(verified) = verified_envelope.as_ref() {
                    dsse::enforce_builder_pin(&matched_policies, &verified.attestation)?;
                }

                // On a successful online run, cache the trust material for later offline
                // verifies against the same Rekor instance. Best-effort + content-equal
                // skip so a batch does not stampede the file or slide the 24h TTL on use.
                if !ctx.offline {
                    cache_trust_material(ctx, rekor_key_pem).await;
                }

                let cert = parse_certificate(&leaf_der)?;

                // Row 13 (CVE-2024-55655), re-asserted over the same parsed leaf the
                // identity extraction below reads. Placed in the tail both content
                // modes share, so "runs for attestations too" is structural. It has no
                // key-mode twin because there is no certificate to bound.
                tlog::verify_integrated_time_within_certificate(
                    // `TransparencyLog`, never the clock: this instant is the entry's
                    // `integratedTime`, already SET- and inclusion-checked above.
                    // Saturating rather than fallible: `BundleTlog::from_entry` widened a
                    // non-negative i64 into this u64, so the conversion cannot trip,
                    // and i64::MAX would fail closed against any real window.
                    SigningInstant::TransparencyLog(i64::try_from(tlog.integrated_time).unwrap_or(i64::MAX)),
                    &cert,
                )?;

                VerifiedSigner {
                    key_backend: KeyBackendKind::Keyless,
                    // Read back off the verified leaf, so these report the
                    // certificate that passed the chain, the SCT and the
                    // signature — not merely one the bundle carried.
                    certificate_identity: subject_identity(&cert),
                    certificate_oidc_issuer: oidc_issuer(&cert),
                    signed_at: Some(tlog.integrated_time),
                }
            }

            // Spec §WP9. `sigstore::bundle::verify` is bypassed here, and it has
            // to be: `sign::to_bundle()` hardcodes `Content::X509CertificateChain`
            // and the verifier reads its key exclusively from
            // `tbs_certificate.subject_public_key_info`. Neither side has a
            // `PublicKey` arm, so handing a key-mode bundle to that verifier does
            // not verify it weakly — it cannot verify it at all. What replaces
            // it is the same DSSE signature check the library performs, against
            // a key the *policy* named rather than one the bundle carried, which
            // is the stricter provenance of the two.
            //
            // Nothing else from the keyless tail applies. No chain, no SCT, no
            // SAN, no validity window — those are not checks skipped here, they
            // are checks over material this shape does not have, which is why
            // `certificate_identity` and `certificate_oidc_issuer` come out
            // absent rather than empty.
            BundleParts::Key { hint, tlog } => {
                // Always `Some` since D2 (both content modes produce a verified
                // envelope); the refusal is the defensive floor, because the key
                // path has no second source of signature bytes to fall back on.
                let envelope = verified_envelope.as_ref().ok_or(VerifyErrorKind::NoUsableBundle)?;
                let signature = &envelope
                    .signatures
                    .first()
                    .ok_or(VerifyErrorKind::SignatureInvalid)?
                    .sig;

                // The PAE, never the bare payload: a signature checked over the
                // payload alone is forgeable across payload types (DSSE's whole
                // reason for the encoding). `DsseEnvelope::parse` already refused
                // any `payloadType` but this one, so the constant is the
                // envelope's own declared type and not an assumption.
                let pae =
                    crate::oci::attest::dsse::pae(crate::oci::attest::DSSE_PAYLOAD_TYPE, &envelope.attestation.payload);
                let matched_policies =
                    identity::matching_key_policies(&pae, signature, ctx.policies).inspect_err(|_| {
                        // The hint is unauthenticated and decides nothing — but
                        // it is the only thing that tells an operator staring at
                        // the refusal *which* key the publisher claims to have
                        // used.
                        tracing::debug!(
                            "no trusted key verified referrer {referrer_digest}; \
                             the bundle's public-key hint is {hint}"
                        );
                    })?;

                // D10 / meta-plan D: transparency evidence is optional here and
                // mandatory on the keyless arm. When it is present it is checked
                // in full — the same SET, the same Merkle proof, the same
                // logged-body binding — so "optional" narrows what must exist,
                // never how hard what exists is looked at.
                let signed_at = match tlog {
                    Some(entry) => {
                        let rekor_key_pem = verify_rekor_set(ctx, &entry, rekor_keys).await?;
                        rekor_log_index = Some(entry.log_index);
                        dsse::verify_tlog_binding(
                            &entry.canonicalized_body,
                            &envelope.attestation.payload,
                            &envelope.signatures,
                        )?;
                        if !ctx.offline {
                            cache_trust_material(ctx, rekor_key_pem).await;
                        }
                        Some(entry.integrated_time)
                    }
                    // Absent: `cosign sign --key` uploads to Rekor only when
                    // asked, so there is no instant to report and none is
                    // invented. The certificate-window check that would have
                    // consumed it does not exist on this path either.
                    None => None,
                };

                dsse::enforce_builder_pin(&matched_policies, &envelope.attestation)?;

                VerifiedSigner {
                    // Every `Scheme` `Scheme::is_implemented` admits reaches
                    // `compile_key_reference` as raw PEM bytes — `file://` off
                    // disk, `env://` out of the environment — so a
                    // `PolicyBackend::Key` arriving here was compiled from a
                    // local PEM whichever door it came through. A KMS backend,
                    // which is a *remote* key rather than another way to spell a
                    // local one, must widen this; that is why the field names the
                    // backend rather than saying "key".
                    key_backend: KeyBackendKind::File,
                    certificate_identity: None,
                    certificate_oidc_issuer: None,
                    signed_at,
                }
            }
        };

        // D6's fallback dedup key. The DSSE envelope's own signature, which both
        // key models produce since D2 — the same bytes `sigstore` and the key
        // arm each verified, so two doors onto one bundle key identically.
        let signature = verified_envelope
            .as_ref()
            .and_then(|verified| verified.signatures.first())
            .map(|signature| signature.sig.clone())
            .unwrap_or_default();

        Ok(CandidateOutcome::Verified {
            verified: VerifiedSignature {
                result: VerifyResult {
                    subject_digest: subject_digest.clone(),
                    referrer_digest,
                    key_backend: signer.key_backend,
                    certificate_identity: signer.certificate_identity,
                    certificate_oidc_issuer: signer.certificate_oidc_issuer,
                    signed_at: signer.signed_at,
                    signature_format: SignatureFormat::Bundle,
                    discovery_method: via,
                    rekor_log_index,
                },
                signature,
            },
            attestation: verified_envelope.map(|verified| verified.attestation),
        })
    }

    /// List the Sigstore-bundle referrers for the subject, and how they were
    /// found. Empty → the caller maps to `NoSignaturesFound` (79).
    async fn list_signature_referrers(
        transport: &dyn OciTransport,
        image: &native::Reference,
        subject_digest: &Digest,
        artifact_type: Option<&str>,
    ) -> Result<ReferrersListing, VerifyErrorKind> {
        // The Unsupported verdict no longer refuses the operation: the OCI referrers
        // tag-schema fallback (`list_referrers_with_fallback` /
        // `append_referrer_fallback_index`) serves a registry without the Referrers
        // API. See `adr_oci_referrers_signing_v1.md`, Amendment 10 — the fallback
        // index is a mutable tag anyone with push access authors, and the residual
        // attack surface that reverses S1-F is recorded there.
        //
        // Server-side artifactType filter; the caller re-filters client-side,
        // since the OCI spec permits a registry to ignore it.
        //
        // The cosign *simplesigning* shape reaches verification through two
        // doors, and this is one of them: a referrer whose `artifactType` is
        // `COSIGN_SIG_ARTIFACT_TYPE` or `COSIGN_SBOM_ARTIFACT_TYPE` carries the
        // same payload `simplesigning_read` reads off a `sha256-<hex>.sig`
        // sidecar tag, and the `via: DiscoveryMethod` this listing returns is
        // what tells the two apart in the report. `artifact_type` is therefore
        // the caller's: `Some(SIGSTORE_BUNDLE_V03)` while the bundle shape is
        // the only one being discovered, `None` when both buckets are wanted out
        // of one request.
        //
        // **Gap reported to G1, not minted here:** there is no frozen constant
        // for a cosign *attestation* artifact type, so `.att` by OCI 1.1
        // referrer has no spelling to code against and is out of scope.
        //
        // **The `.sbom` sidecar tag is not reached from here either**, and for a
        // different reason than `.att`: it has a reader, but not on this path.
        // Its layer is the SBOM document itself, so it belongs to
        // `scan_unverified`'s permissive listing and never to a signature scan.
        // `simplesigning_read::SidecarKind` still does not name it — that enum
        // selects a simplesigning reader, and aiming one at a `.sbom` layer
        // returns an empty scan for every sidecar that exists.
        transport
            .list_referrers_with_fallback(image, subject_digest, artifact_type)
            .await
            .map_err(map_client_error)
    }
}

/// The verification facts the two key models establish differently.
///
/// Exists so the key-model match yields one named value instead of a tuple of
/// two adjacent `Option<String>`s, where a swap type-checks silently.
struct VerifiedSigner {
    key_backend: KeyBackendKind,
    certificate_identity: Option<String>,
    certificate_oidc_issuer: Option<String>,
    signed_at: Option<u64>,
}

/// The Rekor transparency evidence one bundle carries, already structurally
/// checked.
///
/// Its own type because its *presence* is not universal, and the two key models
/// disagree about that: keyless requires it, key mode does not, because cosign's
/// `sign --key` uploads to Rekor only when asked (D10). Splitting it out is what
/// lets [`BundleParts`] make that asymmetry a property of the discriminant
/// instead of a rule someone downstream has to remember.
struct BundleTlog {
    signed_entry_timestamp: Vec<u8>,
    canonicalized_body: Vec<u8>,
    integrated_time: u64,
    log_index: u64,
    log_id_hex: String,
    /// The Merkle inclusion proof. Not optional: an entry without one is
    /// refused in [`BundleTlog::from_entry`], so downstream code cannot
    /// forget to check and cannot silently fall back to the SET alone.
    inclusion_proof: ProtoInclusionProof,
}

impl BundleTlog {
    /// Read one `tlogEntries` element, refusing a promise-only or proof-only
    /// entry.
    fn from_entry(
        entry: &sigstore_protobuf_specs::dev::sigstore::rekor::v1::TransparencyLogEntry,
    ) -> Result<Self, VerifyErrorKind> {
        let signed_entry_timestamp = entry
            .inclusion_promise
            .as_ref()
            .map(|promise| promise.signed_entry_timestamp.clone())
            .ok_or(VerifyErrorKind::RekorSetAbsentTsaPresent)?;

        // Mandatory. The SET is only a promise to include; the proof is the
        // evidence that the entry is in a tree whose root the log signed.
        // Bundle profile v0.1/v0.2 leaves the proof optional at the schema
        // level, so without this a promise-only bundle would verify on strictly
        // weaker evidence than `sigstore`'s own online branch accepts.
        let inclusion_proof = entry
            .inclusion_proof
            .clone()
            .ok_or(VerifyErrorKind::RekorInclusionProofAbsent)?;

        Ok(Self {
            signed_entry_timestamp,
            canonicalized_body: entry.canonicalized_body.clone(),
            integrated_time: entry.integrated_time.max(0) as u64,
            log_index: entry.log_index.max(0) as u64,
            log_id_hex: entry
                .log_id
                .as_ref()
                .map(|id| hex::encode(&id.key_id))
                .unwrap_or_default(),
            inclusion_proof,
        })
    }
}

/// The verification material a parsed bundle offers, and the transparency
/// evidence that came with it.
///
/// An enum, not a struct carrying an empty `leaf_der`: a key-mode bundle has
/// **no** certificate at all, and an empty DER flowing into `parse_certificate`
/// is exactly the silent-accept shape this split makes unrepresentable. The
/// Rekor asymmetry rides the same discriminant — mandatory on the keyless arm,
/// optional on the key arm — so "a keyless bundle with no tlog entry" cannot be
/// *constructed*, rather than being refused by a check some later reader could
/// relax.
enum BundleParts {
    /// Keyless Sigstore: a Fulcio leaf certificate (DER) plus its mandatory
    /// transparency evidence.
    Keyless { leaf_der: Vec<u8>, tlog: BundleTlog },
    /// Key mode (spec §WP9): no certificate, only the bundle's public-key hint,
    /// and transparency evidence **only if the signer uploaded one**.
    Key {
        /// cosign's `publicKey.hint` — by the protobuf's own words an
        /// *unauthenticated* hint, so it decides nothing here. It is logged
        /// when no trusted key verified the signature, which is the one moment
        /// an operator needs to know which key the publisher claims to have
        /// used.
        hint: String,
        tlog: Option<BundleTlog>,
    },
}

impl BundleParts {
    fn from_bundle(
        bundle: &sigstore_protobuf_specs::dev::sigstore::bundle::v1::Bundle,
        mode: &VerifyContentMode,
    ) -> Result<Self, VerifyErrorKind> {
        // The candidate must answer the question this run asked. Both a cosign
        // image signature and an attestation are DSSE envelopes over an in-toto
        // Statement (D2), so the `content` oneof no longer tells them apart —
        // the Statement's `predicateType` does. Both directions are a
        // per-candidate verdict, not an abort: the scan records a mismatch as
        // `ModeMismatch`, which charges the bytes and spends no candidate slot,
        // and keeps going. A subject legitimately carries both kinds, and
        // failing here is how an attestation crowds a signature out of a scan.
        //
        // `messageSignature` is refused in BOTH modes: it is the pre-parity
        // shape and nothing on this path reads it any more.
        //
        // Still asked FIRST, before the verification material is read, and for
        // the same reason as before the predicateType entered it: reading
        // material first would report a malformed bundle of the *other* kind as
        // this mode's failure and spend a slot on it — the crowd-out the
        // non-consuming skip exists to prevent, reached through a different
        // door. What changed is the cost, not the order: the discrimination now
        // parses the DSSE payload rather than only reading the `content` oneof.
        // Those bytes are already in memory and already capped by the caller's
        // bounded blob read, and the probe is deliberately tolerant
        // ([`dsse::is_cosign_image_signature`]) so an unreadable payload routes
        // to the attestation question and keeps its precise refusal instead of
        // vanishing into a skip.
        let content_matches_mode = match bundle.content.as_ref() {
            Some(bundle::Content::DsseEnvelope(envelope)) => {
                let image_signature = dsse::is_cosign_image_signature(envelope);
                match mode {
                    VerifyContentMode::Signature => image_signature,
                    VerifyContentMode::Attestation { .. } => !image_signature,
                }
            }
            _ => false,
        };
        if !content_matches_mode {
            return Err(VerifyErrorKind::NoUsableBundle);
        }

        let material = bundle
            .verification_material
            .as_ref()
            .ok_or(VerifyErrorKind::BundleParseFailed)?;
        // Read once, for both arms. A malformed entry is a refusal under either
        // key model — only its *absence* is read differently below.
        let tlog = material.tlog_entries.first().map(BundleTlog::from_entry).transpose()?;

        match material.content.as_ref() {
            Some(verification_material::Content::X509CertificateChain(chain)) => Ok(Self::Keyless {
                leaf_der: chain
                    .certificates
                    .first()
                    .map(|certificate| certificate.raw_bytes.clone())
                    .ok_or(VerifyErrorKind::BundleParseFailed)?,
                tlog: tlog.ok_or(VerifyErrorKind::RekorSetInvalid)?,
            }),
            Some(verification_material::Content::Certificate(certificate)) => Ok(Self::Keyless {
                leaf_der: certificate.raw_bytes.clone(),
                tlog: tlog.ok_or(VerifyErrorKind::RekorSetInvalid)?,
            }),
            // Key mode (D10 / meta-plan D). The absent tlog entry that refuses a
            // keyless bundle one arm up is legal here and *must* stay legal:
            // cosign's `sign --key` writes no Rekor entry unless asked, so
            // demanding one would refuse every offline key signature cosign
            // produces. Nothing is weakened by it — the signature is still
            // verified against a pinned public key, which is the whole of what
            // key mode ever claimed.
            Some(verification_material::Content::PublicKey(key)) => Ok(Self::Key {
                hint: key.hint.clone(),
                tlog,
            }),
            _ => Err(VerifyErrorKind::BundleParseFailed),
        }
    }
}

/// Verify the Rekor transparency evidence, returning the log key PEM used.
///
/// Checks the Signed Entry Timestamp and the Merkle inclusion proof, both
/// mandatory. Both are computed by `sigstore-rs` in [`tlog`] — no signature,
/// hash-chain or checkpoint parsing lives here.
///
/// Key source is [`RekorKeyMemo::resolve`]'s ladder, in order:
/// 0. **Already resolved this run** — the memo, keyed on this entry's own
///    `logId`, so N candidates cost one resolution and not N (#374, #319).
/// 1. **Pinned** — the trust root carries a Rekor public key (from a TUF root or
///    the trust-root cache). Used with no network; this is the offline path and
///    the fix for #194's trust-on-first-use Rekor-key fetch.
/// 2. **Offline, unpinned** — cannot fetch and no pinned key → fail. (The CLI
///    gates this to an actionable exit-78 error before the pipeline runs; this
///    is the defensive backstop.)
/// 3. **Online, unpinned** — TOFU-fetch from `--rekor-url/api/v1/log/publicKey`
///    (the prior behavior), and return it so the caller can cache it.
async fn verify_rekor_set(
    ctx: &VerifyContext<'_>,
    entry: &BundleTlog,
    rekor_keys: &RekorKeyMemo,
) -> Result<String, VerifyErrorKind> {
    let pem = rekor_keys
        .resolve(ctx.trust_root, ctx.rekor_url, ctx.offline, &entry.log_id_hex)
        .await?;
    let key = tlog::rekor_key(&pem)?;
    tlog::verify_set(
        &key,
        &tlog::TlogEntry {
            canonicalized_body: &entry.canonicalized_body,
            integrated_time: entry.integrated_time,
            log_index: entry.log_index,
            log_id_hex: &entry.log_id_hex,
            signed_entry_timestamp: &entry.signed_entry_timestamp,
        },
    )?;

    // The Merkle proof is independent evidence, and `BundleTlog` has already
    // guaranteed it is present — the type carries the invariant so no caller
    // can fall back to the SET alone.
    tlog::verify_inclusion(&key, &entry.inclusion_proof, &entry.canonicalized_body)?;
    Ok(pem)
}

/// The Rekor log public keys this verify run has already resolved, keyed on the
/// entry's `logId` hex.
///
/// One run resolves the key many times over: once per simplesigning layer of a
/// cosign sidecar (#374) and once per candidate on the bundle path (#319).
/// Unpinned, each of those was its own `/api/v1/log/publicKey` fetch.
///
/// **Keyed on `log_id_hex`, and that is the whole security of it.** The log id
/// arrives from an untrusted sidecar manifest, and
/// [`TrustRoot::rekor_public_key_pem_for`] answers *per log* — a trust root
/// carrying two logs across a rotation returns a different key for each. A memo
/// keyed on nothing would hand the first entry's key to every later one, so an
/// entry from the second log would have its SET checked against the first
/// log's key. That is precisely the confusion
/// `the_rekor_key_is_selectable_by_log_id_and_falls_back_when_unknown` pins out
/// of the selector, re-introduced one layer above it.
///
/// **Successes only.** A transient Rekor 5xx while resolving candidate 1 must
/// not decide candidate 2: this scan is ANY-of — "one verified signature is the
/// ANY-of answer" — and a cached `Err` would promote one flaky fetch into a
/// whole-scan refusal.
///
/// Cheap to clone, and a clone is the *same* memo: the copy
/// [`SidecarVerification`](super::simplesigning_read::SidecarVerification)
/// carries and the one the bundle path holds share one map, so the two doors
/// onto a subject do not each pay for their own resolution.
#[derive(Clone, Default)]
pub struct RekorKeyMemo {
    /// `logId` hex → PEM.
    ///
    /// A `std::sync::Mutex`, and the guard is never held across an `.await`:
    /// the fetch runs with the lock released, so two tasks racing the same cold
    /// log id both fetch and the second insert wins — one wasted request, never
    /// a deadlock.
    resolved: Arc<Mutex<HashMap<String, String>>>,
}

impl RekorKeyMemo {
    /// The Rekor public key PEM for `log_id_hex`: this run's memo first, then
    /// pinned trust material, then an online fetch, and nothing at all when the
    /// run is offline.
    ///
    /// Shared with the cosign sidecar path rather than duplicated, so the
    /// `dev.sigstore.cosign/bundle` annotation cannot grow a second, subtly
    /// different ladder — in particular one that forgets the offline refusal and
    /// reaches for the network anyway.
    ///
    /// The memo is consulted ahead of the trust root, which costs nothing: the
    /// answer is a function of `log_id_hex` and the run's trust root, and the
    /// trust root does not change mid-run.
    ///
    /// # Errors
    ///
    /// [`VerifyErrorKind::TransparencyLogUnavailable`] when the trust root pins
    /// no key for this log and either the run is offline or the fetch fails.
    pub(super) async fn resolve(
        &self,
        trust_root: &TrustRoot,
        rekor_url: &Url,
        offline: bool,
        log_id_hex: &str,
    ) -> Result<String, VerifyErrorKind> {
        if let Some(memoized) = self.lock().get(log_id_hex).cloned() {
            return Ok(memoized);
        }
        let pem = match trust_root.rekor_public_key_pem_for(log_id_hex) {
            Some(pinned) => pinned,
            None if offline => return Err(VerifyErrorKind::TransparencyLogUnavailable),
            None => fetch_rekor_public_key_pem(rekor_url).await?,
        };
        self.lock().insert(log_id_hex.to_owned(), pem.clone());
        Ok(pem)
    }

    /// The one Rekor log key this run resolved, when it resolved exactly one.
    ///
    /// The trust-root cache holds a single key per Rekor authority
    /// ([`super::trust_cache::cache_key_for_rekor`]), so a run that legitimately
    /// read entries from two logs has no single answer to write there — and
    /// writing either would hand a later offline verify the wrong one. `None`
    /// then, and the cache keeps whatever it already had.
    fn single_key(&self) -> Option<String> {
        let resolved = self.lock();
        let mut keys = resolved.values();
        match (keys.next(), keys.next()) {
            (Some(only), None) => Some(only.clone()),
            _ => None,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, String>> {
        self.resolved.lock().expect("rekor key memo lock")
    }
}

/// Fetch the Rekor log's published public key PEM (trust-on-first-use, online).
///
/// `pub(crate)` so the auto-verify hook can fetch the key ONCE for a batch and
/// pin it, instead of every covered package re-fetching it inside the pipeline.
pub(crate) async fn fetch_rekor_public_key_pem(rekor_url: &Url) -> Result<String, VerifyErrorKind> {
    let endpoint = rekor_url
        .join("api/v1/log/publicKey")
        .map_err(|e| VerifyErrorKind::Internal(Box::new(e)))?;
    let response = crate::oci::endpoint::sigstore_http_client()
        .get(endpoint)
        .send()
        .await
        .map_err(|_| VerifyErrorKind::TransparencyLogUnavailable)?;
    if !response.status().is_success() {
        return Err(VerifyErrorKind::TransparencyLogUnavailable);
    }
    // Capped: a PEM public key is under a kilobyte.
    let raw = crate::oci::endpoint::read_body_capped(response)
        .await
        .ok_or(VerifyErrorKind::TransparencyLogUnavailable)?;
    String::from_utf8(raw).map_err(|_| VerifyErrorKind::TransparencyLogUnavailable)
}

/// Pull a referrer payload blob with a hard in-memory read cap (CWE-400 defense).
///
/// Reads at most `cap + 1` bytes so an over-cap body is detected and rejected
/// without buffering the whole thing — the pre-download descriptor check bounds
/// the honest case, this bounds a registry that lies about the size. For an
/// honest under-cap blob the native transport's `VerifyingStream` still checks
/// the blob digest at stream end.
///
/// `cap` is always the *run's*, never the candidate's: the bundle path passes
/// its [`VerifyContentMode`] ceiling, the simplesigning path its payload
/// ceiling.
pub(super) async fn pull_blob_capped(
    transport: &dyn OciTransport,
    image: &native::Reference,
    blob_digest: &Digest,
    cap: usize,
) -> Result<Vec<u8>, VerifyErrorKind> {
    use tokio::io::AsyncReadExt as _;
    let reader = transport
        .pull_blob_streaming(image, blob_digest)
        .await
        .map_err(map_client_error)?;
    let mut bytes = Vec::new();
    reader
        .take(cap as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| VerifyErrorKind::BundleParseFailed)?;
    if bytes.len() > cap {
        return Err(VerifyErrorKind::BundleParseFailed);
    }
    Ok(bytes)
}

/// Fetch a referrer manifest, rejecting a body that exceeds the per-manifest
/// cap ([`MAX_REFERRER_MANIFEST_BYTES`]).
///
/// The descriptor size in the referrers listing is untrusted — a registry can
/// advertise a tiny size then return a huge body — so the *actual* body length
/// is the bound that matters, checked after the read. `pull_manifest_raw`
/// verifies the returned body against the referrer digest, so this does not
/// weaken manifest-digest verification. An over-cap body is rejected before it
/// is parsed as JSON.
async fn pull_referrer_manifest_capped(
    transport: &dyn OciTransport,
    referrer_ref: &native::Reference,
) -> Result<Vec<u8>, VerifyErrorKind> {
    let (referrer_bytes, _) = transport
        .pull_manifest_raw(referrer_ref, ACCEPTED_MANIFEST_TYPES)
        .await
        .map_err(map_client_error)?;
    if referrer_bytes.len() as u64 > MAX_REFERRER_MANIFEST_BYTES {
        return Err(VerifyErrorKind::BundleParseFailed);
    }
    Ok(referrer_bytes)
}

/// Whether a referrers-listing `artifactType` names an **unsigned** SBOM
/// attachment.
///
/// Two independent conventions, both measured, and neither is the other's
/// superset:
///
/// * the document's own media type, which is what `oras attach`, `syft attest
///   --output` and OCX's own attach path put on the descriptor; and
/// * [`COSIGN_SBOM_ARTIFACT_TYPE`], which is what `COSIGN_EXPERIMENTAL=1 cosign
///   attach sbom --registry-referrers-mode oci-1-1` puts there while typing the
///   *layer* by the document. Filtering on the first alone dropped every cosign
///   OCI 1.1 SBOM referrer, which is the read half of the same gap the `.sbom`
///   sidecar tag was.
///
/// Admitting a candidate is not accepting it: `read_unverified_layer` still
/// gates on the layer's media type, which is the only claim about the bytes the
/// registry actually served.
fn is_unsigned_sbom_artifact_type(artifact_type: &str) -> bool {
    sbom_predicate_type_uri(artifact_type).is_some() || artifact_type == COSIGN_SBOM_ARTIFACT_TYPE
}

/// Fetch the manifest behind the `sha256-<hex>.sbom` sidecar tag, or `None`
/// when the subject carries no such tag.
///
/// Returns the digest beside the bytes because this door is addressed by *tag*:
/// the caller has no descriptor, so the only name it can give the attachment —
/// in a listing row or in a refusal — is the one the registry answers with.
///
/// Bounded exactly as `simplesigning_read::read_sidecar_tag` bounds the `.sig`
/// door: an over-cap body is rejected before it is parsed, and a 404 is not an
/// error.
async fn pull_sbom_sidecar_manifest(
    transport: &dyn OciTransport,
    image: &native::Reference,
    subject_digest: &Digest,
) -> Result<Option<(Vec<u8>, Digest)>, VerifyErrorKind> {
    let target = sibling_tag_reference(image, crate::package::tag::sbom_sidecar_tag(subject_digest));
    let (bytes, digest) = match transport.pull_manifest_raw(&target, ACCEPTED_MANIFEST_TYPES).await {
        Ok(answer) => answer,
        Err(ClientError::ManifestNotFound(_)) => return Ok(None),
        Err(other) => return Err(map_client_error(other)),
    };
    if bytes.len() as u64 > MAX_REFERRER_MANIFEST_BYTES {
        return Err(VerifyErrorKind::BundleParseFailed);
    }
    let digest = Digest::try_from(digest.as_str()).map_err(|e| VerifyErrorKind::Internal(Box::new(e)))?;
    Ok(Some((bytes, digest)))
}

/// What one candidate turned out to be, once fetched and parsed.
///
/// Three outcomes rather than `Result<VerifyResult, _>`, because the scan must
/// account for them differently and only this function knows which is which: a
/// candidate of the other content kind is not this run's question at all, and a
/// candidate of the wrong predicate type is a narrowing miss rather than a
/// defect. Collapsing either into an error is what let attestations crowd a
/// signature out of the scan, and what would report a healthy artifact's missing
/// SBOM as a data error.
#[derive(Debug)]
// `Verified` is the success outcome, not a rare one: boxing it would move an
// allocation onto the path that matters to save stack on two payload-free
// variants. It is constructed at most `caps.candidates` times per run (32),
// destructured immediately, and never crosses a task boundary.
#[expect(
    clippy::large_enum_variant,
    reason = "the large variant is the hot one; see the note above"
)]
enum CandidateOutcome {
    /// Verified in the requested mode. `attestation` is `Some` iff the mode was
    /// [`VerifyContentMode::Attestation`].
    Verified {
        verified: VerifiedSignature,
        attestation: Option<VerifiedAttestation>,
    },
    /// The bundle carries the other content kind. Costs bytes (it had to be
    /// fetched to be discriminated — annotations are hints, never authoritative)
    /// but never a candidate slot.
    ModeMismatch,
    /// Verified, but its signed predicateType is not the one requested. Leaves
    /// the scan reporting not-found rather than a data error: nothing is wrong
    /// with the artifact, it just does not carry that document.
    TypeNarrowed,
}

/// What the blocking structural pass produced, before any crypto has run.
///
/// The two skip variants mirror [`CandidateOutcome`]'s and are `Ok` outcomes the
/// scan charges differently, so they travel back as values rather than errors —
/// they are answers about which question this candidate belongs to, not
/// failures. Kept separate from `CandidateOutcome` because that type's success
/// variant carries the verification result, which does not exist yet here.
enum ParsedCandidate {
    /// Everything the crypto tail needs, moved across the task boundary once.
    ///
    /// Boxed because the skip variants below are the *common* case in exactly
    /// the crowded scan `order_candidates` exists for: unboxed, every
    /// mode-mismatched candidate would move ~500 bytes of enum to say one word.
    /// The ready path already owns megabytes, so the allocation is free there.
    Ready(Box<ParsedBundle>),
    /// The bundle carries the other content kind.
    ModeMismatch,
    /// Structurally sound, but not the predicate type that was requested.
    TypeNarrowed,
}

/// The material one candidate's structural pass hands to the crypto tail.
struct ParsedBundle {
    bundle: Bundle,
    parts: BundleParts,
    envelope: Option<VerifiedEnvelope>,
}

/// Why a scan stopped before running out of candidates.
/// What a scan produced: what verified, and what was examined and refused.
///
/// The `Option<VerifiedAttestation>` is populated only in attestation mode; the
/// signature path ignores it. Both entry points reshape this into their own
/// public return before it leaves the module.
#[derive(Debug, Default)]
struct ScanOutcome {
    matches: Vec<(VerifyResult, Option<VerifiedAttestation>)>,
    refused: Vec<RefusedCandidate>,
}

/// The subject one scan runs against, resolved once.
///
/// The pair travels together because they are two halves of one answer: the
/// digest names *what* is being read and the reference names *where from*,
/// after index indirection and the mirror map have both had their say. Split
/// across two arguments they would be one transposition away from addressing
/// the right host for the wrong subject.
/// Which of the two referrer *shapes* a candidate is, decided from its
/// `artifactType` before any fetch.
///
/// Shape only — deliberately not the predicate type. The listing's
/// `artifactType` is a registry-served echo of what the referrer claims, and
/// nothing checks it against the manifest it points at, so it may say which
/// decode to run but must never decide the label the decode's result is
/// reported under. That comes from the layer, in
/// [`Self::read_unverified_referrer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnverifiedPayload {
    /// The document itself, typed by the layer's own media type.
    Raw,
    /// A Sigstore bundle, read for its DSSE payload with nothing verified.
    Bundle,
}

struct ScanTarget {
    /// Registry reference every referrer-facing call is addressed with.
    image: native::Reference,
    /// The per-platform subject manifest digest.
    subject_digest: Digest,
    /// The image-index digest [`subject_digest`](Self::subject_digest) was
    /// reached **through**, when `--platform` narrowed into one.
    ///
    /// `None` in both cases where membership cannot be proved: the reference
    /// resolved straight to the acted-on object (a bare platform digest, or no
    /// `--platform` at all), and an index that could not be fetched
    /// (`OCX_OFFLINE`, absent from the cache). Both fail **closed** — see
    /// [`Self::index_signature_subject`].
    enclosing_index: Option<Digest>,
    /// Every digest the enclosing index lists. Empty when there is no index.
    ///
    /// The containment test reads this and nothing else. It is deliberately not
    /// derived from the selection that produced `subject_digest`: `resolve_sign_target`
    /// *selects*, and its own contract says it makes no validity decision, so a
    /// trust gate that inferred membership from "the selector picked it" would
    /// have no check in it at all.
    index_members: Vec<Digest>,
}

impl ScanTarget {
    /// **C-008.** The digest whose signatures may also count for this subject,
    /// or `None` when they may not.
    ///
    /// `cosign verify <tag>` resolves to the *index* digest and signs there,
    /// while OCX pins a *platform manifest*. The index digest binds every
    /// descriptor the index lists, so a signature over the index covers each
    /// child it names — but only once that containment is **proved**, by
    /// finding this subject among [`index_members`](Self::index_members).
    ///
    /// Fails closed twice over, and neither case may ever read as "assume
    /// membership": no enclosing index (nothing was narrowed, or the index was
    /// unfetchable) returns `None`, and a subject the index does not list
    /// returns `None` even when the index digest is known.
    fn index_signature_subject(&self) -> Option<&Digest> {
        let enclosing = self.enclosing_index.as_ref()?;
        self.index_members.contains(&self.subject_digest).then_some(enclosing)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ScanStop {
    /// The per-mode candidate cap was reached.
    CandidateCap,
    /// The cross-candidate byte budget was spent.
    ByteBudget,
    /// The hard backstop on listing iteration was reached.
    ListingCap,
}

/// Candidate-budget accounting for one scan.
///
/// Split out of the loop because the property that matters most here — a
/// candidate of the *other* content kind never consumes the requested mode's
/// slot — has no other seam: driving it through the pipeline needs a registry,
/// and the regression it prevents (attestations crowding out a signature) is
/// silent.
struct ScanBudget {
    caps: ContentCaps,
    /// Candidates examined **in the requested mode**. This is what the
    /// candidate cap bounds.
    examined: usize,
    /// Candidates the loop processed at all, mode-mismatched ones included.
    /// This is what "were any left unlooked-at" is answered from.
    considered: usize,
    /// Bytes actually read across candidates, never a declared size.
    spent: u64,
    /// Set the first time a bound stopped the scan.
    stop: Option<ScanStop>,
}

impl ScanBudget {
    fn new(caps: ContentCaps) -> Self {
        Self {
            caps,
            examined: 0,
            considered: 0,
            spent: 0,
            stop: None,
        }
    }

    /// Whether another candidate may be fetched, recording why not when not.
    ///
    /// The byte budget is charged from bytes actually read, never a declared
    /// size, so a registry cannot buy extra fetches by advertising size 0.
    ///
    /// One allowance, spent by one pass: an attestation run either verifies
    /// (and then never fetches an unsigned attachment at all) or verifies
    /// nothing (and then reads both referrer kinds in a single pass). Nothing
    /// is rationed between passes, because there is only ever one that fetches.
    fn may_examine(&mut self) -> bool {
        match self.stop_reason() {
            Some(stop) => {
                self.stop = Some(stop);
                false
            }
            None => true,
        }
    }

    /// The same three bounds, asked **without** recording an answer.
    ///
    /// A caller that only wants to know whether a door is still affordable must
    /// use this: [`Self::may_examine`] *stamps* `stop`, and a stamp is what
    /// `finish_scan` reads as "this scan was truncated". Probing with the
    /// recording form turns a run that merely declined to open the `.att` door
    /// into one reporting a truncation with `unexamined == 0`, masking the
    /// actionable per-candidate verdict behind it — on subjects carrying no
    /// `.att` tag at all.
    fn stop_reason(&self) -> Option<ScanStop> {
        if self.examined >= self.caps.candidates {
            Some(ScanStop::CandidateCap)
        } else if self.spent >= self.caps.total_bytes {
            Some(ScanStop::ByteBudget)
        } else if self.considered >= MAX_REFERRER_LISTING_ITERATION {
            Some(ScanStop::ListingCap)
        } else {
            None
        }
    }

    /// Record a bound that stopped a *nested* scan, first one wins.
    ///
    /// The `.att` reader walks its own layers under its own caps, so the bound
    /// that stopped it is not one this budget's counters can ever reach. Without
    /// this, `finish_scan`'s fail-closed attestation arm cannot fire and a
    /// truncated sidecar scan returns its partial list as success.
    fn record_stop(&mut self, stop: ScanStop) {
        self.stop.get_or_insert(stop);
    }

    /// Charge bytes actually read to the cross-candidate budget.
    fn charge(&mut self, bytes: u64) {
        self.spent = self.spent.saturating_add(bytes);
    }

    /// Record a candidate examined in the requested mode.
    fn examined(&mut self) {
        self.examined = self.examined.saturating_add(1);
        self.considered = self.considered.saturating_add(1);
    }

    /// Record a candidate that turned out to carry the other content kind.
    ///
    /// Deliberately does **not** touch [`Self::examined`]: attaching five
    /// attestations and re-running attest a few times would otherwise push a
    /// correctly signed artifact's signature past the candidate cap in the
    /// registry's listing order, and `ocx package verify` would report
    /// `NoSignaturesFound` for it.
    fn skipped_other_mode(&mut self) {
        self.considered = self.considered.saturating_add(1);
    }
}

/// Put the candidate list in the order the scan walks it.
///
/// Two keys, applied as two stable sorts so the result is lexicographic in
/// (disagrees-with-mode, digest):
///
/// 1. **Digest**, so the passing candidate and the aggregate error are
///    reproducible regardless of registry listing order. No trust
///    significance — it is a total order the registry does not choose.
/// 2. **The `dev.sigstore.bundle.content` hint**, which demotes a candidate
///    that positively names the *other* content kind to the tail. Without it a
///    subject carrying nine SBOMs pushes its signature past
///    [`MAX_SIGNATURE_CANDIDATES`] in digest order, and the run reports an
///    honestly-signed artifact as unsigned: the slot-free `ModeMismatch` skip
///    is only reachable *after* the bundle parses, so a candidate refused by
///    the per-mode size gate before that spends a slot on the wrong kind.
///
/// The hint is producer-controlled and untrusted, which is why this orders and
/// never filters: every candidate stays in the list, a demoted one is still
/// fetched and discriminated on its bundle bytes when the scan reaches it, and a
/// candidate carrying no hint keeps its digest position. The `&mut [_]`
/// signature is what enforces that half — a slice cannot change length, so
/// dropping a candidate here is a compile error rather than a review question.
///
/// Whether the scan reaches a tail candidate is the caps' decision, not this
/// function's: the candidate slots, the cross-candidate byte budget and the
/// per-mode declared-size gate all still bite there. Demotion changes the order
/// in which those bounds are met; it never makes a candidate ineligible.
fn order_candidates(candidates: &mut [crate::oci::Descriptor], mode: &VerifyContentMode) {
    candidates.sort_by(|a, b| a.digest.cmp(&b.digest));
    candidates.sort_by_key(|descriptor| annotation_disagrees_with_mode(descriptor, mode));
}

/// Whether a listed referrer's annotations positively name something other than
/// what this run is looking for.
///
/// Two annotations, because since D2 it takes both to tell the kinds apart:
///
/// 1. `dev.sigstore.bundle.content` — **mode-independent** now. Both a cosign
///    image signature and an attestation are `dsse-envelope`, so a hint naming
///    anything else (the pre-parity `message-signature`, or a value from a tool
///    nobody here knows) names a shape neither mode reads.
/// 2. `dev.sigstore.bundle.predicateType` — what actually separates the two.
///    Signature mode demotes every predicateType but cosign's image-signature
///    one; attestation mode demotes exactly that one.
///
/// Without the second rule the availability defect this ordering exists to
/// close would reopen the moment signatures became DSSE: nine SBOM referrers
/// all hinting `dsse-envelope` would sort ahead of the signature by digest and
/// exhaust the scan before it is reached.
///
/// Absent annotation → `false` on both rules: a referrer pushed by a tool that
/// writes no hint, or a transport that does not echo listing annotations, must
/// not be demoted behind one that does.
fn annotation_disagrees_with_mode(descriptor: &crate::oci::Descriptor, mode: &VerifyContentMode) -> bool {
    let Some(annotations) = descriptor.annotations.as_ref() else {
        return false;
    };
    if annotations
        .get(ANNOTATION_BUNDLE_CONTENT)
        .is_some_and(|hint| hint != BUNDLE_CONTENT_DSSE)
    {
        return true;
    }
    annotations
        .get(ANNOTATION_BUNDLE_PREDICATE_TYPE)
        .is_some_and(|hint| match mode {
            VerifyContentMode::Signature => hint != COSIGN_SIGN_PREDICATE_TYPE,
            VerifyContentMode::Attestation { .. } => hint == COSIGN_SIGN_PREDICATE_TYPE,
        })
}

/// Name the bound that stopped a scan, for a caller reporting the truncation.
///
/// A truncated scan cannot answer a question about *every* SBOM a subject
/// carries, so which bound ran out is the actionable part. Shared by both
/// passes; how each one reports it differs — see
/// [`VerifyPipeline::scan_unverified`].
fn truncation_failure(caps: ContentCaps, stop: ScanStop, unexamined: usize) -> VerifyErrorKind {
    match stop {
        ScanStop::CandidateCap => VerifyErrorKind::TooManyAttestations { limit: caps.candidates },
        ScanStop::ByteBudget => VerifyErrorKind::AttestationBudgetExhausted {
            limit: caps.total_bytes,
        },
        ScanStop::ListingCap => VerifyErrorKind::CandidateLimitExhausted { unexamined },
    }
}

/// Pick the most actionable failure across the refused candidates (see
/// [`failure_rank`]), consuming them — every caller of this is on a path that
/// returns `Err` and reports nothing else.
fn best_failure(refused: Vec<RefusedCandidate>) -> Option<VerifyErrorKind> {
    // `min_by_key` returns the *first* minimum, so reversing the rank picks the
    // first strict maximum — the listing-order tiebreak the incremental fold it
    // replaced had. `max_by_key` returns the last, which would silently reorder
    // which of two equally-ranked refusals a caller is shown.
    refused
        .into_iter()
        .min_by_key(|candidate| std::cmp::Reverse(failure_rank(&candidate.reason)))
        .map(|candidate| candidate.reason)
}

/// Decide the aggregate ANY-of failure once no candidate has passed.
///
/// If the candidate cap or byte budget left candidates unexamined, report the
/// limit distinctly ([`VerifyErrorKind::CandidateLimitExhausted`]) — the
/// candidate order is by digest (no trust significance), so a valid signature
/// may sort past the cap, and surfacing an examined candidate's error would
/// misattribute the failure. Otherwise surface the most actionable examined
/// failure, or [`VerifyErrorKind::NoSignaturesFound`] when none was recorded.
fn aggregate_failure(total: usize, examined: usize, best: Option<VerifyErrorKind>) -> VerifyErrorKind {
    let unexamined = total.saturating_sub(examined);
    if unexamined > 0 {
        return VerifyErrorKind::CandidateLimitExhausted { unexamined };
    }
    best.unwrap_or(VerifyErrorKind::NoSignaturesFound)
}

/// Rank verify failures so the aggregate error across candidate referrers
/// surfaces the most actionable one: a real signature failing identity beats an
/// unrelated malformed referrer. Higher = more meaningful.
fn failure_rank(kind: &VerifyErrorKind) -> u8 {
    match kind {
        VerifyErrorKind::IdentityMismatch | VerifyErrorKind::IssuerMismatch => 5,
        VerifyErrorKind::SignatureInvalid
        | VerifyErrorKind::CertChainInvalid
        | VerifyErrorKind::RekorSetInvalid
        | VerifyErrorKind::TransparencyBodyMismatch => 4,
        VerifyErrorKind::TransparencyLogUnavailable | VerifyErrorKind::RekorSetAbsentTsaPresent => 3,
        VerifyErrorKind::BundleParseFailed | VerifyErrorKind::NoUsableBundle => 2,
        _ => 1,
    }
}

/// Cache the trust material a **simplesigning** verify used, at the exits the
/// bundle path caches at (#374).
///
/// The bundle path has the PEM in hand when a candidate verifies and caches it
/// right there. A sidecar layer resolves its log key several frames down, inside
/// `simplesigning_read::logged_entry`, and nothing it returns carries the key
/// back out — so the run's memo is what the key is read from instead.
///
/// Silent on a run that resolved no key at all (a key-mode sidecar uploads
/// nothing to Rekor, so there is none) and on one that resolved two — see
/// [`RekorKeyMemo::single_key`] for why the second case must not guess.
async fn cache_sidecar_trust_material(ctx: &VerifyContext<'_>, rekor_keys: &RekorKeyMemo) {
    if ctx.offline {
        return;
    }
    if let Some(pem) = rekor_keys.single_key() {
        cache_trust_material(ctx, pem).await;
    }
}

/// Cache the trust material of a successful online verify, skipping the write
/// when a fresh entry already holds identical bytes.
///
/// The content-equal skip avoids sliding the 24h TTL on every use and stops N
/// concurrent batch verifies from each rewriting the same file. Best-effort: a
/// cache-write failure never fails a valid verify.
async fn cache_trust_material(ctx: &VerifyContext<'_>, rekor_key_pem: String) {
    let cache_key = super::trust_cache::cache_key_for_rekor(ctx.rekor_url);
    let der_certs = ctx.trust_root.der_certs().to_vec();
    // The CT log keys travel with the anchors: without them a cache-loaded
    // trust root cannot check the SCT, so an offline verify off this entry
    // would fail where the online one that wrote it succeeded.
    let ctfe_keys = ctx.trust_root.ctfe_key_map().clone();

    // A fresh, content-equal entry needs no rewrite — leave its TTL alone.
    if let Ok(Some(existing)) = TrustRootCache::from_cache(&cache_key, ctx.state).await
        && existing.fulcio_der_certs == der_certs
        && existing.ctfe_keys == ctfe_keys
        && existing.rekor_public_key_pem.as_deref() == Some(rekor_key_pem.as_str())
    {
        return;
    }

    let entry = TrustRootCache::new(cache_key, der_certs, ctfe_keys, rekor_key_pem);
    if let Err(e) = entry.write_cache(ctx.state).await {
        tracing::debug!("trust-root cache write skipped: {e}");
    }
}

/// Fetch the subject manifest and prove it hashes to the digest the index
/// resolved.
///
/// `sigstore`'s verifier takes a preimage, not a digest, so verification needs
/// the manifest bytes. Re-hashing them here is what makes that safe: the bytes
/// fed to the verifier are the bytes the registry served under a digest we
/// independently resolved, so a registry cannot swap in a different artifact and
/// have the signature check pass over the digest it prefers. Bounded by the same
/// per-manifest cap as a referrer manifest.
async fn pull_subject_manifest_verified(
    transport: &dyn OciTransport,
    image: &native::Reference,
    subject_digest: &Digest,
) -> Result<Vec<u8>, VerifyErrorKind> {
    // Digest-addressed: `clone_with_digest` drops the tag, so this reads the
    // exact manifest the index resolved rather than whatever the tag points at
    // now.
    let pinned = image.clone_with_digest(subject_digest.to_string());
    let (bytes, _) = transport
        .pull_manifest_raw(&pinned, ACCEPTED_MANIFEST_TYPES)
        .await
        .map_err(map_client_error)?;
    // No size cap here, deliberately: the transport has already allocated the
    // body by the time this runs, so a post-hoc length check prevents no
    // allocation -- it would only refuse a genuine oversized manifest, since a
    // forged one fails the digest check on the next line regardless. Bounding
    // the read itself belongs in the transport.
    let actual = subject_digest.algorithm().hash(&bytes);
    if !actual.hex().eq_ignore_ascii_case(subject_digest.hex()) {
        return Err(VerifyErrorKind::SubjectDigestMismatch);
    }
    Ok(bytes)
}

/// A `sigstore` verification policy that accepts every certificate, because ocx
/// enforces identity itself in [`matching_policies`].
///
/// Not a hole: `matching_policies` runs unconditionally on the same leaf a few
/// lines after `verify_digest` returns, and it is the richer check — `[[trust.policy]]`
/// supports regex identities and ANY-of matching, and its verdict carries the
/// distinction between a wrong identity and a wrong issuer that the exit-code
/// contract exposes as 77. `sigstore`'s `PolicyError` cannot express either, so
/// delegating identity here would flatten two user-visible outcomes into one.
pub(super) struct PolicyDeferredToOcx;

impl VerificationPolicy for PolicyDeferredToOcx {
    fn verify(&self, _cert: &x509_cert::Certificate) -> PolicyResult {
        Ok(())
    }
}

/// Map a `sigstore` verification failure into the ocx verify taxonomy.
///
/// The mapping preserves the exit-code contract: every certificate-side failure
/// is 65 via [`VerifyErrorKind::CertChainInvalid`], a tlog-body inconsistency
/// keeps its own 65 variant, and a signature failure stays
/// [`VerifyErrorKind::SignatureInvalid`].
pub(super) fn map_verification_error(error: sigstore::bundle::verify::VerificationError) -> VerifyErrorKind {
    use sigstore::bundle::verify::VerificationError as E;
    match error {
        E::Bundle(_) => VerifyErrorKind::BundleParseFailed,
        E::Certificate(_) => VerifyErrorKind::CertChainInvalid,
        // Covers both a failed signature check and a tlog body that does not
        // rebuild from this bundle (the GHSA-whqx splice). `sigstore` models the
        // two as one enum whose payload type it does not export, so they cannot
        // be told apart here; both are exit 65 either way, and the second now
        // reports as `signature_invalid` rather than
        // `transparency_body_mismatch`.
        E::Signature(_) => VerifyErrorKind::SignatureInvalid,
        // Unreachable: `PolicyDeferredToOcx` never rejects, and the input is a
        // slice rather than a reader, so no I/O can fail here.
        E::Policy(_) | E::Input(_) => VerifyErrorKind::Internal(Box::new(error)),
    }
}

/// How a `--platform` request reads in an error message when none was made.
///
/// C-010 makes the flag optional, so "no manifest for platform " with nothing
/// after it is now reachable; `any` is what the absence means — act on whatever
/// resolved.
fn platform_label(platform: Option<&Platform>) -> String {
    platform.map_or_else(|| "any".to_string(), Platform::to_string)
}

/// Map the shared `--platform` decision's refusals into the verify taxonomy.
///
/// `PlatformNotFound` and `AmbiguousPlatform` land on [`VerifyErrorKind::TargetNotFound`],
/// which is where the pre-C-010 `Index::select` refusals landed too — the
/// message and the exit code are unchanged for them.
fn map_resolve_target_error(error: ResolveTargetError) -> VerifyErrorKind {
    match error {
        ResolveTargetError::NotAnIndex { platform } => VerifyErrorKind::TargetNotAnIndex { platform },
        ResolveTargetError::PlatformNotFound { platform } | ResolveTargetError::AmbiguousPlatform { platform } => {
            VerifyErrorKind::TargetNotFound { platform }
        }
    }
}

/// Map an OCI client error into the verify taxonomy.
pub(super) fn map_client_error(error: ClientError) -> VerifyErrorKind {
    match error {
        // Not deleted, mapped: the arm below ends in `other => Internal(..)`, so
        // dropping this one would silently reclassify a surviving client-layer
        // verdict to exit 1 instead of failing to compile. `verify` reaches this
        // only through a path that does not fall back to the tag schema, and the
        // read-path answer to "this registry has no referrers for the subject" is
        // 79 / `no_signatures_found` (D3) — 84 is now write-path only.
        ClientError::ReferrersUnsupported { .. } => VerifyErrorKind::NoSignaturesFound,
        ClientError::ManifestNotFound(_) | ClientError::BlobNotFound { .. } => VerifyErrorKind::NoSignaturesFound,
        // A registry that serves a spec-violating image index where a signature
        // artifact was expected is malformed signature data, not an ocx defect:
        // exit 65, not the catch-all's exit 1.
        ClientError::InvalidImageIndex(_) => VerifyErrorKind::BundleParseFailed,
        other => VerifyErrorKind::Internal(Box::new(other)),
    }
}

#[cfg(test)]
mod tests {
    //! Unit coverage for the pure, deterministic pipeline helpers — the
    //! fail-closed edges the acceptance suite (`test/tests/test_verify.py`)
    //! does not isolate. The end-to-end matching/tamper/mismatch behaviour is
    //! validated there against real Fulcio-minted certs and the fake stack.
    use super::*;
    use crate::cli::{ClassifyErrorKind, ExitCode, classify_error};
    use p256::ecdsa::SigningKey;
    use p256::elliptic_curve::rand_core::OsRng;
    use sigstore_protobuf_specs::dev::sigstore::bundle::v1::Bundle;
    use sigstore_protobuf_specs::dev::sigstore::common::v1::{
        HashAlgorithm, HashOutput, LogId, MessageSignature, X509Certificate, X509CertificateChain,
    };
    use sigstore_protobuf_specs::dev::sigstore::rekor::v1::{InclusionPromise, TransparencyLogEntry};

    fn verify_id() -> Identifier {
        Identifier::parse("registry.example/pkg:1.0").expect("parse test identifier")
    }

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
            let err = VerifyError::new(verify_id(), map_client_error(client_error));
            assert_eq!(classify_error(&err), expected, "client error: {rendered}");
        }
    }

    /// The other half of the deferral contract: an `Internal` whose cause no
    /// classifier recognizes must still exit 1, via `classify_error`'s
    /// fall-through rather than an assertion at the wrapper.
    #[test]
    fn unclassifiable_internal_still_exits_failure_through_verify() {
        let kind = VerifyErrorKind::Internal("something no classifier knows".into());
        assert_eq!(classify_error(&VerifyError::new(verify_id(), kind)), ExitCode::Failure);
    }

    /// Generate a self-signed P-256 certificate; return the key and its DER.
    ///
    /// A self-signed cert is its own CA, so a trust root holding it validates
    /// the leaf (matching case), and a trust root holding a *different*
    /// self-signed cert does not (non-matching case).
    fn self_signed_cert() -> (SigningKey, Vec<u8>) {
        use std::str::FromStr;
        use std::time::Duration;
        use x509_cert::builder::{Builder, CertificateBuilder, Profile};
        use x509_cert::der::Encode;
        use x509_cert::name::Name;
        use x509_cert::serial_number::SerialNumber;
        use x509_cert::spki::SubjectPublicKeyInfoOwned;
        use x509_cert::time::Validity;

        let signing_key = SigningKey::random(&mut OsRng);
        let verifying_key = *signing_key.verifying_key();
        let spki = SubjectPublicKeyInfoOwned::from_key(verifying_key).expect("spki");
        let builder = CertificateBuilder::new(
            Profile::Root,
            SerialNumber::from(1u32),
            Validity::from_now(Duration::from_secs(3600)).expect("validity"),
            Name::from_str("CN=ocx-test").expect("name"),
            spki,
            &signing_key,
        )
        .expect("builder");
        let cert = builder.build::<p256::ecdsa::DerSignature>().expect("build");
        (signing_key, cert.to_der().expect("der"))
    }

    /// A trust root carrying the supplied CAs plus a placeholder CT log key.
    ///
    /// The key is never used: every test built on this helper asserts on routing
    /// or on a failure raised before signature verification. It is present only
    /// because the pipeline refuses a CT-keyless trust root up front, and that
    /// refusal would otherwise pre-empt the routing these tests exist to pin.
    fn trust_root_of(certs: &[&[u8]]) -> TrustRoot {
        TrustRoot::from_material(
            certs.iter().map(|der| der.to_vec()).collect(),
            std::collections::BTreeMap::from([("test-ct".to_string(), vec![0x30, 0x00])]),
            std::collections::BTreeMap::new(),
        )
    }

    fn message_bundle(with_material: bool, with_tlog: bool) -> Bundle {
        message_bundle_with(with_material, with_tlog, true)
    }

    fn message_bundle_with(with_material: bool, with_tlog: bool, with_proof: bool) -> Bundle {
        use sigstore_protobuf_specs::dev::sigstore::bundle::v1::{VerificationMaterial, bundle, verification_material};
        use sigstore_protobuf_specs::dev::sigstore::rekor::v1::Checkpoint;
        let message = MessageSignature {
            message_digest: Some(HashOutput {
                algorithm: HashAlgorithm::Sha2256 as i32,
                digest: vec![1; 32],
            }),
            signature: vec![2, 3, 4],
        };
        let material = with_material.then(|| VerificationMaterial {
            timestamp_verification_data: None,
            tlog_entries: with_tlog
                .then(|| TransparencyLogEntry {
                    log_index: 5,
                    log_id: Some(LogId { key_id: vec![0xab] }),
                    kind_version: None,
                    integrated_time: 100,
                    inclusion_promise: Some(InclusionPromise {
                        signed_entry_timestamp: vec![9, 9, 9],
                    }),
                    inclusion_proof: with_proof.then(|| ProtoInclusionProof {
                        log_index: 5,
                        root_hash: vec![0xaa],
                        tree_size: 8,
                        hashes: vec![vec![0xbb], vec![0xcc]],
                        checkpoint: Some(Checkpoint {
                            envelope: "envelope".into(),
                        }),
                    }),
                    canonicalized_body: b"{}".to_vec(),
                })
                .into_iter()
                .collect(),
            content: Some(verification_material::Content::X509CertificateChain(
                X509CertificateChain {
                    certificates: vec![X509Certificate {
                        raw_bytes: vec![0x30, 0x00],
                    }],
                },
            )),
        });
        Bundle {
            media_type: crate::oci::sign::bundle::BUNDLE_V03_MEDIA_TYPE.to_string(),
            verification_material: material,
            content: Some(bundle::Content::MessageSignature(message)),
        }
    }

    #[test]
    fn from_bundle_requires_verification_material() {
        let bundle = into_signature_bundle(message_bundle(false, false));
        assert!(matches!(
            BundleParts::from_bundle(&bundle, &VerifyContentMode::Signature),
            Err(VerifyErrorKind::BundleParseFailed)
        ));
    }

    #[test]
    fn from_bundle_requires_a_tlog_entry() {
        let bundle = into_signature_bundle(message_bundle(true, false));
        assert!(matches!(
            BundleParts::from_bundle(&bundle, &VerifyContentMode::Signature),
            Err(VerifyErrorKind::RekorSetInvalid)
        ));
    }

    fn dsse_bundle() -> Bundle {
        use sigstore_protobuf_specs::dev::sigstore::bundle::v1::bundle;
        use sigstore_protobuf_specs::io::intoto::Envelope;
        let mut bundle = message_bundle(true, true);
        bundle.content = Some(bundle::Content::DsseEnvelope(Envelope {
            payload: Vec::new(),
            payload_type: String::new(),
            signatures: Vec::new(),
        }));
        bundle
    }

    /// Re-content a bundle as the DSSE envelope cosign v3 writes for an **image
    /// signature**: an in-toto Statement whose `predicateType` is the
    /// image-signature one and whose predicate is empty (F4).
    ///
    /// This is the shape signature mode reads since D2, and it is what makes
    /// [`dsse_bundle`] above discriminable *as an attestation*: the two differ
    /// only in the predicateType their payload declares, which is exactly the
    /// distinction `from_bundle` now routes on.
    fn into_signature_bundle(mut bundle: Bundle) -> Bundle {
        use sigstore_protobuf_specs::dev::sigstore::bundle::v1::bundle;
        use sigstore_protobuf_specs::io::intoto::{Envelope, Signature};
        let statement = format!(
            r#"{{"_type":"https://in-toto.io/Statement/v1","subject":[{{"digest":{{"sha256":"{hex}"}}}}],"predicateType":"{COSIGN_SIGN_PREDICATE_TYPE}","predicate":{{}}}}"#,
            hex = "11".repeat(32),
        );
        bundle.content = Some(bundle::Content::DsseEnvelope(Envelope {
            payload: statement.into_bytes(),
            payload_type: crate::oci::attest::DSSE_PAYLOAD_TYPE.to_string(),
            signatures: vec![Signature {
                sig: vec![0xDE, 0xAD, 0xBE, 0xEF],
                keyid: String::new(),
            }],
        }));
        bundle
    }

    /// The whole mode/content/predicateType matrix, because a gate that only
    /// ever sees one cell is indistinguishable from no gate.
    ///
    /// Since D2 both kinds are DSSE envelopes, so the discriminator is the
    /// Statement's predicateType: cosign's image-signature one answers the
    /// signature question, every other one answers the attestation question.
    /// Each side asserts the accept *and* the skip, so hard-wiring either
    /// answer reds — and the `messageSignature` row is asserted in **both**
    /// modes, because the pre-parity shape is now read by neither.
    #[test]
    fn bundle_content_must_match_requested_mode() {
        let signature_bundle = into_signature_bundle(message_bundle(true, true));
        let attestation_bundle = dsse_bundle();
        let message_bundle = message_bundle(true, true);
        let attestation_mode = VerifyContentMode::Attestation { predicate_type: None };

        assert!(
            BundleParts::from_bundle(&signature_bundle, &VerifyContentMode::Signature).is_ok(),
            "signature mode must accept a cosign image-signature DSSE"
        );
        assert!(
            BundleParts::from_bundle(&attestation_bundle, &attestation_mode).is_ok(),
            "attestation mode must accept an attestation DSSE"
        );
        assert!(
            matches!(
                BundleParts::from_bundle(&attestation_bundle, &VerifyContentMode::Signature),
                Err(VerifyErrorKind::NoUsableBundle)
            ),
            "signature mode must skip an attestation DSSE"
        );
        assert!(
            matches!(
                BundleParts::from_bundle(&signature_bundle, &attestation_mode),
                Err(VerifyErrorKind::NoUsableBundle)
            ),
            "attestation mode must skip a cosign image-signature DSSE — otherwise a plain \
             `--attestation` run would match an image signature"
        );
        for mode in [&VerifyContentMode::Signature, &attestation_mode] {
            assert!(
                matches!(
                    BundleParts::from_bundle(&message_bundle, mode),
                    Err(VerifyErrorKind::NoUsableBundle)
                ),
                "a messageSignature bundle is refused in {mode:?}: nothing on this path reads it",
            );
        }
    }

    /// Literals, not the constants themselves: a test spelled in terms of
    /// `MAX_BUNDLE_SIZE_BYTES` passes no matter what that constant becomes, and
    /// these three numbers are what `ocx package verify` has always enforced.
    #[test]
    fn signature_mode_caps_are_the_shipped_numbers() {
        let caps = VerifyContentMode::Signature.caps();
        assert_eq!(caps.bundle_bytes, 512 * 1024);
        assert_eq!(caps.candidates, 8);
        assert_eq!(caps.total_bytes, 4 * 1024 * 1024);
    }

    #[test]
    fn attestation_mode_caps_are_the_attestation_numbers() {
        let caps = VerifyContentMode::Attestation { predicate_type: None }.caps();
        assert_eq!(caps.bundle_bytes, 32 * 1024 * 1024);
        assert_eq!(caps.candidates, 32);
        assert_eq!(caps.total_bytes, 64 * 1024 * 1024);
    }

    /// S-016. One bundle size, two verdicts: the pair is what proves the caps
    /// are actually selected per mode rather than shared. Hoisting the larger
    /// constant into the signature path reds the first assertion; leaving the
    /// smaller one in the attestation path reds the second.
    #[test]
    fn a_one_mebibyte_bundle_is_over_cap_for_signature_and_under_cap_for_attestation() {
        const ONE_MEBIBYTE: usize = 1024 * 1024;
        assert!(
            ONE_MEBIBYTE > VerifyContentMode::Signature.caps().bundle_bytes,
            "a 1 MiB bundle must be rejected in signature mode"
        );
        assert!(
            ONE_MEBIBYTE
                <= VerifyContentMode::Attestation { predicate_type: None }
                    .caps()
                    .bundle_bytes,
            "a 1 MiB bundle must be accepted in attestation mode"
        );
    }

    #[test]
    fn from_bundle_requires_a_merkle_inclusion_proof() {
        // A promise-only bundle (legal under bundle profile v0.1/v0.2) carries
        // no evidence that the entry is in a signed tree. ocx runs `sigstore`'s
        // verifier with `offline: true`, whose online counterpart refuses this
        // exact shape — without this refusal ocx would verify on strictly
        // weaker evidence than the branch it is standing in for. Exit 65.
        let bundle = into_signature_bundle(message_bundle_with(true, true, false));
        assert!(
            matches!(
                BundleParts::from_bundle(&bundle, &VerifyContentMode::Signature),
                Err(VerifyErrorKind::RekorInclusionProofAbsent)
            ),
            "a bundle with an inclusion promise but no proof must be refused"
        );
    }

    /// Renamed from `from_bundle_extracts_message_signature_parts`: the parts
    /// come from the tlog entry, which is shared by both content kinds, and
    /// the bundle they are read out of is a cosign DSSE signature now.
    #[test]
    fn from_bundle_extracts_the_tlog_parts() {
        let bundle = into_signature_bundle(message_bundle(true, true));
        let parts = BundleParts::from_bundle(&bundle, &VerifyContentMode::Signature).expect("valid signature bundle");
        let BundleParts::Keyless { tlog, .. } = parts else {
            panic!("a certificate bundle is keyless material");
        };
        assert_eq!(tlog.integrated_time, 100);
        assert_eq!(tlog.log_index, 5);
        assert_eq!(tlog.log_id_hex, "ab");
    }

    #[test]
    fn failure_rank_prefers_identity_over_parse_failure() {
        // The ANY-of aggregate must surface a real-signature identity failure over
        // an unrelated malformed referrer, so a rotation/splice attempt does not
        // hide behind a junk first referrer.
        assert!(failure_rank(&VerifyErrorKind::IdentityMismatch) > failure_rank(&VerifyErrorKind::BundleParseFailed));
        assert!(failure_rank(&VerifyErrorKind::SignatureInvalid) > failure_rank(&VerifyErrorKind::NoUsableBundle));
    }

    #[test]
    fn failure_rank_orders_the_full_severity_ladder() {
        // The aggregate error across candidates must be the highest-severity one,
        // never the first-in-order. Pin the whole monotone ladder so a later edit
        // cannot flatten a middle tier (e.g. let a Rekor-availability failure mask
        // a real signature-tamper failure). Complements
        // `failure_rank_prefers_identity_over_parse_failure`, which only pins the
        // identity-vs-parse endpoints.
        let identity = failure_rank(&VerifyErrorKind::IdentityMismatch);
        let issuer = failure_rank(&VerifyErrorKind::IssuerMismatch);
        let tamper = failure_rank(&VerifyErrorKind::TransparencyBodyMismatch);
        let rekor_avail = failure_rank(&VerifyErrorKind::TransparencyLogUnavailable);
        let parse = failure_rank(&VerifyErrorKind::BundleParseFailed);

        // identity == issuer (both are the "verified, wrong signer" tier).
        assert_eq!(identity, issuer);
        // identity/issuer  >  crypto-tamper  >  service-availability  >  parse.
        assert!(identity > tamper);
        assert!(tamper > rekor_avail);
        assert!(rekor_avail > parse);
        // Every crypto-tamper variant sits in the same tier.
        assert_eq!(tamper, failure_rank(&VerifyErrorKind::SignatureInvalid));
        assert_eq!(tamper, failure_rank(&VerifyErrorKind::CertChainInvalid));
        assert_eq!(tamper, failure_rank(&VerifyErrorKind::RekorSetInvalid));
    }

    /// The cross-candidate byte budget exists to bound total download, and the
    /// bundle blob is the overwhelming majority of that download — a referrer
    /// manifest is a few hundred bytes, a bundle is up to the per-candidate cap.
    /// Charging only manifests capped real spend at candidates x 256 KiB, well
    /// under the budget in either mode, so the `break` could never fire.
    ///
    /// Asserts the running total, not `is_ok`: a charge that lands on only one
    /// of the two paths still passes any pass/fail assertion.
    #[tokio::test]
    async fn every_candidate_charges_its_bundle_blob_to_the_byte_budget() {
        use crate::oci::client::test_transport::{StubTransport, StubTransportData};

        let blob = vec![7u8; 4096];
        let blob_digest = crate::oci::Algorithm::Sha256.hash(&blob);
        // One byte over the cap, so the bounded read rejects it after paying for
        // it — the failure path, which must charge the cap rather than nothing.
        let oversize_digest = crate::oci::Algorithm::Sha256.hash(b"lying-descriptor");
        let data = StubTransportData::new();
        {
            let mut inner = data.write();
            inner.blobs.insert(blob_digest.to_string(), blob.clone());
            inner
                .blobs
                .insert(oversize_digest.to_string(), vec![0u8; MAX_BUNDLE_SIZE_BYTES + 1]);
        }
        let transport = StubTransport::new(data);
        let image: native::Reference = "registry.example/repo:latest".parse().expect("stub reference");
        let subject_digest = crate::oci::Algorithm::Sha256.hash(b"subject");
        // A real CA cert: `Verifier::new` compiles the trust root into a
        // certificate pool and rejects the placeholder DER the routing tests use.
        let ca_der = super::super::tlog::fixture_certificate_der();
        let trust_root = trust_root_of(&[&ca_der]);
        let verifier = Verifier::new(RekorConfiguration::default(), trust_root.clone()).expect("verifier");
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = StateStore::new(temp.path());
        let index = Index::from_impl(IndirectingIndex {
            physical: Identifier::parse("registry.example/repo:1.0").expect("physical identifier"),
        });
        let identifier = Identifier::parse("ocx.sh/acme/tool:1.0").expect("logical identifier");
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let ctx = VerifyContext {
            identifier: &identifier,
            platform: None,
            policies: &[],
            no_cache: true,
            index: &index,
            trust_root: &trust_root,
            rekor_url: &rekor_url,
            state: &state,
            offline: true,
            content: VerifyContentMode::Signature,
            verification: VerificationMode::Demand,
            signature_format: None,
            allow_unlogged_signature: false,
            report_all: false,
        };

        // A well-formed referrer manifest whose one layer names the blob. The
        // blob itself is junk, so the candidate fails at `parse_bundle` — after
        // the fetch, which is the point: the bytes were paid for either way.
        let referrer_of = |digest: &Digest, size: i64| {
            let payload = crate::oci::Descriptor {
                media_type: SIGSTORE_BUNDLE_V03.to_string(),
                digest: digest.to_string(),
                size,
                ..crate::oci::Descriptor::default()
            };
            let subject = crate::oci::Descriptor {
                media_type: crate::oci::OCI_IMAGE_MEDIA_TYPE.to_string(),
                digest: subject_digest.to_string(),
                size: 7,
                ..crate::oci::Descriptor::default()
            };
            let manifest = crate::oci::referrer::ReferrerManifest::build(subject, SIGSTORE_BUNDLE_V03, payload, None);
            let bytes = manifest.to_canonical_json().expect("referrer manifest serializes");
            let descriptor = crate::oci::Descriptor {
                media_type: crate::oci::OCI_IMAGE_MEDIA_TYPE.to_string(),
                digest: crate::oci::Algorithm::Sha256.hash(&bytes).to_string(),
                size: bytes.len() as i64,
                ..crate::oci::Descriptor::default()
            };
            (descriptor, bytes)
        };

        let mut budget = ScanBudget::new(ctx.content.caps());
        let verify_once = async |descriptor: &crate::oci::Descriptor, bytes: Vec<u8>, budget: &mut ScanBudget| {
            VerifyPipeline::verify_one_referrer(
                &transport,
                &ctx,
                &verifier,
                descriptor,
                bytes,
                &subject_digest,
                b"subject",
                &image,
                crate::oci::verify::DiscoveryMethod::ReferrersApi,
                budget,
                &RekorKeyMemo::default(),
            )
            .await
        };

        for candidate in 1..=2u64 {
            let (descriptor, bytes) = referrer_of(&blob_digest, blob.len() as i64);
            let verdict = verify_once(&descriptor, bytes, &mut budget).await;
            assert!(
                matches!(verdict, Err(VerifyErrorKind::BundleParseFailed)),
                "junk blob must fail to parse: {verdict:?}"
            );
            assert_eq!(
                budget.spent,
                candidate * blob.len() as u64,
                "each fetched bundle blob must be charged to the budget"
            );
        }

        // The failure path charges the cap: a registry that lies about the size
        // still costs a bounded read, and charging nothing there would let it
        // repeat for free.
        let (descriptor, bytes) = referrer_of(&oversize_digest, blob.len() as i64);
        let verdict = verify_once(&descriptor, bytes, &mut budget).await;
        assert!(
            matches!(verdict, Err(VerifyErrorKind::BundleParseFailed)),
            "an over-cap blob must be rejected: {verdict:?}"
        );
        assert_eq!(
            budget.spent,
            2 * blob.len() as u64 + MAX_BUNDLE_SIZE_BYTES as u64,
            "a rejected over-cap read must be charged at the cap, not skipped"
        );
    }

    #[tokio::test]
    async fn pull_blob_capped_streams_honest_blob_and_rejects_oversize() {
        // Covers the Wave-B `pull_blob` → `pull_blob_streaming` switch and the
        // CWE-400 bounded read: an honest under-cap bundle streams back intact,
        // while a registry lying about the size (an over-cap body) is rejected by
        // the `.take(MAX + 1)` read without buffering the whole thing. The
        // per-download descriptor pre-check bounds the honest case; THIS bounds the
        // lying registry — so both are exercised here against the stub transport.
        use crate::oci::client::test_transport::{StubTransport, StubTransportData};

        let data = StubTransportData::new();
        let honest = b"a genuine under-cap sigstore bundle payload".to_vec();
        let honest_digest = crate::oci::Algorithm::Sha256.hash(&honest);
        // One byte over the cap: the stub keys blobs by digest string, so the
        // digest need not match the (deliberately oversized) content.
        let oversize = vec![0u8; MAX_BUNDLE_SIZE_BYTES + 1];
        let oversize_digest = crate::oci::Algorithm::Sha256.hash(b"lying-descriptor");
        {
            let mut inner = data.write();
            inner.blobs.insert(honest_digest.to_string(), honest.clone());
            inner.blobs.insert(oversize_digest.to_string(), oversize);
        }
        let transport = StubTransport::new(data);
        // Parsed, not direct-constructed: this fixture only needs a well-formed
        // reference to key the stub, and T-arch-G1 reserves the direct
        // constructors for `oci/client.rs` (it scans source text, so even
        // naming one in a comment would trip it).
        let image: native::Reference = "registry.example/repo:latest".parse().expect("stub reference");

        let streamed = pull_blob_capped(&transport, &image, &honest_digest, MAX_BUNDLE_SIZE_BYTES)
            .await
            .expect("honest under-cap blob streams back");
        assert_eq!(streamed, honest, "streamed bytes must equal the stored blob");

        assert!(
            matches!(
                pull_blob_capped(&transport, &image, &oversize_digest, MAX_BUNDLE_SIZE_BYTES).await,
                Err(VerifyErrorKind::BundleParseFailed)
            ),
            "an over-cap blob (registry lying about size) must be rejected by the bounded read",
        );
    }

    #[tokio::test]
    async fn pull_referrer_manifest_capped_accepts_honest_and_rejects_oversize() {
        // The declared descriptor size is untrusted; the actual body length is
        // the bound that matters. An honest under-cap manifest returns intact,
        // while an over-cap body (a registry lying about the size) is rejected
        // before it is parsed as JSON.
        use crate::oci::client::test_transport::{StubTransport, StubTransportData};

        let honest = br#"{"schemaVersion":2,"layers":[]}"#.to_vec();
        let oversize = vec![b'x'; MAX_REFERRER_MANIFEST_BYTES as usize + 1];
        // Parsed for the same reason as above (T-arch-G1 seam gate).
        let honest_ref: native::Reference = format!("registry.example/repo@sha256:{}", "a".repeat(64))
            .parse()
            .expect("stub reference");
        let oversize_ref: native::Reference = format!("registry.example/repo@sha256:{}", "b".repeat(64))
            .parse()
            .expect("stub reference");

        let data = StubTransportData::new();
        {
            let mut inner = data.write();
            inner
                .manifests
                .insert(honest_ref.to_string(), (honest.clone(), "sha256:honest".to_string()));
            inner
                .manifests
                .insert(oversize_ref.to_string(), (oversize, "sha256:oversize".to_string()));
        }
        let transport = StubTransport::new(data);

        let streamed = pull_referrer_manifest_capped(&transport, &honest_ref)
            .await
            .expect("honest under-cap referrer manifest");
        assert_eq!(streamed, honest, "returned bytes must equal the stored manifest");

        assert!(
            matches!(
                pull_referrer_manifest_capped(&transport, &oversize_ref).await,
                Err(VerifyErrorKind::BundleParseFailed)
            ),
            "an over-cap referrer manifest body (registry lying about size) must be rejected",
        );
    }

    #[test]
    fn aggregate_failure_reports_candidate_limit_when_candidates_unexamined() {
        // The cap left candidates unexamined and none passed: report the limit
        // distinctly, NOT an examined candidate's error — a valid signature may
        // sort past the cap, so an examined IdentityMismatch would misattribute.
        let failure = aggregate_failure(10, 8, Some(VerifyErrorKind::IdentityMismatch));
        assert!(
            matches!(failure, VerifyErrorKind::CandidateLimitExhausted { unexamined: 2 }),
            "got: {failure:?}",
        );
    }

    #[test]
    fn aggregate_failure_surfaces_examined_error_when_all_examined() {
        // Every candidate examined: surface the most actionable examined error.
        let failure = aggregate_failure(8, 8, Some(VerifyErrorKind::SignatureInvalid));
        assert!(matches!(failure, VerifyErrorKind::SignatureInvalid), "got: {failure:?}");
    }

    #[test]
    fn aggregate_failure_defaults_to_no_signatures_when_none_recorded() {
        // All examined, nothing recorded (e.g. an empty examined set) → the
        // not-signed signal, exit 79.
        let failure = aggregate_failure(3, 3, None);
        assert!(
            matches!(failure, VerifyErrorKind::NoSignaturesFound),
            "got: {failure:?}"
        );
    }

    #[test]
    fn digest_addressed_refs_derived_from_the_seam_carry_no_tag() {
        // `Client::transport_reference` returns a reference carrying the
        // resolved tag, but a `repo:tag@digest` reference keys a DIFFERENT
        // registry path and 404s — the pre-seam code built these digest-only.
        // Pins oci-spec's `clone_with_digest` tag-clearing so an upstream bump
        // that starts preserving tags fails here instead of at pull time.
        let image: native::Reference = "8.8.8.8/acme/tool:1.0".parse().expect("tagged reference");
        let derived = image.clone_with_digest(format!("sha256:{}", "a".repeat(64)));
        assert_eq!(derived.tag(), None, "digest-addressed ref must carry no tag");
        assert_eq!(derived.registry(), "8.8.8.8", "host must survive");
        assert_eq!(derived.repository(), "acme/tool", "repository must survive");
    }

    // ── Index indirection: transport traffic follows the PHYSICAL registry ──

    /// SHA-256 the indirecting test index reports as the subject digest.
    /// Preimage of [`indirection_subject_digest`]: the pipeline fetches and
    /// re-hashes the subject manifest, so the stub transport has to serve bytes
    /// that actually hash to the digest the index resolved.
    const INDIRECTION_SUBJECT_MANIFEST: &[u8] = b"indirected subject manifest";

    fn indirection_subject_digest() -> Digest {
        crate::oci::Algorithm::Sha256.hash(INDIRECTION_SUBJECT_MANIFEST)
    }

    /// Stand-in bundle blob — not a real Sigstore bundle, so verification
    /// fail-closes at `BundleParseFailed` once it has been fetched.
    const STUB_BUNDLE_BLOB: &[u8] = b"not a sigstore bundle";

    /// A structurally valid signature referrer manifest whose single layer
    /// points at [`STUB_BUNDLE_BLOB`]. Built through the production builder so
    /// the fixture cannot drift from the shape the pipeline parses.
    ///
    /// Exists so the indirection tests reach the referrer-manifest pull AND the
    /// bundle-blob pull; an empty referrer listing short-circuits at
    /// `NoSignaturesFound` and leaves those two later reads unobserved.
    fn stub_referrer_manifest() -> Vec<u8> {
        let payload = crate::oci::Descriptor {
            media_type: crate::oci::sign::bundle::BUNDLE_V03_MEDIA_TYPE.to_string(),
            digest: crate::oci::Algorithm::Sha256.hash(STUB_BUNDLE_BLOB).to_string(),
            size: STUB_BUNDLE_BLOB.len() as i64,
            ..crate::oci::Descriptor::default()
        };
        let subject = crate::oci::Descriptor {
            media_type: crate::oci::OCI_IMAGE_MEDIA_TYPE.to_string(),
            digest: indirection_subject_digest().to_string(),
            size: 2,
            ..crate::oci::Descriptor::default()
        };
        crate::oci::referrer::ReferrerManifest::build(subject, SIGSTORE_BUNDLE_V03, payload, None)
            .to_canonical_json()
            .expect("referrer manifest json")
    }

    #[tokio::test]
    async fn subject_manifest_is_rehashed_and_a_substituted_body_is_refused() {
        // `sigstore`'s verifier hashes a preimage rather than accepting a digest,
        // so the pipeline fetches the subject manifest itself. That fetch is only
        // sound if the bytes are re-hashed: a registry that serves a different
        // manifest for the resolved digest would otherwise have the signature
        // checked against ITS bytes, not the ones the index resolved.
        use crate::oci::client::test_transport::{StubTransport, StubTransportData};

        let honest = br#"{"schemaVersion":2,"config":{},"layers":[]}"#.to_vec();
        let honest_digest = crate::oci::Algorithm::Sha256.hash(&honest);
        // Same digest key, different body: exactly the substitution the re-hash
        // exists to catch.
        let substituted_digest = crate::oci::Algorithm::Sha256.hash(b"the manifest the index resolved");

        // Parsed rather than direct-constructed (T-arch-G1 seam gate).
        let image: native::Reference = "registry.example/repo:latest".parse().expect("stub reference");
        let honest_ref = image.clone_with_digest(honest_digest.to_string());
        let substituted_ref = image.clone_with_digest(substituted_digest.to_string());

        let data = StubTransportData::new();
        {
            let mut inner = data.write();
            inner
                .manifests
                .insert(honest_ref.to_string(), (honest.clone(), honest_digest.to_string()));
            inner.manifests.insert(
                substituted_ref.to_string(),
                (
                    b"a different manifest entirely".to_vec(),
                    substituted_digest.to_string(),
                ),
            );
        }
        let transport = StubTransport::new(data);

        let fetched = pull_subject_manifest_verified(&transport, &image, &honest_digest)
            .await
            .expect("a manifest that hashes to the resolved digest is accepted");
        assert_eq!(fetched, honest, "the verified preimage must be the served bytes");

        assert!(
            matches!(
                pull_subject_manifest_verified(&transport, &image, &substituted_digest).await,
                Err(VerifyErrorKind::SubjectDigestMismatch)
            ),
            "a body that does not hash to the resolved digest must be refused",
        );
    }

    #[tokio::test]
    async fn verify_refuses_a_trust_root_carrying_no_ct_log_key() {
        // A trust root carrying anchors but no CT log key — the shape a
        // hand-assembled document produces. `sigstore` builds an empty keyring
        // from it without complaint and
        // then fails every SCT check, so the pipeline refuses up front with the
        // remedy instead. Exit 78 (config), not 65 (bad signature).
        let (_key, cert) = self_signed_cert();
        let keyless = TrustRoot::from_material(
            vec![cert],
            std::collections::BTreeMap::new(),
            std::collections::BTreeMap::new(),
        );
        let (outcome, calls, _state) = drive_verify_with_trust_root(
            "8.8.8.8/acme/tool:1.0",
            crate::oci::client::MirrorMap::default(),
            keyless,
        )
        .await;
        use crate::cli::{ClassifyErrorKind, ExitCode};
        let error = match outcome {
            Ok(_) => panic!("a CT-keyless trust root cannot verify anything"),
            Err(error) => error,
        };
        assert!(
            matches!(
                error.kind,
                VerifyErrorKind::TrustRootLoad(TrustRootLoadReason::NoCtLogKey)
            ),
            "expected the no-CT-key remedy, got: {error}",
        );
        assert_eq!(
            error.kind.exit_code(),
            ExitCode::ConfigError,
            "a keyless trust root is a configuration fault, not a verification failure",
        );
        // Refused before the subject preimage is fetched: the guard is there to
        // stop per-candidate work, so reaching the manifest pull would mean it
        // sits in the wrong place.
        assert!(
            !calls.iter().any(|call| call.starts_with("pull_blob_streaming")),
            "the run must not reach signature material, got: {calls:?}",
        );
    }

    /// A test index whose logical name resolves to a DIFFERENT physical
    /// registry — the `index.ocx.sh` shape (`ocx.sh/<ns>/<pkg>` pointing at
    /// `oci://8.8.8.8/<org>/<repo>`) reduced to what the pipeline consumes.
    #[derive(Clone)]
    struct IndirectingIndex {
        physical: Identifier,
    }

    #[async_trait::async_trait]
    impl crate::oci::index::IndexImpl for IndirectingIndex {
        async fn list_repositories(&self, _: &str) -> crate::Result<Vec<String>> {
            Ok(Vec::new())
        }

        async fn list_tags(&self, _: &Identifier) -> crate::Result<Option<Vec<String>>> {
            Ok(None)
        }

        async fn fetch_manifest(
            &self,
            _: &Identifier,
            _: IndexOperation,
        ) -> crate::Result<Option<(Digest, crate::oci::Manifest)>> {
            Ok(Some((
                indirection_subject_digest(),
                crate::oci::Manifest::Image(crate::oci::ImageManifest::default()),
            )))
        }

        async fn fetch_manifest_digest(&self, _: &Identifier, _: IndexOperation) -> crate::Result<Option<Digest>> {
            Ok(Some(indirection_subject_digest()))
        }

        async fn fetch_blob(&self, _: &crate::oci::PinnedIdentifier) -> crate::Result<Option<Vec<u8>>> {
            Ok(None)
        }

        async fn physical_reference(&self, _: &Identifier) -> crate::Result<Option<Identifier>> {
            Ok(Some(self.physical.clone()))
        }

        fn box_clone(&self) -> Box<dyn crate::oci::index::IndexImpl> {
            Box::new(self.clone())
        }
    }

    /// Transport double that records `"<method>:<registry>"` for every call, so
    /// a test can assert which host the pipeline actually talked to. Only the
    /// methods this pipeline reaches do any work.
    #[derive(Clone, Default)]
    struct RecordingTransport {
        calls: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl RecordingTransport {
        fn record(&self, method: &str, image: &native::Reference) {
            self.calls
                .lock()
                .expect("recorder lock")
                .push(format!("{method}:{}", image.resolve_registry()));
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().expect("recorder lock").clone()
        }
    }

    #[async_trait::async_trait]
    impl OciTransport for RecordingTransport {
        async fn ensure_auth(
            &self,
            image: &native::Reference,
            _: crate::oci::RegistryOperation,
        ) -> std::result::Result<(), ClientError> {
            self.record("ensure_auth", image);
            Ok(())
        }

        async fn list_tags(
            &self,
            _: &native::Reference,
            _: usize,
            _: Option<String>,
        ) -> std::result::Result<Vec<String>, ClientError> {
            unimplemented!("verify never lists tags")
        }

        async fn catalog(
            &self,
            _: &native::Reference,
            _: usize,
            _: Option<String>,
        ) -> std::result::Result<Vec<String>, ClientError> {
            unimplemented!("verify never reads the catalog")
        }

        async fn fetch_manifest_digest(&self, _: &native::Reference) -> std::result::Result<String, ClientError> {
            unimplemented!("verify resolves digests through the index")
        }

        async fn pull_manifest_raw(
            &self,
            image: &native::Reference,
            _: &[&str],
        ) -> std::result::Result<(Vec<u8>, String), ClientError> {
            self.record("pull_manifest_raw", image);
            // Two manifests come through this one call: the subject itself
            // (digest-addressed, and re-hashed by the pipeline, so it must be
            // the real preimage) and the referrer manifest.
            let subject = indirection_subject_digest();
            let bytes = if image.digest() == Some(subject.to_string().as_str()) {
                INDIRECTION_SUBJECT_MANIFEST.to_vec()
            } else {
                stub_referrer_manifest()
            };
            Ok((bytes, subject.to_string()))
        }

        async fn pull_blob(&self, _: &native::Reference, _: &Digest) -> std::result::Result<Vec<u8>, ClientError> {
            unimplemented!("verify streams blobs")
        }

        async fn pull_blob_streaming(
            &self,
            image: &native::Reference,
            _: &Digest,
        ) -> std::result::Result<Box<dyn tokio::io::AsyncRead + Send + Unpin + 'static>, ClientError> {
            // Junk, on purpose: the run only has to REACH this read on the right
            // host. `parse_bundle` then fail-closes into `BundleParseFailed`.
            self.record("pull_blob_streaming", image);
            Ok(Box::new(std::io::Cursor::new(STUB_BUNDLE_BLOB.to_vec())))
        }

        async fn pull_blob_to_file(
            &self,
            _: &native::Reference,
            _: &Digest,
            _: &std::path::Path,
        ) -> std::result::Result<(), ClientError> {
            unimplemented!("verify never writes blobs to disk")
        }

        async fn head_blob(&self, _: &native::Reference, _: &Digest) -> std::result::Result<u64, ClientError> {
            unimplemented!("verify never HEADs blobs")
        }

        async fn push_manifest(
            &self,
            _: &native::Reference,
            _: &crate::oci::Manifest,
        ) -> std::result::Result<String, ClientError> {
            unimplemented!("verify never pushes")
        }

        async fn push_manifest_raw(
            &self,
            _: &native::Reference,
            _: Vec<u8>,
            _: &str,
        ) -> std::result::Result<String, ClientError> {
            unimplemented!("verify never pushes")
        }

        async fn push_blob(
            &self,
            _: &native::Reference,
            _: Vec<u8>,
            _: &Digest,
            _: std::sync::Arc<dyn Fn(u64) + Send + Sync>,
        ) -> std::result::Result<String, ClientError> {
            unimplemented!("verify never pushes")
        }

        async fn push_blob_from_path(
            &self,
            _: &native::Reference,
            _: &std::path::Path,
            _: &Digest,
            _: std::sync::Arc<dyn Fn(u64) + Send + Sync>,
        ) -> std::result::Result<String, ClientError> {
            unimplemented!("verify never pushes a file-backed blob")
        }

        async fn push_referrer_manifest(
            &self,
            _: &native::Reference,
            _: &Digest,
            _: &[u8],
            _: &str,
        ) -> std::result::Result<crate::oci::Descriptor, ClientError> {
            unimplemented!("verify never pushes")
        }

        async fn list_referrers(
            &self,
            image: &native::Reference,
            _: &Digest,
            _: Option<&str>,
        ) -> std::result::Result<Vec<crate::oci::Descriptor>, ClientError> {
            self.record("list_referrers", image);
            let bytes = stub_referrer_manifest();
            Ok(vec![crate::oci::Descriptor {
                media_type: crate::oci::OCI_IMAGE_MEDIA_TYPE.to_string(),
                digest: crate::oci::Algorithm::Sha256.hash(&bytes).to_string(),
                size: bytes.len() as i64,
                artifact_type: Some(SIGSTORE_BUNDLE_V03.to_string()),
                ..crate::oci::Descriptor::default()
            }])
        }

        fn box_clone(&self) -> Box<dyn OciTransport> {
            Box::new(self.clone())
        }
    }

    /// Drive a verify run against the recording transport for the logical name
    /// `ocx.sh/acme/tool:1.0`, indirected to the physical `8.8.8.8/acme/tool:1.0`.
    /// Returns the `"<method>:<registry>"` log plus the state dir, so a caller
    /// can read back the persisted capability record. The transport serves one
    /// referrer whose bundle blob is junk, so the run walks the full read chain
    /// (probe → list → referrer manifest → bundle blob) and ends in
    /// `BundleParseFailed`.
    async fn run_recorded_verify(mirrors: crate::oci::client::MirrorMap) -> (Vec<String>, tempfile::TempDir) {
        // A public IP literal, not a name: the pipeline now resolves the physical
        // host before dialing it (dial-site SSRF guard), and an IP literal resolves
        // locally -- a DNS name here would make this unit test open a socket.
        let (outcome, calls, temp) = drive_verify_at("8.8.8.8/acme/tool:1.0", mirrors).await;
        let Err(error) = outcome else {
            panic!("the recording transport serves a junk bundle, so verify must fail");
        };
        assert!(
            matches!(error.kind, VerifyErrorKind::BundleParseFailed),
            "expected the junk-bundle outcome, got: {error}",
        );
        (calls, temp)
    }

    /// `run_recorded_verify` with the physical registry the index rewrites to
    /// made an argument and the outcome returned rather than asserted, so a
    /// test can point the indirection at a forbidden target.
    async fn drive_verify_at(
        physical: &str,
        mirrors: crate::oci::client::MirrorMap,
    ) -> (Result<Vec<VerifyResult>, VerifyError>, Vec<String>, tempfile::TempDir) {
        let (_key, cert) = self_signed_cert();
        drive_verify_with_trust_root(physical, mirrors, trust_root_of(&[&cert])).await
    }

    /// `drive_verify_at` with the trust root made an argument, so a test can
    /// drive the run with material the pipeline is expected to refuse.
    async fn drive_verify_with_trust_root(
        physical: &str,
        mirrors: crate::oci::client::MirrorMap,
        trust_root: TrustRoot,
    ) -> (Result<Vec<VerifyResult>, VerifyError>, Vec<String>, tempfile::TempDir) {
        let logical = Identifier::parse("ocx.sh/acme/tool:1.0").expect("logical identifier");
        let physical = Identifier::parse(physical).expect("physical identifier");

        let transport = RecordingTransport::default();
        let mut client = Client::with_transport(Box::new(transport.clone()));
        client.mirrors = mirrors;
        let index = Index::from_impl(IndirectingIndex { physical });
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = StateStore::new(temp.path());
        // Loopback rather than a `.example` name: the pipeline's dial-time SSRF
        // guard resolves the endpoint before use, and a documentation domain does
        // not resolve -- which would make this unit test depend on DNS. Loopback is
        // also what a real local stack looks like.
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");

        let outcome = VerifyPipeline::run(
            &client,
            VerifyContext {
                identifier: &logical,
                platform: None,
                policies: &[],
                no_cache: true,
                index: &index,
                trust_root: &trust_root,
                rekor_url: &rekor_url,
                state: &state,
                offline: true,
                content: VerifyContentMode::Signature,
                verification: VerificationMode::Demand,
                signature_format: None,
                allow_unlogged_signature: false,
                report_all: false,
            },
        )
        .await;
        (outcome, transport.calls(), temp)
    }

    #[tokio::test]
    async fn verify_reads_referrers_from_the_physical_registry_not_the_logical_one() {
        // Index indirection (`adr_index_indirection.md` C2): the logical
        // `ocx.sh/...` name is a pointer; the artifact and its signature
        // referrers live on the physical registry the index root names. A
        // pipeline that builds its transport reference from the LOGICAL
        // identifier asks the wrong host for the signature — which for an
        // indirected package reads as "not signed" (exit 79) no matter how the
        // publisher signed it.
        let (calls, _state_dir) = run_recorded_verify(crate::oci::client::MirrorMap::default()).await;
        // Name every stage explicitly: the `all()` below passes vacuously if the
        // run short-circuits before the later reads, so the later reads have to
        // be asserted present, not just consistent.
        for stage in [
            "list_referrers:8.8.8.8",
            "pull_manifest_raw:8.8.8.8",
            "pull_blob_streaming:8.8.8.8",
        ] {
            assert!(
                calls.iter().any(|call| call == stage),
                "`{stage}` must target the physical registry, got: {calls:?}",
            );
        }
        assert!(
            calls.iter().all(|call| call.ends_with(":8.8.8.8")),
            "no transport call may target the logical index host, got: {calls:?}",
        );
    }

    #[tokio::test]
    async fn verify_stays_on_the_mirror_and_never_writes() {
        // Verify is read-only, so unlike sign it has no canonical-host half:
        // every read — the referrer listing included — follows the mirror.
        // The write assertion is the standing guard — a push added here would
        // hit a read-only mirror (ADR Q5).
        //
        // This test used to also pin the referrers *capability cache key* to the
        // mirror. That half is gone with the probe: `ensure_referrers_supported`
        // was verify's only `ReferrersApiCapability::probe` site, and D-1 replaced
        // it with `list_referrers_with_fallback`, which asks no capability
        // question and writes no cache entry. The CWE-345 property it defended —
        // deciding on one host while acting on another — now rides entirely on
        // the addressing assertions below, which cover it at the new call site:
        // `list_referrers_with_fallback` and the fallback-tag read inside it both
        // derive from the one `image` reference the `all(..)` assertion pins to
        // the mirror. The capability cache itself is still guarded on the write
        // path, by `sign/pipeline.rs`'s own mirror test.
        let mirrors = crate::oci::client::MirrorMap::new([(
            "8.8.8.8".to_string(),
            crate::config::mirror::ParsedMirror {
                protocol: "https".to_string(),
                host: "mirror.example".to_string(),
                path_prefix: "proxy".to_string(),
            },
        )]);
        let (calls, _state_dir) = run_recorded_verify(mirrors).await;

        for stage in [
            "list_referrers:mirror.example",
            "pull_manifest_raw:mirror.example",
            "pull_blob_streaming:mirror.example",
        ] {
            assert!(
                calls.iter().any(|call| call == stage),
                "`{stage}` must follow the mirror, got: {calls:?}",
            );
        }
        assert!(
            calls.iter().all(|call| call.ends_with(":mirror.example")),
            "every verify call is a read and must follow the mirror, got: {calls:?}",
        );
        assert!(
            !calls.iter().any(|call| call.starts_with("push_")),
            "verify must never write, got: {calls:?}",
        );
    }

    // NOTE: the pipeline-wire E2E adversarial cases — ANY-of key rotation,
    //   malformed-first-referrer DoS, and the cross-subject splice — need a
    //   transport that serves `list_referrers` + referrer manifests + bundle blobs
    //   plus real Fulcio-minted certs and a real Rekor SET. `StubTransport`
    //   serves seeded referrers but no bundle crypto, and minting that crypto
    //   material in Rust would mean reimplementing a Sigstore CA here.
    //   Those cases are covered end-to-end in the acceptance suite against the
    //   real local stack (`test/tests/test_verify.py`, `test_auto_verify.py`); the
    //   pure body/SET-binding splice is unit-covered by the
    //   `transparency_body_binding_*` tests above.

    #[tokio::test]
    async fn verify_refuses_a_rewritten_registry_that_resolves_into_a_forbidden_range() {
        // The read-side half of the same CWE-918 hole: a hostile index rewrite
        // makes the verify pipeline dial an internal address with the caller's
        // registry credentials attached. Refused at the dial site, fail-closed --
        // the upstream string check tolerates a resolution failure by design.
        let (outcome, calls, _state) = drive_verify_at(
            "169.254.169.254/acme/tool:1.0",
            crate::oci::client::MirrorMap::default(),
        )
        .await;
        let Err(error) = outcome else {
            panic!("a link-local rewrite target must be refused");
        };
        assert!(
            matches!(error.kind, VerifyErrorKind::ForbiddenRegistryTarget { .. }),
            "expected the SSRF refusal, got: {error}",
        );
        assert!(
            calls.is_empty(),
            "no transport call may precede the refusal, got: {calls:?}",
        );
    }

    // ── WP6: the attestation scan's budget accounting ──

    /// A signature that a crowd of attestations must not be able to hide.
    ///
    /// `MAX_SIGNATURE_CANDIDATES` is 8, so nine attestation referrers ahead of
    /// the signature in listing order would exhaust the scan before it is ever
    /// examined — if a mode-mismatched candidate consumed a slot. Attaching
    /// SBOMs to a signed artifact is the *normal* case, which is what makes
    /// this a live availability defect and not a hypothetical one.
    ///
    /// Mutation target: making `skipped_other_mode` increment `examined` (or
    /// routing `ModeMismatch` through `examined()`) must turn this red.
    #[test]
    fn mode_mismatched_candidates_never_consume_the_requested_modes_budget() {
        let caps = VerifyContentMode::Signature.caps();
        let mut budget = ScanBudget::new(caps);

        for attestation in 1..=(caps.candidates + 1) {
            assert!(
                budget.may_examine(),
                "candidate {attestation} must still be reachable: attestations cost bytes, never slots",
            );
            budget.charge(4096);
            budget.skipped_other_mode();
        }

        assert_eq!(budget.examined, 0, "no attestation was examined in signature mode");
        assert_eq!(
            budget.considered,
            caps.candidates + 1,
            "but every one of them was looked at, which is what the aggregate reports from",
        );
        assert!(budget.may_examine(), "and the signature behind them is still reachable",);
        assert!(budget.stop.is_none(), "nothing stopped the scan");
    }

    /// The other half: a candidate of the requested kind does spend a slot, so
    /// the cap still bounds the scan. Without this the test above passes for a
    /// budget that counts nothing at all.
    #[test]
    fn in_mode_candidates_do_consume_the_budget_and_the_cap_still_bites() {
        let caps = VerifyContentMode::Signature.caps();
        let mut budget = ScanBudget::new(caps);
        for _ in 0..caps.candidates {
            assert!(budget.may_examine());
            budget.examined();
        }
        assert!(!budget.may_examine(), "the candidate cap must stop the scan");
        assert_eq!(budget.stop, Some(ScanStop::CandidateCap));
    }

    /// The listing backstop. Independent of the candidate cap, because a
    /// registry can answer a referrers listing with far more entries than
    /// either mode's candidate ceiling and every one of them costs a decision.
    #[test]
    fn listing_iteration_is_backstopped_independently_of_the_candidate_cap() {
        let caps = VerifyContentMode::Attestation { predicate_type: None }.caps();
        let mut budget = ScanBudget::new(caps);
        // Only mode-mismatched candidates, so neither the candidate cap nor the
        // byte budget can be what stops it.
        for _ in 0..MAX_REFERRER_LISTING_ITERATION {
            assert!(budget.may_examine());
            budget.skipped_other_mode();
        }
        assert!(!budget.may_examine());
        assert_eq!(
            budget.stop,
            Some(ScanStop::ListingCap),
            "an unbounded listing must stop on the listing backstop, not run forever",
        );
    }

    /// The byte budget is charged from bytes actually read, so a registry
    /// cannot buy extra fetches by advertising size 0.
    #[test]
    fn the_byte_budget_stops_the_scan_on_bytes_actually_read() {
        let caps = VerifyContentMode::Attestation { predicate_type: None }.caps();
        let mut budget = ScanBudget::new(caps);
        assert!(budget.may_examine());
        budget.charge(caps.total_bytes);
        assert!(!budget.may_examine());
        assert_eq!(budget.stop, Some(ScanStop::ByteBudget));
    }

    /// A refused candidate with a fixed digest, so a report assertion can name
    /// which one without the fixture deciding it.
    fn refusal(reason: VerifyErrorKind) -> RefusedCandidate {
        RefusedCandidate {
            referrer_digest: "sha256:refused".into(),
            reason,
        }
    }

    fn attestation_ctx<'a>(
        identifier: &'a Identifier,
        index: &'a Index,
        trust_root: &'a TrustRoot,
        rekor_url: &'a Url,
        state: &'a StateStore,
    ) -> VerifyContext<'a> {
        VerifyContext {
            identifier,
            // `IndirectingIndex` resolves to a bare image manifest, so there is
            // nothing to narrow into: C-010's "act on whatever resolved".
            platform: None,
            policies: &[],
            no_cache: true,
            index,
            trust_root,
            rekor_url,
            state,
            offline: true,
            content: VerifyContentMode::Attestation { predicate_type: None },
            verification: VerificationMode::Demand,
            signature_format: None,
            allow_unlogged_signature: false,
            report_all: false,
        }
    }

    /// Which bound stopped a truncated attestation scan is the actionable part,
    /// and each one has its own exit-code-bearing variant. Fail-closed: a
    /// truncated scan cannot answer a question about *every* attestation, so it
    /// never returns the partial list — asserted here as the raise, because the
    /// `matches` argument is non-empty in every case.
    #[test]
    fn a_truncated_attestation_scan_raises_the_bound_that_stopped_it() {
        let identifier = verify_id();
        let index = Index::from_impl(IndirectingIndex {
            physical: identifier.clone(),
        });
        let trust_root = trust_root_of(&[]);
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = StateStore::new(temp.path());
        let ctx = attestation_ctx(&identifier, &index, &trust_root, &rekor_url, &state);
        let caps = ctx.content.caps();

        let verified = || {
            (
                VerifyResult {
                    subject_digest: crate::oci::Algorithm::Sha256.hash(b"subject"),
                    referrer_digest: crate::oci::Algorithm::Sha256.hash(b"referrer"),
                    key_backend: KeyBackendKind::Keyless,
                    certificate_identity: None,
                    certificate_oidc_issuer: None,
                    signed_at: Some(0),
                    signature_format: SignatureFormat::Bundle,
                    discovery_method: DiscoveryMethod::ReferrersApi,
                    rekor_log_index: None,
                },
                None,
            )
        };

        for (stop, expected) in [
            (ScanStop::CandidateCap, "too_many_attestations"),
            (ScanStop::ByteBudget, "attestation_budget_exhausted"),
            (ScanStop::ListingCap, "candidate_limit_exhausted"),
        ] {
            let mut budget = ScanBudget::new(caps);
            budget.stop = Some(stop);
            budget.considered = 3;
            let outcome = VerifyPipeline::finish_scan(&ctx, caps, 9, &budget, vec![verified()], Vec::new());
            let error = outcome.expect_err("a truncated scan never returns a partial list");
            assert_eq!(
                error.kind_detail(),
                expected,
                "{stop:?} must name the bound that stopped the scan",
            );
        }
    }

    /// An untruncated scan that verified nothing and recorded no defect is
    /// genuinely not-found (exit 79), not a data error. A narrowing miss —
    /// `--type` asked for something this artifact does not carry — records no
    /// failure at all, which is what makes this the reachable outcome for it.
    #[test]
    fn an_untruncated_attestation_scan_with_nothing_recorded_is_not_found() {
        let identifier = verify_id();
        let index = Index::from_impl(IndirectingIndex {
            physical: identifier.clone(),
        });
        let trust_root = trust_root_of(&[]);
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = StateStore::new(temp.path());
        let ctx = attestation_ctx(&identifier, &index, &trust_root, &rekor_url, &state);
        let caps = ctx.content.caps();

        let outcome = VerifyPipeline::finish_scan(&ctx, caps, 2, &ScanBudget::new(caps), Vec::new(), Vec::new());
        assert!(matches!(outcome, Err(VerifyErrorKind::AttestationNotFound)));
        assert_eq!(
            VerifyErrorKind::AttestationNotFound.exit_code(),
            ExitCode::NotFound,
            "S-017: a missing SBOM is not-found, never a data error",
        );

        // But a recorded defect outranks it: a candidate that was of the right
        // kind and broken must not be reported as "this artifact has none".
        let outcome = VerifyPipeline::finish_scan(
            &ctx,
            caps,
            2,
            &ScanBudget::new(caps),
            Vec::new(),
            vec![refusal(VerifyErrorKind::TlogBindingMismatch)],
        );
        assert!(matches!(outcome, Err(VerifyErrorKind::TlogBindingMismatch)));
    }

    /// Collect-all, not first-match: the attestation scan returns every
    /// verified candidate. Letting the registry's listing order pick one of
    /// several verified documents would be the defect — a subject can carry an
    /// SBOM *and* provenance, and `--type`-less `ocx package sbom` must see both.
    ///
    /// Mutation target: returning early on the first match in the `All` arm, or
    /// truncating here, must turn this red.
    #[test]
    fn an_untruncated_attestation_scan_returns_every_verified_candidate() {
        let identifier = verify_id();
        let index = Index::from_impl(IndirectingIndex {
            physical: identifier.clone(),
        });
        let trust_root = trust_root_of(&[]);
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = StateStore::new(temp.path());
        let ctx = attestation_ctx(&identifier, &index, &trust_root, &rekor_url, &state);
        let caps = ctx.content.caps();

        let matches: Vec<(VerifyResult, Option<VerifiedAttestation>)> = (0..3u8)
            .map(|n| {
                (
                    VerifyResult {
                        subject_digest: crate::oci::Algorithm::Sha256.hash(b"subject"),
                        referrer_digest: crate::oci::Algorithm::Sha256.hash([n]),
                        key_backend: KeyBackendKind::Keyless,
                        certificate_identity: None,
                        certificate_oidc_issuer: None,
                        signed_at: Some(0),
                        signature_format: SignatureFormat::Bundle,
                        discovery_method: DiscoveryMethod::ReferrersApi,
                        rekor_log_index: None,
                    },
                    None,
                )
            })
            .collect();

        let returned = VerifyPipeline::finish_scan(&ctx, caps, 3, &ScanBudget::new(caps), matches, Vec::new())
            .expect("three verified candidates are three results");
        assert_eq!(
            returned.matches.len(),
            3,
            "every verified attestation is returned, not the first"
        );
    }

    /// A refused candidate beside a passing one is reported, not dropped and not
    /// fatal. Dropping it under-reports ("1 attestation" where the subject
    /// carries two, one broken); failing on it hands a single malformed referrer
    /// the power to hide every valid attestation next to it.
    ///
    /// Mutation targets: returning `Err` on a non-empty `refused`, or dropping
    /// `refused` from the `Ok` arm, must each turn this red.
    #[test]
    fn a_scan_that_finds_matches_still_reports_the_candidates_it_refused() {
        let identifier = verify_id();
        let index = Index::from_impl(IndirectingIndex {
            physical: identifier.clone(),
        });
        let trust_root = trust_root_of(&[]);
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = StateStore::new(temp.path());
        let ctx = attestation_ctx(&identifier, &index, &trust_root, &rekor_url, &state);
        let caps = ctx.content.caps();

        let passing = (
            VerifyResult {
                subject_digest: crate::oci::Algorithm::Sha256.hash(b"subject"),
                referrer_digest: crate::oci::Algorithm::Sha256.hash(b"good"),
                key_backend: KeyBackendKind::Keyless,
                certificate_identity: None,
                certificate_oidc_issuer: None,
                signed_at: Some(0),
                signature_format: SignatureFormat::Bundle,
                discovery_method: DiscoveryMethod::ReferrersApi,
                rekor_log_index: None,
            },
            None,
        );

        let returned = VerifyPipeline::finish_scan(
            &ctx,
            caps,
            2,
            &ScanBudget::new(caps),
            vec![passing],
            vec![refusal(VerifyErrorKind::TlogBindingMismatch)],
        )
        .expect("one refused candidate must not fail a scan that found a match");

        assert_eq!(returned.matches.len(), 1, "the passing candidate is still returned");
        assert_eq!(returned.refused.len(), 1, "and the refused one travels out beside it");
        assert_eq!(
            returned.refused[0].referrer_digest, "sha256:refused",
            "the report names which candidate was refused",
        );
        assert_eq!(
            returned.refused[0].reason.kind_detail(),
            "tlog_binding_mismatch",
            "and why",
        );
    }

    /// The aggregate failure keeps the first strictly-highest-ranked refusal, so
    /// two equally-ranked refusals resolve in listing order rather than by
    /// whichever the fold happened to see last.
    #[test]
    fn the_aggregate_failure_is_the_first_most_actionable_refusal() {
        // `SignatureInvalid` outranks `BundleParseFailed`, and the two
        // `SignatureInvalid`s tie — the first must win.
        let picked = best_failure(vec![
            refusal(VerifyErrorKind::BundleParseFailed),
            RefusedCandidate {
                referrer_digest: "sha256:first".into(),
                reason: VerifyErrorKind::SignatureInvalid,
            },
            RefusedCandidate {
                referrer_digest: "sha256:second".into(),
                reason: VerifyErrorKind::SignatureInvalid,
            },
        ])
        .expect("a non-empty refusal list has a best");
        assert_eq!(picked.kind_detail(), "signature_invalid");
        assert!(best_failure(Vec::new()).is_none(), "nothing refused, nothing to report");
    }

    /// The signature arm is untouched by all of the above: a `FirstMatch` scan
    /// that found nothing still aggregates today's failure, over the candidates
    /// actually looked at.
    #[test]
    fn the_signature_arm_still_aggregates_its_failure() {
        let identifier = verify_id();
        let index = Index::from_impl(IndirectingIndex {
            physical: identifier.clone(),
        });
        let trust_root = trust_root_of(&[]);
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = StateStore::new(temp.path());
        let ctx = VerifyContext {
            identifier: &identifier,
            platform: None,
            policies: &[],
            no_cache: true,
            index: &index,
            trust_root: &trust_root,
            rekor_url: &rekor_url,
            state: &state,
            offline: true,
            content: VerifyContentMode::Signature,
            verification: VerificationMode::Demand,
            signature_format: None,
            allow_unlogged_signature: false,
            report_all: false,
        };
        let caps = ctx.content.caps();
        let mut budget = ScanBudget::new(caps);
        budget.considered = 1;

        let outcome = VerifyPipeline::finish_scan(
            &ctx,
            caps,
            1,
            &budget,
            Vec::new(),
            vec![refusal(VerifyErrorKind::SignatureInvalid)],
        );
        assert!(
            matches!(outcome, Err(VerifyErrorKind::SignatureInvalid)),
            "signature-mode aggregation is unchanged",
        );
    }

    /// Discriminating a candidate's content kind needs only the `content`
    /// oneof, which `parse_bundle` has already produced. Checking the
    /// verification material first means a *malformed* bundle of the other kind
    /// spends a candidate slot in this mode's budget — the same crowd-out the
    /// non-consuming skip exists to prevent, reached through a different door.
    #[test]
    fn the_mode_gate_precedes_the_verification_material_checks() {
        use sigstore_protobuf_specs::dev::sigstore::bundle::v1::bundle;
        use sigstore_protobuf_specs::io::intoto::Envelope;
        let attestation_mode = VerifyContentMode::Attestation { predicate_type: None };

        let mut dsse_without_material = message_bundle(false, false);
        dsse_without_material.content = Some(bundle::Content::DsseEnvelope(Envelope {
            payload: Vec::new(),
            payload_type: String::new(),
            signatures: Vec::new(),
        }));
        assert!(
            matches!(
                BundleParts::from_bundle(&dsse_without_material, &VerifyContentMode::Signature),
                Err(VerifyErrorKind::NoUsableBundle)
            ),
            "an attestation in signature mode is the other kind before it is malformed",
        );

        let message_without_material = message_bundle(false, false);
        assert!(
            matches!(
                BundleParts::from_bundle(&message_without_material, &attestation_mode),
                Err(VerifyErrorKind::NoUsableBundle)
            ),
            "and symmetrically in the other direction",
        );

        // The gate moving earlier must not swallow a genuine malformed-bundle
        // report for a candidate that IS the requested kind. Re-asserted
        // through a cosign image-signature DSSE since D2: a `messageSignature`
        // is now the other kind in *both* modes, so it can no longer stand in
        // for "the requested kind, but malformed".
        let signature_without_material = into_signature_bundle(message_bundle(false, false));
        assert!(matches!(
            BundleParts::from_bundle(&signature_without_material, &VerifyContentMode::Signature),
            Err(VerifyErrorKind::BundleParseFailed)
        ));
    }

    // ── WP-R1: the annotation contract, both halves ──

    /// A referrer as the referrers API lists it, with or without the
    /// `dev.sigstore.bundle.content` hint the sign and attest paths write.
    fn listed(digest: &str, hint: Option<&str>) -> crate::oci::Descriptor {
        crate::oci::Descriptor {
            media_type: crate::oci::OCI_IMAGE_MEDIA_TYPE.to_string(),
            digest: digest.to_string(),
            size: 512,
            annotations: hint.map(|hint| {
                std::collections::BTreeMap::from([(ANNOTATION_BUNDLE_CONTENT.to_string(), hint.to_string())])
            }),
            ..crate::oci::Descriptor::default()
        }
    }

    fn digests(candidates: &[crate::oci::Descriptor]) -> Vec<&str> {
        candidates.iter().map(|candidate| candidate.digest.as_str()).collect()
    }

    /// [`listed`] plus the `dev.sigstore.bundle.predicateType` hint — the
    /// annotation that tells a signature from an attestation now that both are
    /// `dsse-envelope`.
    fn listed_typed(digest: &str, content: &str, predicate_type: &str) -> crate::oci::Descriptor {
        let mut descriptor = listed(digest, Some(content));
        descriptor
            .annotations
            .get_or_insert_default()
            .insert(ANNOTATION_BUNDLE_PREDICATE_TYPE.to_string(), predicate_type.to_string());
        descriptor
    }

    /// A predicateType that is emphatically not the image-signature one.
    fn sbom_predicate() -> &'static str {
        PredicateType::CycloneDx.uri()
    }

    /// The availability defect the ordering half closes: attaching SBOMs to a
    /// signed artifact must not make it unverifiable.
    ///
    /// `MAX_SIGNATURE_CANDIDATES` is 8, so eight attestation referrers sorting
    /// ahead of the signature by digest exhaust a signature scan before it is
    /// reached. The slot-free `ModeMismatch` skip does not save it: that skip is
    /// only reachable once the bundle has been pulled and parsed, and a DSSE
    /// bundle over the 512 KiB signature-mode gate is refused before that — a
    /// refusal, which spends a slot.
    ///
    /// Since D2 the crowd and the signature share the `dsse-envelope` content
    /// hint, so the **predicateType** annotation is what carries the demotion.
    /// Written in that vocabulary rather than deleted: the availability defect
    /// is unchanged, only the annotation that discriminates it moved.
    ///
    /// Both modes asserted, so a demotion wired to one answer reds.
    #[test]
    fn a_content_hint_naming_the_other_kind_sorts_behind_every_other_candidate() {
        let crowd = || {
            let mut candidates: Vec<crate::oci::Descriptor> = (0..MAX_SIGNATURE_CANDIDATES)
                .map(|n| listed_typed(&format!("sha256:0{n}"), BUNDLE_CONTENT_DSSE, sbom_predicate()))
                .collect();
            // Sorts last by digest, which is what makes it unreachable without
            // the demotion: every attestation above it spends a slot first.
            candidates.push(listed_typed(
                "sha256:ff",
                BUNDLE_CONTENT_DSSE,
                COSIGN_SIGN_PREDICATE_TYPE,
            ));
            candidates
        };

        let mut candidates = crowd();
        order_candidates(&mut candidates, &VerifyContentMode::Signature);
        assert_eq!(
            candidates[0].digest, "sha256:ff",
            "the signature must be examined first, whatever the digests sort to",
        );
        let mut candidates = crowd();
        order_candidates(
            &mut candidates,
            &VerifyContentMode::Attestation { predicate_type: None },
        );
        assert_eq!(
            candidates[0].digest, "sha256:00",
            "and in attestation mode the attestations lead instead",
        );
        assert_eq!(
            candidates.last().expect("non-empty").digest,
            "sha256:ff",
            "with the signature demoted, not dropped",
        );
    }

    /// A referrer carrying no hint keeps its digest position: pushed by a tool
    /// that writes no annotation, or listed by a transport that does not echo
    /// them, it must not sort behind one that does. Only a hint that positively
    /// names the other kind demotes.
    #[test]
    fn a_candidate_with_no_content_hint_keeps_its_digest_position() {
        let mut candidates = vec![
            listed("sha256:c", None),
            // Sorts first by digest; the hint is what moves it to the tail —
            // `message-signature` is a shape neither mode reads since D2.
            listed("sha256:a", Some("message-signature")),
            listed("sha256:b", None),
            listed("sha256:a0", Some(BUNDLE_CONTENT_DSSE)),
        ];
        order_candidates(&mut candidates, &VerifyContentMode::Signature);
        assert_eq!(
            digests(&candidates),
            ["sha256:a0", "sha256:b", "sha256:c", "sha256:a"],
            "digest order inside each group, mismatched hints last",
        );
    }

    /// Demoted candidates keep their digest order among themselves, and an
    /// unrecognised hint demotes exactly like a known-other-kind one.
    ///
    /// This pins the ordering, not the survival: `order_candidates` takes a
    /// `&mut [_]`, so dropping a candidate inside it is a compile error rather
    /// than something a test has to catch — which is why the signature is a
    /// slice and not a `&mut Vec`.
    #[test]
    fn a_mismatched_or_unrecognised_hint_is_demoted_not_reordered_among_peers() {
        let mut candidates = vec![
            listed("sha256:aa", Some(BUNDLE_CONTENT_DSSE)),
            listed("sha256:bb", Some("some-kind-ocx-has-never-heard-of")),
            listed("sha256:cc", Some("message-signature")),
        ];
        order_candidates(&mut candidates, &VerifyContentMode::Signature);
        assert_eq!(
            digests(&candidates),
            ["sha256:aa", "sha256:bb", "sha256:cc"],
            "a matching hint leads; the pre-parity and unrecognised hints follow in digest order",
        );
    }

    /// The referrer-manifest bytes for a bundle blob, plus the listing
    /// descriptor addressing them. `annotation` is the unsigned
    /// `dev.sigstore.bundle.predicateType` claim the direction check reads.
    fn referrer_with(
        subject_digest: &Digest,
        blob_digest: &Digest,
        blob_size: i64,
        annotation: Option<&str>,
    ) -> (crate::oci::Descriptor, Vec<u8>) {
        let payload = crate::oci::Descriptor {
            media_type: SIGSTORE_BUNDLE_V03.to_string(),
            digest: blob_digest.to_string(),
            size: blob_size,
            ..crate::oci::Descriptor::default()
        };
        let subject = crate::oci::Descriptor {
            media_type: crate::oci::OCI_IMAGE_MEDIA_TYPE.to_string(),
            digest: subject_digest.to_string(),
            size: 7,
            ..crate::oci::Descriptor::default()
        };
        let annotations = annotation.map(|predicate_type| {
            std::collections::BTreeMap::from([(
                ANNOTATION_BUNDLE_PREDICATE_TYPE.to_string(),
                predicate_type.to_string(),
            )])
        });
        let manifest =
            crate::oci::referrer::ReferrerManifest::build(subject, SIGSTORE_BUNDLE_V03, payload, annotations);
        let bytes = manifest.to_canonical_json().expect("referrer manifest serializes");
        let descriptor = crate::oci::Descriptor {
            media_type: crate::oci::OCI_IMAGE_MEDIA_TYPE.to_string(),
            digest: crate::oci::Algorithm::Sha256.hash(&bytes).to_string(),
            size: bytes.len() as i64,
            ..crate::oci::Descriptor::default()
        };
        (descriptor, bytes)
    }

    /// A bundle carrying a DSSE envelope over an in-toto Statement that binds
    /// `subject_digest` and declares `predicate_type` in its **signed** payload.
    fn dsse_bundle_binding(subject_digest: &Digest, predicate_type: &str, predicate: &str) -> Bundle {
        use sigstore_protobuf_specs::dev::sigstore::bundle::v1::bundle;
        use sigstore_protobuf_specs::io::intoto::{Envelope, Signature};
        let statement = format!(
            r#"{{"_type":"https://in-toto.io/Statement/v1","subject":[{{"name":"pkg","digest":{{"sha256":"{}"}}}}],"predicateType":"{predicate_type}","predicate":{predicate}}}"#,
            subject_digest.hex(),
        );
        let mut bundle = message_bundle(true, true);
        bundle.content = Some(bundle::Content::DsseEnvelope(Envelope {
            payload: statement.into_bytes(),
            payload_type: crate::oci::attest::DSSE_PAYLOAD_TYPE.to_string(),
            signatures: vec![Signature {
                sig: vec![0xDE, 0xAD, 0xBE, 0xEF],
                keyid: String::new(),
            }],
        }));
        bundle
    }

    /// Row 7 / D-e, the annotation **direction**: the signed payload decides the
    /// predicateType, and the unsigned annotation is only ever cross-checked
    /// against it (CVE-2022-35929 class). A registry that rewrites that one
    /// string gets a refusal — never a relabelled document, and never a quiet
    /// "none found", which is why this is a failure and not a narrowing miss.
    ///
    /// Both directions asserted, because a check that only ever meets the
    /// disagreeing case is indistinguishable from one wired to the wrong
    /// comparison: deleting the check reds the first half, inverting `!=` to
    /// `==` reds the second.
    #[tokio::test]
    async fn an_annotation_disagreeing_with_the_signed_predicate_type_is_refused() {
        use crate::oci::client::test_transport::{StubTransport, StubTransportData};

        const SIGNED: &str = "https://cyclonedx.org/bom";
        const CYCLONEDX_PREDICATE: &str = r#"{"bomFormat":"CycloneDX"}"#;
        const REWRITTEN: &str = "https://slsa.dev/provenance/v1";

        let subject_bytes = b"the artifact under attestation";
        let subject_digest = crate::oci::Algorithm::Sha256.hash(subject_bytes);
        let blob = serde_json::to_vec(&dsse_bundle_binding(&subject_digest, SIGNED, CYCLONEDX_PREDICATE))
            .expect("bundle serializes");
        let blob_digest = crate::oci::Algorithm::Sha256.hash(&blob);

        let data = StubTransportData::new();
        data.write().blobs.insert(blob_digest.to_string(), blob.clone());
        let transport = StubTransport::new(data);
        let image: native::Reference = "registry.example/repo:latest".parse().expect("stub reference");

        // A real CA: `Verifier::new` compiles the trust root into a certificate
        // pool and refuses the placeholder DER the routing tests use.
        let ca_der = super::super::tlog::fixture_certificate_der();
        let trust_root = trust_root_of(&[&ca_der]);
        let verifier = Verifier::new(RekorConfiguration::default(), trust_root.clone()).expect("verifier");
        let identifier = verify_id();
        let index = Index::from_impl(IndirectingIndex {
            physical: identifier.clone(),
        });
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = StateStore::new(temp.path());
        let ctx = attestation_ctx(&identifier, &index, &trust_root, &rekor_url, &state);

        let verdict_for = async |annotation: Option<&str>| {
            let (descriptor, bytes) = referrer_with(&subject_digest, &blob_digest, blob.len() as i64, annotation);
            let mut budget = ScanBudget::new(ctx.content.caps());
            VerifyPipeline::verify_one_referrer(
                &transport,
                &ctx,
                &verifier,
                &descriptor,
                bytes,
                &subject_digest,
                subject_bytes,
                &image,
                crate::oci::verify::DiscoveryMethod::ReferrersApi,
                &mut budget,
                &RekorKeyMemo::default(),
            )
            .await
        };

        // Agreeing first, so each half's red state is reachable on its own: an
        // inverted comparison reds here, and a deleted check reds below.
        // The candidate goes on to the crypto, which is where this fixture's
        // placeholder certificate stops it — any verdict but a predicate-type
        // mismatch proves the check let it past.
        for annotation in [Some(SIGNED), None] {
            let verdict = verdict_for(annotation).await;
            assert!(
                !matches!(&verdict, Err(VerifyErrorKind::PredicateTypeMismatch { .. })),
                "an agreeing ({annotation:?}) annotation must not be refused here: {verdict:?}",
            );
        }

        // Disagreeing: refused, and the refusal names both strings so the
        // operator can see which one was rewritten.
        let verdict = verdict_for(Some(REWRITTEN)).await;
        assert!(
            matches!(
                &verdict,
                Err(VerifyErrorKind::PredicateTypeMismatch { expected, actual })
                    if expected == REWRITTEN && actual == SIGNED
            ),
            "a rewritten predicateType annotation must be refused: {verdict:?}",
        );
    }

    // ── S-002: cosign's own keyless DSSE image signature, verified offline ──

    /// cosign v3.1.1's signature bundle, captured in G0. `include_str!` rather
    /// than a runtime read: a moved fixture becomes a compile error.
    const GOLDEN_KEYLESS_BUNDLE: &str = include_str!("../../../../../test/tests/fixtures/golden/keyless_bundle.json");

    /// The referrer manifest cosign pushed alongside it — the source of every
    /// digest and annotation this test pins, so nothing is transcribed by hand.
    const GOLDEN_KEYLESS_REFERRER: &str =
        include_str!("../../../../../test/tests/fixtures/golden/keyless_referrer_manifest.json");

    /// The whole local Sigstore trust root: Fulcio CA, CT log key, Rekor key.
    /// Committed and deterministic, which is what makes this test offline.
    const GOLDEN_TRUSTED_ROOT: &str = include_str!("../../../../../test/sigstore/trusted_root.json");

    /// The subject manifest cosign signed, byte-for-byte.
    ///
    /// Not a committed fixture: `generate.py` builds every golden subject from
    /// the fixed payload `b"ocx-golden-subject"` through
    /// `registry.push_minimal_image`, so these bytes are reproducible rather
    /// than captured. `the_golden_subject_bytes_are_the_ones_cosign_signed`
    /// below is what ties them to the committed referrer manifest — without it
    /// this constant would be an unchecked transcription.
    const GOLDEN_SUBJECT_MANIFEST: &str = concat!(
        r#"{"schemaVersion": 2, "mediaType": "application/vnd.oci.image.manifest.v1+json", "#,
        r#""config": {"mediaType": "application/vnd.oci.empty.v1+json", "#,
        r#""digest": "sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a", "size": 2}, "#,
        r#""layers": [{"mediaType": "application/octet-stream", "#,
        r#""digest": "sha256:ee88d8a4c22bbe871bcee1c56bcc02377e249363600edcaf096ad7a5a862149f", "size": 18}]}"#,
    );

    /// The SAN and Fulcio issuer of the golden leaf, as the test stack minted it.
    const GOLDEN_IDENTITY: &str = "ocx-test@example.com";
    const GOLDEN_ISSUER: &str = "http://dex:5556/dex";
    /// The tlog entry's `integratedTime`. The certificate expired ten minutes
    /// after capture, so this — never a clock — is what the validity window is
    /// anchored to (`super::signing_instant`).
    const GOLDEN_INTEGRATED_TIME: u64 = 1_787_969_275;

    fn golden_referrer_field(pointer: &str) -> String {
        let manifest: serde_json::Value =
            serde_json::from_str(GOLDEN_KEYLESS_REFERRER).expect("golden referrer manifest is JSON");
        manifest
            .pointer(pointer)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_else(|| panic!("golden referrer manifest carries {pointer}"))
            .to_owned()
    }

    /// Ties [`GOLDEN_SUBJECT_MANIFEST`] to the committed fixture before anything
    /// is built on it. Without this the verification below could pass against
    /// bytes nobody checked, and a regenerated fixture would move the goalposts
    /// silently — the same guard `tlog`'s window fixture carries.
    #[test]
    fn the_golden_subject_bytes_are_the_ones_cosign_signed() {
        assert_eq!(
            crate::oci::Algorithm::Sha256
                .hash(GOLDEN_SUBJECT_MANIFEST.as_bytes())
                .to_string(),
            golden_referrer_field("/subject/digest"),
            "the reproduced subject manifest must hash to the digest cosign's referrer names",
        );
    }

    /// **S-002.** A cosign-written keyless DSSE image signature verifies through
    /// OCX's *signature* path, offline, against the committed trust root.
    ///
    /// This is the whole gate, not a parse test: `verify_one_referrer` pulls the
    /// bundle blob, routes it by predicateType (D2), runs `dsse::verify_envelope`
    /// for the subject binding, hands the bundle to `sigstore`'s verifier for the
    /// Fulcio chain + embedded SCT + DSSE signature, checks the Rekor SET and the
    /// Merkle inclusion proof against the pinned log key, binds the logged body to
    /// the verified payload, matches the certificate against a trust policy, and
    /// anchors the certificate-validity window to the entry's `integratedTime`.
    /// Every key it needs is committed, so nothing here touches the network or a
    /// container.
    ///
    /// The leaf certificate **is expired** — its window was about ten minutes,
    /// long past. That is the point: a wall-clock validity check refuses this
    /// bundle, which is why `SigningInstant` exists and why
    /// `signing_instant::tests::the_certificate_validity_path_reads_no_clock`
    /// fails the build on a `now()` under `src/oci/verify/`.
    #[tokio::test]
    async fn cosigns_own_keyless_dsse_signature_verifies_through_the_signature_path() {
        use crate::oci::client::test_transport::{StubTransport, StubTransportData};

        let subject_bytes = GOLDEN_SUBJECT_MANIFEST.as_bytes();
        let subject_digest = crate::oci::Algorithm::Sha256.hash(subject_bytes);

        // The committed bundle is pretty-printed, so its digest is not the one
        // cosign's referrer names; the referrer is rebuilt around these bytes
        // and carries the predicateType annotation cosign wrote, which the
        // annotation-direction check then cross-examines.
        let blob = GOLDEN_KEYLESS_BUNDLE.as_bytes().to_vec();
        let blob_digest = crate::oci::Algorithm::Sha256.hash(&blob);
        let annotated_predicate_type = golden_referrer_field("/annotations/dev.sigstore.bundle.predicateType");

        let data = StubTransportData::new();
        data.write().blobs.insert(blob_digest.to_string(), blob.clone());
        let transport = StubTransport::new(data);
        let image: native::Reference = "registry.example/repo:latest".parse().expect("stub reference");

        let trust_root =
            TrustRoot::load_trusted_root_json(GOLDEN_TRUSTED_ROOT.as_bytes()).expect("the committed trust root loads");
        let verifier = Verifier::new(RekorConfiguration::default(), trust_root.clone()).expect("verifier");
        let policies = [crate::trust::CompiledPolicy {
            builder: None,
            backends: vec![crate::trust::PolicyBackend::Keyless(crate::trust::CompiledKeyless {
                identity: crate::trust::IdentityRule::Exact(GOLDEN_IDENTITY.to_string()),
                issuer: GOLDEN_ISSUER.to_string(),
            })],
        }];
        let identifier = verify_id();
        let index = Index::from_impl(IndirectingIndex {
            physical: identifier.clone(),
        });
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = StateStore::new(temp.path());
        let ctx = VerifyContext {
            identifier: &identifier,
            platform: None,
            policies: &policies,
            no_cache: true,
            index: &index,
            trust_root: &trust_root,
            rekor_url: &rekor_url,
            state: &state,
            // No Sigstore network at all: the Rekor key must come from the
            // committed root. A rekor_url that resolves to nothing is the
            // second half of that proof — a fetch here would fail, not pass.
            offline: true,
            content: VerifyContentMode::Signature,
            verification: VerificationMode::Demand,
            signature_format: None,
            allow_unlogged_signature: false,
            report_all: false,
        };

        let (descriptor, bytes) = referrer_with(
            &subject_digest,
            &blob_digest,
            blob.len() as i64,
            Some(&annotated_predicate_type),
        );
        let mut budget = ScanBudget::new(ctx.content.caps());
        let outcome = VerifyPipeline::verify_one_referrer(
            &transport,
            &ctx,
            &verifier,
            &descriptor,
            bytes,
            &subject_digest,
            subject_bytes,
            &image,
            crate::oci::verify::DiscoveryMethod::ReferrersApi,
            &mut budget,
            &RekorKeyMemo::default(),
        )
        .await;

        let Ok(CandidateOutcome::Verified { verified, attestation }) = outcome else {
            panic!("cosign's own keyless signature must verify in signature mode, got: {outcome:?}");
        };
        let result = verified.result;
        // The identity and issuer are read back off the verified leaf, so these
        // two assert that the certificate that passed the chain, the SCT and the
        // signature is the one the test stack minted — not merely that some
        // candidate returned Ok.
        assert_eq!(result.certificate_identity.as_deref(), Some(GOLDEN_IDENTITY));
        assert_eq!(result.certificate_oidc_issuer.as_deref(), Some(GOLDEN_ISSUER));
        // `signed_at` is the tlog entry's `integratedTime`, which is also the
        // instant the (expired) certificate's validity window was checked at.
        assert_eq!(result.signed_at, Some(GOLDEN_INTEGRATED_TIME));
        // A certificate signed it, so the reported backend is the keyless one —
        // the field that tells a key-mode result apart from this one.
        assert_eq!(result.key_backend, KeyBackendKind::Keyless);
        // The subject binding came out of the signed Statement, not the caller's
        // argument, and the predicateType is cosign's image-signature one — the
        // two facts that make this a *signature* rather than an attestation.
        let attestation = attestation.expect("signature mode reads the DSSE statement since D2");
        assert_eq!(attestation.predicate_type, COSIGN_SIGN_PREDICATE_TYPE);
        assert_eq!(attestation.subject_digest, subject_digest);
        assert_eq!(
            attestation.predicate.get(),
            "{}",
            "cosign writes an empty predicate on an image signature (F4)",
        );
    }

    // ── S-003: cosign's own KEY-mode DSSE image signature ──────────────────

    /// cosign v3.1.1's **key-mode** signature bundle, captured in G0. Its
    /// `verificationMaterial` is a `publicKey` + hint with no certificate
    /// anywhere, which is the whole shape under test.
    const GOLDEN_KEY_BUNDLE: &str = include_str!("../../../../../test/tests/fixtures/golden/key_bundle.json");

    /// The public half of the cosign key pair that signed it. `include_str!` so
    /// a moved fixture is a compile error, same as every constant above.
    const GOLDEN_PUBLIC_KEY_PEM: &str = include_str!("../../../../../test/tests/fixtures/golden/keys/cosign.pub");

    /// A second, unrelated SPKI public key — the local Rekor log's. Used as
    /// "a key the policy trusts that did not sign this artifact", so that case
    /// is exercised against a real key rather than a synthesized one.
    const UNRELATED_PUBLIC_KEY_PEM: &str = include_str!("../../../../../test/sigstore/keys/rekor.pub.pem");

    /// The referrer manifest cosign pushed alongside the key bundle — the
    /// source of its predicateType annotation, so nothing is transcribed.
    const GOLDEN_KEY_REFERRER: &str =
        include_str!("../../../../../test/tests/fixtures/golden/key_referrer_manifest.json");

    fn golden_key_referrer_field(pointer: &str) -> String {
        let manifest: serde_json::Value =
            serde_json::from_str(GOLDEN_KEY_REFERRER).expect("golden key referrer manifest is JSON");
        manifest
            .pointer(pointer)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_else(|| panic!("golden key referrer manifest carries {pointer}"))
            .to_owned()
    }

    /// The same tie-down `the_golden_subject_bytes_are_the_ones_cosign_signed`
    /// performs for the keyless capture. Without it the key-mode test could
    /// verify a statement bound to some *other* subject and still pass, because
    /// the harness supplies both halves.
    #[test]
    fn the_key_bundle_signs_the_same_subject_the_keyless_one_does() {
        assert_eq!(
            crate::oci::Algorithm::Sha256
                .hash(GOLDEN_SUBJECT_MANIFEST.as_bytes())
                .to_string(),
            golden_key_referrer_field("/subject/digest"),
            "the reproduced subject manifest must hash to the digest cosign's key-mode referrer names",
        );
    }

    fn key_policy(pem: &str) -> crate::trust::CompiledPolicy {
        crate::trust::CompiledPolicy {
            builder: None,
            backends: vec![crate::trust::PolicyBackend::Key(
                sigstore::crypto::CosignVerificationKey::try_from_pem(pem.as_bytes())
                    .expect("the fixture is an SPKI PEM"),
            )],
        }
    }

    fn golden_keyless_policy() -> crate::trust::CompiledPolicy {
        crate::trust::CompiledPolicy {
            builder: None,
            backends: vec![crate::trust::PolicyBackend::Keyless(crate::trust::CompiledKeyless {
                identity: crate::trust::IdentityRule::Exact(GOLDEN_IDENTITY.to_string()),
                issuer: GOLDEN_ISSUER.to_string(),
            })],
        }
    }

    /// Strip `verificationMaterial.tlogEntries`, leaving everything else the
    /// bundle carries intact.
    ///
    /// One helper for both halves of D10's asymmetry on purpose: the key case
    /// and the keyless case are then provably asked the *same* question, and
    /// the only thing that differs between them is the verification material.
    /// The `expect` is the guard that keeps this a mutation rather than a no-op
    /// — a fixture regenerated without a tlog entry must break the harness, not
    /// silently turn both tests into duplicates of their siblings.
    fn without_tlog_entries(bundle_json: &str) -> String {
        let mut bundle: serde_json::Value = serde_json::from_str(bundle_json).expect("golden bundle is JSON");
        bundle
            .get_mut("verificationMaterial")
            .and_then(serde_json::Value::as_object_mut)
            .expect("a bundle carries verificationMaterial")
            .remove("tlogEntries")
            .expect("both golden bundles carry a tlog entry (F4)");
        bundle.to_string()
    }

    /// Splice the *keyless* capture's Signed Entry Timestamp onto the key
    /// bundle's tlog entry.
    ///
    /// A real, well-formed SET over a different log entry, rather than random
    /// bytes: that keeps the refusal attributable to the signature check rather
    /// than to a DER parse, which is the difference between proving the SET is
    /// verified and proving it is merely decoded.
    fn with_a_foreign_rekor_set(bundle_json: &str) -> String {
        let foreign: serde_json::Value =
            serde_json::from_str(GOLDEN_KEYLESS_BUNDLE).expect("the keyless bundle is JSON");
        let foreign_set =
            foreign["verificationMaterial"]["tlogEntries"][0]["inclusionPromise"]["signedEntryTimestamp"].clone();
        let mut bundle: serde_json::Value = serde_json::from_str(bundle_json).expect("golden bundle is JSON");
        let set = &mut bundle["verificationMaterial"]["tlogEntries"][0]["inclusionPromise"]["signedEntryTimestamp"];
        assert!(
            set.is_string() && *set != foreign_set,
            "the splice must actually change the SET, or this proves nothing",
        );
        *set = foreign_set;
        bundle.to_string()
    }

    /// Drive `verify_one_referrer` over one bundle blob, offline, against the
    /// committed trust root.
    ///
    /// Extracted from S-002's body rather than duplicated per case: every test
    /// below differs only in the bundle bytes and the policy set, and a second
    /// hand-built context is how the two halves of an asymmetry end up being
    /// compared under quietly different conditions.
    async fn verify_golden_candidate(
        bundle_json: &str,
        annotated_predicate_type: &str,
        policies: &[crate::trust::CompiledPolicy],
    ) -> Result<CandidateOutcome, VerifyErrorKind> {
        use crate::oci::client::test_transport::{StubTransport, StubTransportData};

        let subject_bytes = GOLDEN_SUBJECT_MANIFEST.as_bytes();
        let subject_digest = crate::oci::Algorithm::Sha256.hash(subject_bytes);
        let blob = bundle_json.as_bytes().to_vec();
        let blob_digest = crate::oci::Algorithm::Sha256.hash(&blob);

        let data = StubTransportData::new();
        data.write().blobs.insert(blob_digest.to_string(), blob.clone());
        let transport = StubTransport::new(data);
        let image: native::Reference = "registry.example/repo:latest".parse().expect("stub reference");

        let trust_root =
            TrustRoot::load_trusted_root_json(GOLDEN_TRUSTED_ROOT.as_bytes()).expect("the committed trust root loads");
        let verifier = Verifier::new(RekorConfiguration::default(), trust_root.clone()).expect("verifier");
        let identifier = verify_id();
        let index = Index::from_impl(IndirectingIndex {
            physical: identifier.clone(),
        });
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = StateStore::new(temp.path());
        let ctx = VerifyContext {
            identifier: &identifier,
            platform: None,
            policies,
            no_cache: true,
            index: &index,
            trust_root: &trust_root,
            rekor_url: &rekor_url,
            state: &state,
            // No Sigstore network at all: the Rekor key must come from the
            // committed root, and a `rekor_url` that resolves to nothing is the
            // second half of that proof.
            offline: true,
            content: VerifyContentMode::Signature,
            verification: VerificationMode::Demand,
            signature_format: None,
            allow_unlogged_signature: false,
            report_all: false,
        };

        let (descriptor, bytes) = referrer_with(
            &subject_digest,
            &blob_digest,
            blob.len() as i64,
            Some(annotated_predicate_type),
        );
        let mut budget = ScanBudget::new(ctx.content.caps());
        VerifyPipeline::verify_one_referrer(
            &transport,
            &ctx,
            &verifier,
            &descriptor,
            bytes,
            &subject_digest,
            subject_bytes,
            &image,
            crate::oci::verify::DiscoveryMethod::ReferrersApi,
            &mut budget,
            &RekorKeyMemo::default(),
        )
        .await
    }

    /// **S-003.** cosign's own key-mode DSSE image signature verifies through
    /// OCX's signature path, offline, against a pinned public key.
    ///
    /// This is the whole WP9 gate, not a parse test: the bundle carries a
    /// `publicKey` hint and no certificate, so `sigstore::bundle::verify` — which
    /// reads its key exclusively out of a leaf certificate — cannot answer for
    /// it at all. What has to run instead is the DSSE signature check against
    /// the key `--key` named, plus the Rekor SET and Merkle proof, and none of
    /// the certificate machinery.
    #[tokio::test]
    async fn cosigns_own_key_mode_dsse_signature_verifies_against_a_pinned_public_key() {
        let outcome = verify_golden_candidate(
            GOLDEN_KEY_BUNDLE,
            &golden_key_referrer_field("/annotations/dev.sigstore.bundle.predicateType"),
            &[key_policy(GOLDEN_PUBLIC_KEY_PEM)],
        )
        .await;

        let Ok(CandidateOutcome::Verified { verified, attestation }) = outcome else {
            panic!("cosign's own key-mode signature must verify in signature mode, got: {outcome:?}");
        };
        let result = verified.result;
        // The reported backend is the file one, which is what distinguishes this
        // verdict from S-002's — both return `Verified`, and only this field
        // says which of the two paths produced it.
        assert_eq!(result.key_backend, KeyBackendKind::File);
        // Absent, never empty: there is no certificate, so there is no identity
        // and no issuer to read. An empty string here would report "signed by
        // nobody" as a fact about a certificate that does not exist.
        assert_eq!(result.certificate_identity, None);
        assert_eq!(result.certificate_oidc_issuer, None);
        // The instant is the tlog entry's `integratedTime` — the key bundle
        // carries one, so the transparency evidence was checked in full.
        assert_eq!(result.signed_at, Some(GOLDEN_INTEGRATED_TIME));
        assert_eq!(
            result.subject_digest.to_string(),
            golden_key_referrer_field("/subject/digest")
        );
        // The subject binding came out of the signed Statement, and the
        // predicateType is cosign's image-signature one: together they say a
        // *signature* over *this* artifact verified, not merely that some
        // candidate returned `Ok`.
        let attestation = attestation.expect("signature mode reads the DSSE statement since D2");
        assert_eq!(attestation.predicate_type, COSIGN_SIGN_PREDICATE_TYPE);
        assert_eq!(
            attestation.subject_digest.to_string(),
            golden_key_referrer_field("/subject/digest")
        );
    }

    /// D10, the half that must be **accepted**: `cosign sign --key` uploads to
    /// Rekor only when asked, so a key-mode bundle with no `tlogEntries`
    /// verifies. It reports no `signed_at`, because there is no logged instant
    /// to report and none is invented.
    #[tokio::test]
    async fn a_key_mode_bundle_with_no_transparency_entry_verifies() {
        let outcome = verify_golden_candidate(
            &without_tlog_entries(GOLDEN_KEY_BUNDLE),
            &golden_key_referrer_field("/annotations/dev.sigstore.bundle.predicateType"),
            &[key_policy(GOLDEN_PUBLIC_KEY_PEM)],
        )
        .await;

        let Ok(CandidateOutcome::Verified { verified, .. }) = outcome else {
            panic!("a key signature with no Rekor entry must verify, got: {outcome:?}");
        };
        let result = verified.result;
        assert_eq!(result.key_backend, KeyBackendKind::File);
        assert_eq!(
            result.signed_at, None,
            "with no transparency entry there is no integratedTime to report",
        );
    }

    /// The key arm runs the transparency checks it does not *require*. When a
    /// tlog entry is present its SET is verified against the pinned log key,
    /// exactly as on the keyless arm — otherwise "optional evidence" and
    /// "unchecked evidence" would be indistinguishable, and a key bundle
    /// carrying a spliced Rekor entry would pass on the DSSE signature alone.
    #[tokio::test]
    async fn a_key_mode_bundle_with_a_foreign_rekor_set_is_refused() {
        let outcome = verify_golden_candidate(
            &with_a_foreign_rekor_set(GOLDEN_KEY_BUNDLE),
            &golden_key_referrer_field("/annotations/dev.sigstore.bundle.predicateType"),
            &[key_policy(GOLDEN_PUBLIC_KEY_PEM)],
        )
        .await;

        assert!(
            matches!(outcome, Err(VerifyErrorKind::RekorSetInvalid)),
            "a SET signed over another log entry must refuse the candidate: {outcome:?}",
        );
    }

    /// D10, the half that must be **refused**, and the reason the previous test
    /// is not a weakening: the *same* stripping applied to the keyless capture
    /// still fails closed. A keyless signature's only proof of when it was made
    /// is the log entry, and its certificate lived about ten minutes.
    #[tokio::test]
    async fn a_keyless_bundle_with_no_transparency_entry_is_refused() {
        let outcome = verify_golden_candidate(
            &without_tlog_entries(GOLDEN_KEYLESS_BUNDLE),
            COSIGN_SIGN_PREDICATE_TYPE,
            &[golden_keyless_policy()],
        )
        .await;

        assert!(
            matches!(outcome, Err(VerifyErrorKind::RekorSetInvalid)),
            "a keyless bundle stripped of its tlog entry must be refused: {outcome:?}",
        );
    }

    /// D5 in the key direction: a policy naming only keyless signers names
    /// nobody who signs with a key, so a key-signed artifact must not satisfy
    /// it. Reading a keyless matcher as "no objection" is exactly how a policy
    /// stops meaning anything.
    #[tokio::test]
    async fn a_key_signed_bundle_under_an_all_keyless_policy_is_refused() {
        let outcome = verify_golden_candidate(
            GOLDEN_KEY_BUNDLE,
            &golden_key_referrer_field("/annotations/dev.sigstore.bundle.predicateType"),
            &[golden_keyless_policy()],
        )
        .await;

        assert!(
            matches!(outcome, Err(VerifyErrorKind::IdentityMismatch)),
            "a keyless-only policy must refuse a key signature (77): {outcome:?}",
        );
    }

    /// The signature is checked against the policy's key, not merely parsed
    /// beside it: a real, well-formed public key that did not produce this
    /// signature refuses the candidate.
    #[tokio::test]
    async fn a_key_bundle_signed_by_another_key_is_refused() {
        let outcome = verify_golden_candidate(
            GOLDEN_KEY_BUNDLE,
            &golden_key_referrer_field("/annotations/dev.sigstore.bundle.predicateType"),
            &[key_policy(UNRELATED_PUBLIC_KEY_PEM)],
        )
        .await;

        assert!(
            matches!(outcome, Err(VerifyErrorKind::SignatureInvalid)),
            "a key that did not sign this envelope must refuse it (65): {outcome:?}",
        );
    }

    /// The other direction of the same gate, through the same driver: a bundle
    /// whose content is a `messageSignature` — the shape OCX itself wrote before
    /// cosign parity — is refused, and refused as the *other kind* so it charges
    /// bytes without spending a candidate slot.
    #[tokio::test]
    async fn a_message_signature_bundle_is_no_longer_read_in_signature_mode() {
        use crate::oci::client::test_transport::{StubTransport, StubTransportData};

        let subject_bytes = b"the artifact under signature";
        let subject_digest = crate::oci::Algorithm::Sha256.hash(subject_bytes);
        let blob = serde_json::to_vec(&message_bundle(true, true)).expect("bundle serializes");
        let blob_digest = crate::oci::Algorithm::Sha256.hash(&blob);

        let data = StubTransportData::new();
        data.write().blobs.insert(blob_digest.to_string(), blob.clone());
        let transport = StubTransport::new(data);
        let image: native::Reference = "registry.example/repo:latest".parse().expect("stub reference");

        let ca_der = super::super::tlog::fixture_certificate_der();
        let trust_root = trust_root_of(&[&ca_der]);
        let verifier = Verifier::new(RekorConfiguration::default(), trust_root.clone()).expect("verifier");
        let identifier = verify_id();
        let index = Index::from_impl(IndirectingIndex {
            physical: identifier.clone(),
        });
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = StateStore::new(temp.path());
        let ctx = VerifyContext {
            content: VerifyContentMode::Signature,
            ..attestation_ctx(&identifier, &index, &trust_root, &rekor_url, &state)
        };

        let (descriptor, bytes) = referrer_with(&subject_digest, &blob_digest, blob.len() as i64, None);
        let mut budget = ScanBudget::new(ctx.content.caps());
        let outcome = VerifyPipeline::verify_one_referrer(
            &transport,
            &ctx,
            &verifier,
            &descriptor,
            bytes,
            &subject_digest,
            subject_bytes,
            &image,
            crate::oci::verify::DiscoveryMethod::ReferrersApi,
            &mut budget,
            &RekorKeyMemo::default(),
        )
        .await;

        assert!(
            matches!(outcome, Ok(CandidateOutcome::ModeMismatch)),
            "a messageSignature answers neither question now, got: {outcome:?}",
        );
    }

    /// The per-candidate bundle cap, named for the mode that tripped it. An SBOM
    /// near the ceiling is a real authoring outcome, so attestation mode reports
    /// the bound and the size; signature mode keeps the kind `ocx package verify`
    /// has always reported for this shape.
    ///
    /// The declared size is refused before any fetch, which is the point: the
    /// stub holds no blob at all, so a check that ran after the download would
    /// fail with the transport's error instead.
    #[tokio::test]
    async fn an_over_cap_bundle_layer_is_refused_before_it_is_fetched() {
        use crate::oci::client::test_transport::{StubTransport, StubTransportData};

        let transport = StubTransport::new(StubTransportData::new());
        let image: native::Reference = "registry.example/repo:latest".parse().expect("stub reference");
        let subject_digest = crate::oci::Algorithm::Sha256.hash(b"subject");
        let blob_digest = crate::oci::Algorithm::Sha256.hash(b"a bundle nobody will fetch");
        let ca_der = super::super::tlog::fixture_certificate_der();
        let trust_root = trust_root_of(&[&ca_der]);
        let verifier = Verifier::new(RekorConfiguration::default(), trust_root.clone()).expect("verifier");
        let identifier = verify_id();
        let index = Index::from_impl(IndirectingIndex {
            physical: identifier.clone(),
        });
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = StateStore::new(temp.path());

        let attestation_caps = VerifyContentMode::Attestation { predicate_type: None }.caps();
        let oversize = attestation_caps.bundle_bytes as i64 + 1;
        let (descriptor, bytes) = referrer_with(&subject_digest, &blob_digest, oversize, None);

        for (content, expected) in [
            (
                VerifyContentMode::Attestation { predicate_type: None },
                VerifyErrorKind::AttestationTooLarge {
                    limit: attestation_caps.bundle_bytes as u64,
                    actual: oversize as u64,
                },
            ),
            (VerifyContentMode::Signature, VerifyErrorKind::BundleParseFailed),
        ] {
            let ctx = VerifyContext {
                identifier: &identifier,
                platform: None,
                policies: &[],
                no_cache: true,
                index: &index,
                trust_root: &trust_root,
                rekor_url: &rekor_url,
                state: &state,
                offline: true,
                content: content.clone(),
                verification: VerificationMode::Demand,
                signature_format: None,
                allow_unlogged_signature: false,
                report_all: false,
            };
            let mut budget = ScanBudget::new(ctx.content.caps());
            let verdict = VerifyPipeline::verify_one_referrer(
                &transport,
                &ctx,
                &verifier,
                &descriptor,
                bytes.clone(),
                &subject_digest,
                b"subject",
                &image,
                crate::oci::verify::DiscoveryMethod::ReferrersApi,
                &mut budget,
                &RekorKeyMemo::default(),
            )
            .await;
            let error = verdict.expect_err("an over-cap bundle layer is never verified");
            assert_eq!(
                error.to_string(),
                expected.to_string(),
                "{content:?} must name its own bound",
            );
            assert_eq!(
                budget.spent, 0,
                "nothing was fetched, so nothing may be charged to the budget",
            );
        }
    }

    /// Each truncating bound carries the limit it tripped, not just its name.
    /// The sibling test above pins which variant each `ScanStop` maps to; this
    /// pins the number inside it, which a swapped `caps` field leaves green.
    #[test]
    fn a_truncated_attestation_scan_reports_the_limit_it_tripped() {
        let identifier = verify_id();
        let index = Index::from_impl(IndirectingIndex {
            physical: identifier.clone(),
        });
        let trust_root = trust_root_of(&[]);
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = StateStore::new(temp.path());
        let ctx = attestation_ctx(&identifier, &index, &trust_root, &rekor_url, &state);
        let caps = ctx.content.caps();

        let stopped_at = |stop: ScanStop| {
            let mut budget = ScanBudget::new(caps);
            budget.stop = Some(stop);
            budget.considered = 2;
            VerifyPipeline::finish_scan(&ctx, caps, 5, &budget, Vec::new(), Vec::new())
                .expect_err("a truncated scan never returns a partial list")
        };

        assert!(
            matches!(
                stopped_at(ScanStop::CandidateCap),
                VerifyErrorKind::TooManyAttestations { limit } if limit == caps.candidates,
            ),
            "the candidate cap reports the candidate ceiling",
        );
        assert!(
            matches!(
                stopped_at(ScanStop::ByteBudget),
                VerifyErrorKind::AttestationBudgetExhausted { limit } if limit == caps.total_bytes,
            ),
            "the byte budget reports the byte ceiling",
        );
        assert!(
            matches!(
                stopped_at(ScanStop::ListingCap),
                VerifyErrorKind::CandidateLimitExhausted { unexamined } if unexamined == 3,
            ),
            "the listing backstop reports how many candidates were left unlooked-at",
        );
    }

    // ── Unsigned SBOM referrers (`cosign attach sbom` shape) ────────────────

    /// One referrer the SBOM transport serves, described by what a publisher
    /// would have chosen: its artifact type, its layer's media type, and the
    /// document bytes.
    #[derive(Clone)]
    struct StubReferrer {
        artifact_type: String,
        layer_media_type: String,
        document: Vec<u8>,
        /// Payload layers **after** the first, appended to the manifest the
        /// production builder wrote.
        ///
        /// The builder writes exactly one layer, so this is the shape it cannot
        /// produce and a registry can serve anyway: the OCI image-manifest
        /// schema bounds `layers` below at one and not above (C-024).
        extra_documents: Vec<Vec<u8>>,
        /// Overrides the layer's declared size, so a cap test can lie about it
        /// the way a hostile registry would.
        declared_layer_size: Option<i64>,
    }

    impl StubReferrer {
        /// An unsigned SBOM referrer: the document is the payload, and the
        /// artifact type and the layer media type agree.
        fn sbom(media_type: &str, document: &str) -> Self {
            Self {
                artifact_type: media_type.to_string(),
                layer_media_type: media_type.to_string(),
                document: document.as_bytes().to_vec(),
                extra_documents: Vec::new(),
                declared_layer_size: None,
            }
        }

        /// An unsigned SBOM referrer whose listing entry and payload layer
        /// disagree about what the document is.
        ///
        /// Nothing prevents a registry from serving this: the `artifactType` on
        /// a referrers listing is never checked against the manifest it points
        /// at, so the two are independent claims and only the layer's is about
        /// the bytes.
        fn mislabelled_sbom(artifact_type: &str, layer_media_type: &str, document: &str) -> Self {
            Self {
                artifact_type: artifact_type.to_string(),
                layer_media_type: layer_media_type.to_string(),
                document: document.as_bytes().to_vec(),
                extra_documents: Vec::new(),
                declared_layer_size: None,
            }
        }

        /// A Sigstore-bundle referrer carrying a real DSSE envelope over an
        /// in-toto statement binding this transport's subject.
        ///
        /// Structurally sound and cryptographically worthless - the signature
        /// is four bytes of 0xDEADBEEF. That is exactly the fixture the
        /// permissive mode needs: it must extract this payload, and it must
        /// never be able to call it verified.
        fn attestation_bundle(predicate_type: &str, predicate: &str) -> Self {
            let bundle = dsse_bundle_binding(&indirection_subject_digest(), predicate_type, predicate);
            Self {
                artifact_type: SIGSTORE_BUNDLE_V03.to_string(),
                layer_media_type: crate::oci::sign::bundle::BUNDLE_V03_MEDIA_TYPE.to_string(),
                document: serde_json::to_vec(&bundle).expect("bundle serializes"),
                extra_documents: Vec::new(),
                declared_layer_size: None,
            }
        }

        /// A Sigstore-bundle referrer whose blob is junk, which is how far a
        /// unit test can drive the signed pass: it reaches `parse_bundle` and
        /// fail-closes into `BundleParseFailed`.
        fn junk_bundle() -> Self {
            Self {
                artifact_type: SIGSTORE_BUNDLE_V03.to_string(),
                layer_media_type: crate::oci::sign::bundle::BUNDLE_V03_MEDIA_TYPE.to_string(),
                document: STUB_BUNDLE_BLOB.to_vec(),
                extra_documents: Vec::new(),
                declared_layer_size: None,
            }
        }

        /// Hang a second payload layer off this referrer.
        fn with_extra_document(mut self, document: &str) -> Self {
            self.extra_documents.push(document.as_bytes().to_vec());
            self
        }

        fn document_digest(&self) -> Digest {
            crate::oci::Algorithm::Sha256.hash(&self.document)
        }

        /// The bytes this referrer serves under `digest`, across every layer it
        /// declares.
        fn blob_for(&self, digest: &Digest) -> Option<Vec<u8>> {
            std::iter::once(&self.document)
                .chain(self.extra_documents.iter())
                .find(|body| &crate::oci::Algorithm::Sha256.hash(body) == digest)
                .cloned()
        }

        /// Built through the production builder, so the fixture cannot drift
        /// from the shape the read path parses.
        fn manifest_bytes(&self) -> Vec<u8> {
            let payload = crate::oci::Descriptor {
                media_type: self.layer_media_type.clone(),
                digest: self.document_digest().to_string(),
                size: self.declared_layer_size.unwrap_or(self.document.len() as i64),
                ..crate::oci::Descriptor::default()
            };
            let subject = crate::oci::Descriptor {
                media_type: crate::oci::OCI_IMAGE_MEDIA_TYPE.to_string(),
                digest: indirection_subject_digest().to_string(),
                size: INDIRECTION_SUBJECT_MANIFEST.len() as i64,
                ..crate::oci::Descriptor::default()
            };
            let built = crate::oci::referrer::ReferrerManifest::build(subject, &self.artifact_type, payload, None)
                .to_canonical_json()
                .expect("referrer manifest json");
            if self.extra_documents.is_empty() {
                return built;
            }
            // Spliced onto the builder's own output rather than hand-written, so
            // the extra layers differ from the real one in nothing but their
            // digest and size.
            let mut manifest: serde_json::Value =
                serde_json::from_slice(&built).expect("the built referrer manifest is JSON");
            let template = manifest["layers"][0].clone();
            let mut layers = vec![template.clone()];
            for document in &self.extra_documents {
                let mut layer = template.clone();
                layer["digest"] = serde_json::json!(crate::oci::Algorithm::Sha256.hash(document).to_string());
                layer["size"] = serde_json::json!(document.len());
                layers.push(layer);
            }
            manifest["layers"] = serde_json::Value::Array(layers);
            serde_json::to_vec(&manifest).expect("the spliced referrer manifest serializes")
        }

        fn descriptor(&self) -> crate::oci::Descriptor {
            let bytes = self.manifest_bytes();
            crate::oci::Descriptor {
                media_type: crate::oci::OCI_IMAGE_MEDIA_TYPE.to_string(),
                digest: crate::oci::Algorithm::Sha256.hash(&bytes).to_string(),
                size: bytes.len() as i64,
                artifact_type: Some(self.artifact_type.clone()),
                ..crate::oci::Descriptor::default()
            }
        }
    }

    /// What the `sha256-<hex>.sbom` tag serves: one manifest and the blob each
    /// of its layers names.
    ///
    /// Deliberately not a [`StubReferrer`]: that type builds a *referrer*
    /// manifest, with an `artifactType` and a `subject`, and a `.sbom` sidecar
    /// has neither. Reusing it would let these tests pass against a shape cosign
    /// does not write.
    #[derive(Clone)]
    struct StubSbomSidecar {
        manifest: Vec<u8>,
        /// One document per manifest layer, in manifest order.
        documents: Vec<Vec<u8>>,
    }

    impl StubSbomSidecar {
        fn digest(&self) -> Digest {
            crate::oci::Algorithm::Sha256.hash(&self.manifest)
        }

        /// The document the manifest's layers name under `digest`, matched by
        /// position: the digest is read out of the manifest rather than
        /// recomputed from the body, so a fixture whose layer digest does not
        /// cover its document is served exactly as inconsistently as a registry
        /// would serve it, instead of being silently repaired here.
        fn document_for(&self, digest: &str) -> Option<Vec<u8>> {
            let manifest: ImageManifest = serde_json::from_slice(&self.manifest).expect("sidecar manifest parses");
            let position = manifest.layers.iter().position(|layer| layer.digest == digest)?;
            Some(
                self.documents
                    .get(position)
                    .expect("the fixture serves one document per layer")
                    .clone(),
            )
        }
    }

    /// Serves a caller-chosen referrer set — and, when one is hung on it, the
    /// `sha256-<hex>.sbom` sidecar tag — recording what was asked for.
    ///
    /// Honours the server-side `artifactType` filter, unlike the
    /// spec-permitted registry that ignores it — which is the point: the signed
    /// pass must ask for bundles and get only bundles, so an unsigned referrer
    /// reaching a verification candidate would be this double's fault to expose
    /// and not to hide.
    #[derive(Clone)]
    struct SbomTransport {
        referrers: Vec<StubReferrer>,
        /// `None` — the overwhelmingly common case — is a subject with no
        /// `.sbom` tag, which the transport answers with `ManifestNotFound`.
        sidecar: Option<StubSbomSidecar>,
        listing_filters: std::sync::Arc<std::sync::Mutex<Vec<Option<String>>>>,
        /// Every subject digest a referrer listing was addressed with, in call
        /// order. C-011's second pass is otherwise invisible from outside:
        /// this double serves one referrer set regardless of subject, so only
        /// the addressing says how many subjects were read.
        listed_subjects: std::sync::Arc<std::sync::Mutex<Vec<Digest>>>,
        pulled_blobs: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl SbomTransport {
        fn new(referrers: Vec<StubReferrer>) -> Self {
            Self {
                referrers,
                sidecar: None,
                listing_filters: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
                listed_subjects: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
                pulled_blobs: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            }
        }

        /// Hang a `sha256-<hex>.sbom` sidecar off the subject, one document
        /// per layer the manifest declares.
        fn with_sidecar(mut self, manifest: &[u8], documents: &[&[u8]]) -> Self {
            self.sidecar = Some(StubSbomSidecar {
                manifest: manifest.to_vec(),
                documents: documents.iter().map(|body| body.to_vec()).collect(),
            });
            self
        }

        fn listed_subjects(&self) -> Vec<Digest> {
            self.listed_subjects.lock().expect("recorder lock").clone()
        }

        fn pulled_blobs(&self) -> Vec<String> {
            self.pulled_blobs.lock().expect("recorder lock").clone()
        }

        fn listing_filters(&self) -> Vec<Option<String>> {
            self.listing_filters.lock().expect("recorder lock").clone()
        }
    }

    #[async_trait::async_trait]
    impl OciTransport for SbomTransport {
        async fn ensure_auth(
            &self,
            _: &native::Reference,
            _: crate::oci::RegistryOperation,
        ) -> std::result::Result<(), ClientError> {
            Ok(())
        }

        async fn list_tags(
            &self,
            _: &native::Reference,
            _: usize,
            _: Option<String>,
        ) -> std::result::Result<Vec<String>, ClientError> {
            unimplemented!("the sbom scan never lists tags")
        }

        async fn catalog(
            &self,
            _: &native::Reference,
            _: usize,
            _: Option<String>,
        ) -> std::result::Result<Vec<String>, ClientError> {
            unimplemented!("the sbom scan never reads the catalog")
        }

        async fn fetch_manifest_digest(&self, _: &native::Reference) -> std::result::Result<String, ClientError> {
            unimplemented!("the sbom scan resolves digests through the index")
        }

        async fn pull_manifest_raw(
            &self,
            image: &native::Reference,
            _: &[&str],
        ) -> std::result::Result<(Vec<u8>, String), ClientError> {
            let subject = indirection_subject_digest();
            if image.digest() == Some(subject.to_string().as_str()) {
                return Ok((INDIRECTION_SUBJECT_MANIFEST.to_vec(), subject.to_string()));
            }
            // Tag-addressed: the `sha256-<hex>.att` and `.sbom` sidecar doors.
            // The `.sbom` one is served when a test hung a sidecar on the
            // subject; everything else 404s, which is what a real registry does
            // and what the readers must read as "no legacy artifact" —
            // modelled here rather than panicked on, so a scan that
            // legitimately tries a door does not fail a test about something
            // else. Every *digest*-addressed read still has to be one this
            // transport listed.
            let Some(wanted) = image.digest().map(str::to_owned) else {
                if let (Some(tag), Some(sidecar)) = (image.tag(), self.sidecar.as_ref())
                    && tag.ends_with(".sbom")
                {
                    return Ok((sidecar.manifest.clone(), sidecar.digest().to_string()));
                }
                return Err(ClientError::ManifestNotFound(image.to_string()));
            };
            let referrer = self
                .referrers
                .iter()
                .find(|stub| stub.descriptor().digest == wanted)
                .expect("the scan only asks for referrers this transport listed");
            Ok((referrer.manifest_bytes(), wanted))
        }

        async fn pull_blob(&self, _: &native::Reference, _: &Digest) -> std::result::Result<Vec<u8>, ClientError> {
            unimplemented!("the sbom scan streams blobs")
        }

        async fn pull_blob_streaming(
            &self,
            _: &native::Reference,
            digest: &Digest,
        ) -> std::result::Result<Box<dyn tokio::io::AsyncRead + Send + Unpin + 'static>, ClientError> {
            self.pulled_blobs
                .lock()
                .expect("recorder lock")
                .push(digest.to_string());
            if let Some(sidecar) = self.sidecar.as_ref()
                && let Some(document) = sidecar.document_for(&digest.to_string())
            {
                return Ok(Box::new(std::io::Cursor::new(document)));
            }
            let document = self
                .referrers
                .iter()
                .find_map(|stub| stub.blob_for(digest))
                .expect("the scan only asks for blobs a listed referrer named");
            Ok(Box::new(std::io::Cursor::new(document)))
        }

        async fn pull_blob_to_file(
            &self,
            _: &native::Reference,
            _: &Digest,
            _: &std::path::Path,
        ) -> std::result::Result<(), ClientError> {
            unimplemented!("the sbom scan never writes blobs to disk")
        }

        async fn head_blob(&self, _: &native::Reference, _: &Digest) -> std::result::Result<u64, ClientError> {
            unimplemented!("the sbom scan never HEADs blobs")
        }

        async fn push_manifest(
            &self,
            _: &native::Reference,
            _: &crate::oci::Manifest,
        ) -> std::result::Result<String, ClientError> {
            unimplemented!("reading an SBOM never pushes")
        }

        async fn push_manifest_raw(
            &self,
            _: &native::Reference,
            _: Vec<u8>,
            _: &str,
        ) -> std::result::Result<String, ClientError> {
            unimplemented!("reading an SBOM never pushes")
        }

        async fn push_blob(
            &self,
            _: &native::Reference,
            _: Vec<u8>,
            _: &Digest,
            _: std::sync::Arc<dyn Fn(u64) + Send + Sync>,
        ) -> std::result::Result<String, ClientError> {
            unimplemented!("reading an SBOM never pushes")
        }

        async fn push_blob_from_path(
            &self,
            _: &native::Reference,
            _: &std::path::Path,
            _: &Digest,
            _: std::sync::Arc<dyn Fn(u64) + Send + Sync>,
        ) -> std::result::Result<String, ClientError> {
            unimplemented!("verify never pushes a file-backed blob")
        }

        async fn push_referrer_manifest(
            &self,
            _: &native::Reference,
            _: &Digest,
            _: &[u8],
            _: &str,
        ) -> std::result::Result<crate::oci::Descriptor, ClientError> {
            unimplemented!("reading an SBOM never pushes")
        }

        async fn list_referrers(
            &self,
            _: &native::Reference,
            subject: &Digest,
            artifact_type: Option<&str>,
        ) -> std::result::Result<Vec<crate::oci::Descriptor>, ClientError> {
            self.listing_filters
                .lock()
                .expect("recorder lock")
                .push(artifact_type.map(str::to_string));
            self.listed_subjects
                .lock()
                .expect("recorder lock")
                .push(subject.clone());
            Ok(self
                .referrers
                .iter()
                .filter(|stub| artifact_type.is_none_or(|wanted| stub.artifact_type == wanted))
                .map(StubReferrer::descriptor)
                .collect())
        }

        fn box_clone(&self) -> Box<dyn OciTransport> {
            Box::new(self.clone())
        }
    }

    /// A CycloneDX document, spelled non-canonically so a re-serialization on
    /// the read path would be observable in the bytes.
    const RAW_CYCLONEDX: &str = r#"{"bomFormat":"CycloneDX","specVersion":"1.6","components":[ ]}"#;
    const RAW_SPDX: &str = r#"{"spdxVersion":"SPDX-2.3"}"#;
    /// The predicateType a CycloneDX document is stated under, spelled out
    /// rather than imported: `predicate::URI_CYCLONEDX` is private to its
    /// module, and a test that asserts the wire value should name it.
    const CYCLONEDX_URI: &str = "https://cyclonedx.org/bom";
    /// The SPDX counterpart, spelled out for the same reason.
    const SPDX_URI: &str = "https://spdx.dev/Document";

    /// Drive an `ocx package sbom` scan against a caller-chosen referrer set.
    ///
    /// The mode is explicit at every call site rather than defaulted: it is
    /// the whole subject of these tests, and a default would make half of them
    /// assert against a mode nobody chose.
    async fn drive_sbom_scan(
        referrers: Vec<StubReferrer>,
        predicate_type: Option<PredicateType>,
        verification: VerificationMode,
    ) -> (Result<AttestationScan, VerifyError>, SbomTransport, tempfile::TempDir) {
        let (_key, cert) = self_signed_cert();
        drive_sbom_scan_with_trust_root(referrers, predicate_type, verification, trust_root_of(&[&cert])).await
    }

    /// [`drive_sbom_scan`] with the trust root chosen by the caller, so a test
    /// can hand the pipeline material no signature could verify against.
    async fn drive_sbom_scan_with_trust_root(
        referrers: Vec<StubReferrer>,
        predicate_type: Option<PredicateType>,
        verification: VerificationMode,
        trust_root: TrustRoot,
    ) -> (Result<AttestationScan, VerifyError>, SbomTransport, tempfile::TempDir) {
        drive_sbom_scan_over_transport(SbomTransport::new(referrers), predicate_type, verification, trust_root).await
    }

    /// [`drive_sbom_scan`] over a transport the caller assembled — the door for
    /// a subject carrying a `sha256-<hex>.sbom` sidecar, which is a property of
    /// the registry rather than of the referrer list.
    async fn drive_sbom_scan_over_transport(
        transport: SbomTransport,
        predicate_type: Option<PredicateType>,
        verification: VerificationMode,
        trust_root: TrustRoot,
    ) -> (Result<AttestationScan, VerifyError>, SbomTransport, tempfile::TempDir) {
        let logical = Identifier::parse("ocx.sh/acme/tool:1.0").expect("logical identifier");
        // A public IP literal, not a name: the dial-site SSRF guard resolves the
        // physical host, and a DNS name here would make this unit test open a
        // socket.
        let physical = Identifier::parse("8.8.8.8/acme/tool:1.0").expect("physical identifier");

        let client = Client::with_transport(Box::new(transport.clone()));
        let index = Index::from_impl(IndirectingIndex { physical });
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = StateStore::new(temp.path());
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");

        let outcome = VerifyPipeline::run_attestations(
            &client,
            VerifyContext {
                identifier: &logical,
                platform: None,
                policies: &[],
                no_cache: true,
                index: &index,
                trust_root: &trust_root,
                rekor_url: &rekor_url,
                state: &state,
                offline: true,
                content: VerifyContentMode::Attestation { predicate_type },
                verification,
                signature_format: None,
                allow_unlogged_signature: false,
                report_all: true,
            },
        )
        .await;
        (outcome, transport, temp)
    }

    /// **C-011.** Drive an attestation scan against a chosen resolution, so the
    /// second-subject pass — which lives above the scan, in
    /// [`VerifyPipeline::run_attestations_inner`] — is actually exercised.
    ///
    /// Its own harness rather than a parameter on [`drive_sbom_scan`]: that one
    /// resolves to a bare image manifest, where there is no enclosing index and
    /// the question does not arise. This is the shape where it does.
    async fn drive_sbom_scan_over(
        referrers: Vec<StubReferrer>,
        resolved: Option<(Digest, crate::oci::Manifest)>,
        platform: Option<&Platform>,
    ) -> (Result<AttestationScan, VerifyError>, SbomTransport, tempfile::TempDir) {
        let identifier = Identifier::parse("ocx.sh/acme/tool:1.0").expect("logical identifier");
        let transport = SbomTransport::new(referrers);
        let client = Client::with_transport(Box::new(transport.clone()));
        // Physical == logical: not a rewrite, so the dial-site SSRF guard's
        // own carve-out applies and this unit test resolves no name.
        let index = Index::from_impl(ResolvingIndex {
            physical: identifier.clone(),
            resolved,
        });
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = StateStore::new(temp.path());
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let outcome = VerifyPipeline::run_attestations(
            &client,
            VerifyContext {
                identifier: &identifier,
                platform,
                policies: &[],
                no_cache: true,
                index: &index,
                trust_root: &TrustRoot::default(),
                rekor_url: &rekor_url,
                state: &state,
                offline: true,
                content: VerifyContentMode::Attestation { predicate_type: None },
                verification: VerificationMode::Permissive,
                signature_format: None,
                allow_unlogged_signature: false,
                report_all: true,
            },
        )
        .await;
        (outcome, transport, temp)
    }

    /// **C-011.** An attestation run reads the enclosing index as a **second
    /// subject**, not as a fallback for an empty first one.
    ///
    /// This is the premise the whole shadowing contract stands on: cosign
    /// attests a multi-platform tag at the index while OCX pins a platform
    /// manifest, so a run that read only the narrowed subject would hide every
    /// index-level document — and `shadowed` would be a field that can never be
    /// true.
    ///
    /// Asserted on the *addressing*, because this double serves one referrer set
    /// for any subject: how many subjects were listed is the only observable
    /// difference between one pass and two.
    #[tokio::test]
    async fn an_attestation_run_reads_the_enclosing_index_as_a_second_subject() {
        let child = indirection_subject_digest();
        let enclosing = crate::oci::Algorithm::Sha256.hash(b"the enclosing image index");
        let platform: Platform = "linux/amd64".parse().expect("platform parses");

        let (outcome, transport, _temp) = drive_sbom_scan_over(
            vec![StubReferrer::sbom("application/vnd.cyclonedx+json", RAW_CYCLONEDX)],
            Some((enclosing.clone(), image_index_of(&[("linux/amd64", &child)]))),
            Some(&platform),
        )
        .await;
        let scan = outcome.expect("the subject carries an unsigned SBOM");

        assert_eq!(
            transport.listed_subjects(),
            vec![child.clone(), enclosing.clone()],
            "the platform manifest is read first, then the index behind it",
        );
        assert_eq!(
            scan.platform_subject,
            Some(child),
            "the narrowed subject is reported so the shadowing decision has a fact to key on",
        );

        // Discriminating control: with no `--platform` nothing is narrowed, so
        // one subject is read and no shadowing decision is possible. Without
        // this, "always two subjects" and "correctly two subjects" would look
        // the same.
        let (outcome, transport, _temp) = drive_sbom_scan_over(
            vec![StubReferrer::sbom("application/vnd.cyclonedx+json", RAW_CYCLONEDX)],
            Some((enclosing.clone(), image_index_of(&[("linux/amd64", &enclosing)]))),
            None,
        )
        .await;
        let scan = outcome.expect("the subject carries an unsigned SBOM");
        assert_eq!(
            transport.listed_subjects(),
            vec![enclosing],
            "nothing was narrowed, so there is no second subject to read",
        );
        assert_eq!(scan.platform_subject, None);
    }

    /// The headline case: a subject carrying a bundle referrer the signed pass
    /// cannot use *and* an unsigned SBOM. The SBOM is reported, the bundle's
    /// refusal travels beside it, and neither hides the other.
    ///
    /// Without the two-pass ordering this reds as `BundleParseFailed`: one
    /// refused signed candidate would end a listing that had a perfectly
    /// readable document in it — the same DoS `AttestationScan::refused` exists
    /// to prevent, one layer up.
    #[tokio::test]
    async fn an_unsigned_sbom_is_listed_beside_a_refused_bundle_referrer() {
        let (outcome, _transport, _state) = drive_sbom_scan(
            vec![
                StubReferrer::junk_bundle(),
                StubReferrer::sbom("application/vnd.cyclonedx+json", RAW_CYCLONEDX),
            ],
            None,
            VerificationMode::Permissive,
        )
        .await;

        let scan = outcome.expect("a readable unsigned SBOM is an answer");
        assert!(scan.matches.is_empty(), "the junk bundle cannot verify");
        assert_eq!(scan.unverified.len(), 1, "the unsigned SBOM must be reported");
        let sbom = &scan.unverified[0];
        assert_eq!(
            sbom.predicate_type, "https://cyclonedx.org/bom",
            "an unsigned entry is labelled with the predicateType its artifactType stands for",
        );
        assert_eq!(
            sbom.document,
            RAW_CYCLONEDX.as_bytes(),
            "the document is returned verbatim, not re-serialized",
        );
        assert_eq!(sbom.subject_digest, indirection_subject_digest());
        assert_eq!(scan.refused.len(), 1, "the bundle's refusal travels beside the answer");
        assert!(matches!(scan.refused[0].reason, VerifyErrorKind::BundleParseFailed));
    }

    /// `ocx package verify --attestation` goes through the ANY-of entry point,
    /// which never runs the unsigned pass — so an unsigned referrer can never
    /// become a *verification* candidate. Structural, not a filter someone has
    /// to remember.
    ///
    /// Asserted twice over: the run reports "nothing to verify", and the
    /// document's blob was never fetched at all.
    #[tokio::test]
    async fn verify_attestation_never_treats_an_unsigned_sbom_as_a_candidate() {
        let sbom = StubReferrer::sbom("application/vnd.cyclonedx+json", RAW_CYCLONEDX);
        let document_digest = sbom.document_digest().to_string();

        let logical = Identifier::parse("ocx.sh/acme/tool:1.0").expect("logical identifier");
        let physical = Identifier::parse("8.8.8.8/acme/tool:1.0").expect("physical identifier");
        let transport = SbomTransport::new(vec![sbom]);
        let client = Client::with_transport(Box::new(transport.clone()));
        let index = Index::from_impl(IndirectingIndex { physical });
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = StateStore::new(temp.path());
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let (_key, cert) = self_signed_cert();
        let trust_root = trust_root_of(&[&cert]);

        let outcome = VerifyPipeline::run(
            &client,
            VerifyContext {
                identifier: &logical,
                platform: None,
                policies: &[],
                no_cache: true,
                index: &index,
                trust_root: &trust_root,
                rekor_url: &rekor_url,
                state: &state,
                offline: true,
                content: VerifyContentMode::Attestation { predicate_type: None },
                verification: VerificationMode::Demand,
                signature_format: None,
                allow_unlogged_signature: false,
                report_all: false,
            },
        )
        .await;

        let Err(error) = outcome else {
            panic!("an unsigned referrer must never satisfy a verification");
        };
        assert!(
            matches!(error.kind, VerifyErrorKind::NoSignaturesFound),
            "expected nothing-to-verify, got: {error}",
        );
        assert!(
            !transport.pulled_blobs().contains(&document_digest),
            "the verify path must not even fetch an unsigned document, got: {:?}",
            transport.pulled_blobs(),
        );
        // The signed listing kept its server-side filter, which is the other
        // half of why an unsigned referrer never reaches this scan.
        assert!(
            transport
                .listing_filters()
                .contains(&Some(SIGSTORE_BUNDLE_V03.to_string())),
            "the signature listing must still ask the registry for bundles only, got: {:?}",
            transport.listing_filters(),
        );
    }

    /// `--type` narrows unsigned referrers by the same predicateType vocabulary
    /// a signed entry carries, so one flag means one thing across both trust
    /// classes. The unnarrowed row is the control: without it a narrowing that
    /// dropped everything would pass every other row.
    #[tokio::test]
    async fn the_type_flag_narrows_unsigned_referrers_by_predicate_type() {
        let referrers = vec![
            StubReferrer::sbom("application/vnd.cyclonedx+json", RAW_CYCLONEDX),
            StubReferrer::sbom("application/spdx+json", RAW_SPDX),
        ];
        let cases: [(Option<PredicateType>, &[&str]); 4] = [
            (None, &["https://cyclonedx.org/bom", "https://spdx.dev/Document"]),
            (Some(PredicateType::CycloneDx), &["https://cyclonedx.org/bom"]),
            (Some(PredicateType::SpdxJson), &["https://spdx.dev/Document"]),
            // The two SPDX spellings share one predicateType URI, so narrowing
            // is by URI and `spdx` reaches the JSON serialization too.
            (Some(PredicateType::Spdx), &["https://spdx.dev/Document"]),
        ];
        for (predicate_type, expected) in cases {
            let (outcome, _transport, _state) =
                drive_sbom_scan(referrers.clone(), predicate_type.clone(), VerificationMode::Permissive).await;
            let scan = outcome.unwrap_or_else(|error| panic!("{predicate_type:?} must list: {error}"));
            let mut found: Vec<String> = scan.unverified.iter().map(|sbom| sbom.predicate_type.clone()).collect();
            found.sort();
            assert_eq!(found, expected, "for --type {predicate_type:?}");
        }
    }

    /// The one structural claim an unsigned referrer makes that can be checked
    /// without a key. Nothing signs the artifactType, so without this a referrer
    /// could advertise `application/vnd.cyclonedx+json` and carry anything at
    /// all, and the listing would present it as an SBOM.
    ///
    /// With nothing else on the subject the refusal is also what the scan
    /// *reports*: "not found" would send a publisher looking for an attach that
    /// did happen.
    #[tokio::test]
    async fn an_unsigned_referrer_whose_layer_is_not_an_sbom_is_refused_by_name() {
        let mut stub = StubReferrer::sbom("application/vnd.cyclonedx+json", RAW_CYCLONEDX);
        stub.layer_media_type = "application/octet-stream".to_string();

        let (outcome, _transport, _state) = drive_sbom_scan(vec![stub], None, VerificationMode::Permissive).await;

        let Err(error) = outcome else {
            panic!("a layer typed outside the SBOM set must not be listed as an SBOM");
        };
        let VerifyErrorKind::SbomMediaTypeUnsupported { media_type } = &error.kind else {
            panic!("expected the media-type refusal, got: {error}");
        };
        assert_eq!(media_type, "application/octet-stream");
        assert_eq!(classify_error(&error), ExitCode::DataError);
    }

    /// The size cap, on the declared size, before the body is fetched. The
    /// declared value is untrusted, so this is the cheap half — the read itself
    /// is separately bounded — but it is the half that keeps a hostile registry
    /// from making the client open the connection at all.
    #[tokio::test]
    async fn an_oversized_unsigned_document_is_refused_before_it_is_fetched() {
        let mut stub = StubReferrer::sbom("application/vnd.cyclonedx+json", RAW_CYCLONEDX);
        stub.declared_layer_size = Some(MAX_ATTESTATION_ENVELOPE_BYTES as i64 + 1);

        let (outcome, transport, _state) =
            drive_sbom_scan(vec![stub.clone()], None, VerificationMode::Permissive).await;

        let Err(error) = outcome else {
            panic!("an over-cap document must be refused");
        };
        let VerifyErrorKind::AttestationTooLarge { limit, actual } = &error.kind else {
            panic!("expected the size refusal naming the bound, got: {error}");
        };
        assert_eq!(*limit, MAX_ATTESTATION_ENVELOPE_BYTES as u64);
        assert_eq!(*actual, MAX_ATTESTATION_ENVELOPE_BYTES as u64 + 1);
        assert_eq!(classify_error(&error), ExitCode::DataError);
        assert!(
            !transport.pulled_blobs().contains(&stub.document_digest().to_string()),
            "the refusal must land before the fetch, got: {:?}",
            transport.pulled_blobs(),
        );
    }

    /// A subject with no referrers of either kind still ends where it always
    /// did — the 79 `ocx package sbom` and its callers branch on. The unsigned
    /// pass must not turn an empty subject into a success with an empty list.
    #[tokio::test]
    async fn a_subject_with_nothing_attached_is_still_not_found() {
        let (outcome, _transport, _state) = drive_sbom_scan(Vec::new(), None, VerificationMode::Permissive).await;

        let Err(error) = outcome else {
            panic!("an empty subject carries no SBOMs");
        };
        assert!(
            matches!(
                error.kind,
                VerifyErrorKind::AttestationNotFound | VerifyErrorKind::NoSignaturesFound
            ),
            "expected a not-found verdict, got: {error}",
        );
        assert_eq!(classify_error(&error), ExitCode::NotFound);
    }

    /// Volume of unsigned referrers cannot starve the signed pass.
    ///
    /// A registry attaching more SBOM-typed referrers than the candidate cap
    /// allows must not be able to spend the budget the signed pass needs: on
    /// one shared allowance it would take every slot before the signed pass
    /// looked at anything, the run would report `TooManyAttestations`, and the
    /// bundle sitting behind them would never be fetched at all.
    ///
    /// Under `Demand` that is closed structurally rather than by rationing —
    /// an unsigned attachment can never be an answer here, so it is refused
    /// from the listing and its blob is never fetched. Both halves are
    /// asserted: the bundle was reached, and not one unsigned document was
    /// read. The second is what fails the moment the refusal starts costing a
    /// fetch, whatever budget arithmetic surrounds it.
    #[tokio::test]
    async fn unsigned_referrer_volume_cannot_starve_the_signed_pass() {
        let bundle = StubReferrer::junk_bundle();
        let unsigned: Vec<StubReferrer> = (0..=MAX_ATTESTATION_CANDIDATES)
            .map(|index| {
                // Distinct bytes per referrer: identical documents would share
                // one digest and the transport could not tell them apart.
                let document = format!(r#"{{"bomFormat":"CycloneDX","specVersion":"1.6","serialNumber":"{index}"}}"#);
                StubReferrer::sbom("application/vnd.cyclonedx+json", &document)
            })
            .collect();
        // Listed last, so being reached at all is the property under test.
        let mut referrers = unsigned.clone();
        referrers.push(bundle.clone());

        let (outcome, transport, _state) = drive_sbom_scan(referrers, None, VerificationMode::Demand).await;

        // Nothing verifies in a unit test, so the whole scan ends as the
        // bundle's own refusal — promoted over the unsigned ones because a
        // real signature failure outranks them (`failure_rank`).
        let Err(error) = outcome else {
            panic!("a junk bundle cannot verify, and no unsigned document may stand in for it");
        };
        assert!(
            matches!(error.kind, VerifyErrorKind::BundleParseFailed),
            "the signed pass must have reached the bundle, got: {error}",
        );
        let pulled = transport.pulled_blobs();
        assert!(
            pulled.contains(&bundle.document_digest().to_string()),
            "the signed pass must still reach the bundle's blob, got: {pulled:?}",
        );
        for candidate in &unsigned {
            assert!(
                !pulled.contains(&candidate.document_digest().to_string()),
                "a demanded scan must refuse an unsigned attachment without fetching it, got: {pulled:?}",
            );
        }
    }

    /// The refusal a demanded scan records for an unsigned attachment, and the
    /// code it exits with when that is all the subject carries.
    ///
    /// 77, not 79: the SBOM is there and the operator can see it listed by
    /// `--no-verify`. What happened is that a policy demanded a signer and this
    /// document has none — the same class of answer as the wrong signer, and
    /// the code a script already branches on for it.
    #[tokio::test]
    async fn a_demanded_scan_refuses_an_unsigned_sbom_with_permission_denied() {
        let sbom = StubReferrer::sbom("application/vnd.cyclonedx+json", RAW_CYCLONEDX);
        let (outcome, transport, _state) = drive_sbom_scan(vec![sbom.clone()], None, VerificationMode::Demand).await;

        let Err(error) = outcome else {
            panic!("an unsigned attachment is not an answer to a demanded scan");
        };
        assert!(
            matches!(error.kind, VerifyErrorKind::UnsignedRejectedByPolicy),
            "expected the unsigned refusal, got: {error}",
        );
        assert_eq!(classify_error(&error), ExitCode::PermissionDenied);
        assert!(
            transport.pulled_blobs().is_empty(),
            "the refusal lands before the fetch, got: {:?}",
            transport.pulled_blobs(),
        );
    }

    /// The same subject under `--no-verify`: the document a demanded scan
    /// refuses is exactly the one a permissive scan lists.
    ///
    /// The pair is the contract. Read separately, either half looks like a
    /// bug — a refused SBOM that is plainly there, or an unverified row nobody
    /// checked — and it is the flag that decides which is correct.
    #[tokio::test]
    async fn a_permissive_scan_lists_the_sbom_a_demanded_scan_refuses() {
        let sbom = StubReferrer::sbom("application/vnd.cyclonedx+json", RAW_CYCLONEDX);
        let (outcome, _transport, _state) = drive_sbom_scan(vec![sbom], None, VerificationMode::Permissive).await;

        let scan = outcome.expect("a permissive scan reads what is attached");
        assert!(scan.matches.is_empty(), "nothing is verified in this mode");
        assert_eq!(scan.unverified.len(), 1);
        assert_eq!(scan.unverified[0].document, RAW_CYCLONEDX.as_bytes());
    }

    /// A signed publisher's SBOM is readable with no Sigstore setup at all:
    /// `--no-verify` extracts the bundle's DSSE payload and lists it, with the
    /// trust class it actually has.
    ///
    /// This is the case the mode exists for, so it asserts both halves. The
    /// document must come out — the predicate verbatim, not the envelope — and
    /// it must be labelled `unverified` and carry no signer, because nothing
    /// here checked a certificate. An extraction that reported a signer would
    /// be presenting registry-controlled bytes as provenance.
    #[tokio::test]
    async fn a_permissive_scan_extracts_a_bundle_payload_without_verifying_it() {
        let bundle = StubReferrer::attestation_bundle(CYCLONEDX_URI, RAW_CYCLONEDX);
        let (outcome, _transport, _state) = drive_sbom_scan(vec![bundle], None, VerificationMode::Permissive).await;

        let scan = outcome.expect("the payload is readable without a key");
        assert!(scan.matches.is_empty(), "nothing is verified in this mode");
        assert_eq!(scan.unverified.len(), 1, "the bundle's payload is the answer");
        let extracted = &scan.unverified[0];
        assert_eq!(
            extracted.predicate_type, CYCLONEDX_URI,
            "the predicateType comes from the payload, which is the only place it is stated",
        );
        assert_eq!(
            extracted.document,
            RAW_CYCLONEDX.as_bytes(),
            "the predicate is the verbatim sub-slice, never a re-serialization",
        );
    }

    /// The mode runs no cryptography, proven by handing it a trust root
    /// nothing could ever verify against.
    ///
    /// An empty [`TrustRoot`] has no CT-log key, which the signed scan refuses
    /// outright (`NoCtLogKey`) before it looks at a single candidate. So the
    /// same subject, the same bundle and the same fixture split cleanly on the
    /// mode: demanded it fails on trust material, permissive it returns the
    /// document. A permissive path that quietly grew a verification step would
    /// red here, whatever it did with the result.
    #[tokio::test]
    async fn a_permissive_scan_needs_no_trust_material_at_all() {
        let bundle = StubReferrer::attestation_bundle(CYCLONEDX_URI, RAW_CYCLONEDX);

        let (permissive, _transport, _state) = drive_sbom_scan_with_trust_root(
            vec![bundle.clone()],
            None,
            VerificationMode::Permissive,
            TrustRoot::default(),
        )
        .await;
        let scan = permissive.expect("reading a payload needs no trust root");
        assert_eq!(scan.unverified.len(), 1);

        let (demanded, _transport, _state) =
            drive_sbom_scan_with_trust_root(vec![bundle], None, VerificationMode::Demand, TrustRoot::default()).await;
        let Err(error) = demanded else {
            panic!("a demanded scan cannot verify against an empty trust root");
        };
        assert!(
            matches!(
                error.kind,
                VerifyErrorKind::TrustRootLoad(TrustRootLoadReason::NoCtLogKey)
            ),
            "the demanded half must fail on trust material, got: {error}",
        );
    }

    /// `--type` narrows a bundle payload too, and after the parse rather than
    /// before it: a bundle states its predicateType inside the envelope, so
    /// there is nothing to narrow on until it has been read.
    #[tokio::test]
    async fn the_type_flag_narrows_a_bundle_payload_after_the_parse() {
        let bundle = StubReferrer::attestation_bundle(CYCLONEDX_URI, RAW_CYCLONEDX);
        let (outcome, _transport, _state) = drive_sbom_scan(
            vec![bundle],
            Some(PredicateType::SpdxJson),
            VerificationMode::Permissive,
        )
        .await;

        let Err(error) = outcome else {
            panic!("a narrowing miss leaves nothing to list");
        };
        assert!(
            matches!(
                error.kind,
                VerifyErrorKind::AttestationNotFound | VerifyErrorKind::NoSignaturesFound
            ),
            "a narrowing miss is not-found, never a refusal: the candidate was sound, got: {error}",
        );
    }

    /// A referrer whose listing entry and payload layer disagree is reported
    /// under the *layer's* type, because that is the one claim attached to the
    /// bytes that were served.
    ///
    /// The attack this closes: a registry lists `artifactType:
    /// application/vnd.cyclonedx+json` over a `text/spdx` layer, and a consumer
    /// that trusted the listing hands SPDX bytes to a CycloneDX parser — or,
    /// worse, records them in an inventory under a format they are not in.
    /// Nothing checks a listing's `artifactType` against the manifest it points
    /// at, so it may choose which decode to run and nothing more.
    #[tokio::test]
    async fn an_unverified_row_is_typed_by_its_layer_not_by_the_listing() {
        let mislabelled = StubReferrer::mislabelled_sbom(
            "application/vnd.cyclonedx+json",
            crate::oci::referrer::media_types::SBOM_SPDX_TEXT,
            RAW_SPDX,
        );
        let (outcome, _transport, _state) =
            drive_sbom_scan(vec![mislabelled], None, VerificationMode::Permissive).await;

        let scan = outcome.expect("a cross-family disagreement is labelled, not refused");
        assert_eq!(scan.unverified.len(), 1);
        assert_eq!(
            scan.unverified[0].predicate_type, SPDX_URI,
            "the layer served SPDX bytes, so SPDX is what the row may claim",
        );
        assert_eq!(scan.unverified[0].document, RAW_SPDX.as_bytes());
    }

    /// `--type` narrows a raw attachment on the layer-derived type: the
    /// requested type matches the layer, and the row comes out.
    ///
    /// Paired with its miss below. Either assertion alone passes against the
    /// listing-derived label too — it is the *pair*, on one fixture whose two
    /// claims disagree, that pins which of the two the filter reads.
    #[tokio::test]
    async fn the_type_flag_narrows_a_raw_attachment_on_the_layer_type() {
        let mislabelled = StubReferrer::mislabelled_sbom(
            "application/vnd.cyclonedx+json",
            crate::oci::referrer::media_types::SBOM_SPDX_TEXT,
            RAW_SPDX,
        );
        let (outcome, _transport, _state) = drive_sbom_scan(
            vec![mislabelled],
            Some(PredicateType::SpdxJson),
            VerificationMode::Permissive,
        )
        .await;

        let scan = outcome.expect("--type spdx matches the SPDX layer");
        assert_eq!(scan.unverified.len(), 1);
        assert_eq!(scan.unverified[0].predicate_type, SPDX_URI);
    }

    /// The miss half: `--type cyclonedx` against the same fixture drops it,
    /// even though the *listing* said CycloneDX.
    #[tokio::test]
    async fn the_type_flag_ignores_the_listings_claim_when_narrowing() {
        let mislabelled = StubReferrer::mislabelled_sbom(
            "application/vnd.cyclonedx+json",
            crate::oci::referrer::media_types::SBOM_SPDX_TEXT,
            RAW_SPDX,
        );
        let (outcome, _transport, _state) = drive_sbom_scan(
            vec![mislabelled],
            Some(PredicateType::CycloneDx),
            VerificationMode::Permissive,
        )
        .await;

        let Err(error) = outcome else {
            panic!("the only candidate's layer is SPDX, so nothing answers --type cyclonedx");
        };
        assert!(
            matches!(
                error.kind,
                VerifyErrorKind::AttestationNotFound | VerifyErrorKind::NoSignaturesFound
            ),
            "a narrowing miss is not-found, never a refusal: the candidate was sound, got: {error}",
        );
    }

    /// A layer typed outside the SBOM set is still refused - the gate the
    /// layer-derived label replaced is the same gate, not a dropped one.
    #[tokio::test]
    async fn a_raw_attachment_with_a_non_sbom_layer_is_refused() {
        let mislabelled = StubReferrer::mislabelled_sbom(
            "application/vnd.cyclonedx+json",
            "application/vnd.oci.image.layer.v1.tar+gzip",
            RAW_CYCLONEDX,
        );
        let (outcome, _transport, _state) =
            drive_sbom_scan(vec![mislabelled], None, VerificationMode::Permissive).await;

        let Err(error) = outcome else {
            panic!("an arbitrary blob is not an SBOM however the listing types it");
        };
        assert!(
            matches!(error.kind, VerifyErrorKind::SbomMediaTypeUnsupported { .. }),
            "the layer's media type decides, got: {error}",
        );
    }

    /// A bundle whose blob is not a bundle is refused, not served.
    ///
    /// The permissive mode reads a payload without checking a signature; it
    /// does not read *anything* and call it a payload. The structural parse is
    /// what separates the two, and it is the same one the signed path runs.
    #[tokio::test]
    async fn a_permissive_scan_refuses_an_unparseable_bundle() {
        let (outcome, _transport, _state) =
            drive_sbom_scan(vec![StubReferrer::junk_bundle()], None, VerificationMode::Permissive).await;

        let Err(error) = outcome else {
            panic!("junk is not a document");
        };
        assert!(
            matches!(error.kind, VerifyErrorKind::BundleParseFailed),
            "expected the parse refusal, got: {error}",
        );
    }

    // ── D-5: the discovery merge, its dedup, and D9's preference + pin ──────

    /// cosign's key-mode `.sig` sidecar, the same committed capture
    /// `simplesigning_read`'s own tests read. Key mode on purpose: it carries
    /// **no** transparency entry, so it exercises D6's fallback dedup tuple
    /// rather than the Rekor-log-index branch — which is exactly where double
    /// discovery is most likely, since `cosign sign --key` uploads nothing.
    const SIDECAR_MANIFEST: &str =
        include_str!("../../../../../test/tests/fixtures/golden/simplesigning_key_manifest.json");
    const SIDECAR_PAYLOAD: &[u8] =
        include_bytes!("../../../../../test/tests/fixtures/golden/simplesigning_key_payload.json");

    /// The registry reference every merge test addresses.
    const SCAN_IMAGE: &str = "registry.example/repo:latest";

    /// The subject the sidecar payload binds, read out of the committed bytes
    /// rather than transcribed — a transcribed digest is a second source of
    /// truth, and the claim check would then be asserting against itself.
    fn sidecar_subject() -> Digest {
        let parsed: serde_json::Value = serde_json::from_slice(SIDECAR_PAYLOAD).expect("the payload is JSON");
        Digest::try_from(
            parsed
                .pointer("/critical/image/docker-manifest-digest")
                .and_then(serde_json::Value::as_str)
                .expect("the payload names a subject"),
        )
        .expect("the subject is an OCI digest")
    }

    /// The sidecar manifest's one simplesigning layer descriptor.
    fn sidecar_layer() -> crate::oci::Descriptor {
        let manifest: crate::oci::ImageManifest =
            serde_json::from_str(SIDECAR_MANIFEST).expect("the sidecar manifest parses");
        manifest
            .layers
            .into_iter()
            .next()
            .expect("the sidecar carries one layer")
    }

    /// A referrer descriptor pointing at an already-seeded manifest, typed with
    /// `artifact_type`. Unlike [`referrer_with`] this does not build the
    /// manifest — the sidecar shape is a plain OCI image manifest the caller
    /// seeds verbatim.
    fn referrer_descriptor(manifest_bytes: &[u8], artifact_type: &str) -> crate::oci::Descriptor {
        crate::oci::Descriptor {
            media_type: crate::oci::OCI_IMAGE_MEDIA_TYPE.to_string(),
            digest: crate::oci::Algorithm::Sha256.hash(manifest_bytes).to_string(),
            size: manifest_bytes.len() as i64,
            artifact_type: Some(artifact_type.to_string()),
            ..crate::oci::Descriptor::default()
        }
    }

    /// Drive [`VerifyPipeline::scan`] over a seeded stub registry.
    ///
    /// The scan, not one candidate: the merge of the four discovery shapes into
    /// one candidate list happens here and nowhere else, so a test that drove
    /// `verify_one_referrer` directly could not observe it.
    async fn drive_scan(
        data: crate::oci::client::test_transport::StubTransportData,
        subject_digest: &Digest,
        policies: &[crate::trust::CompiledPolicy],
        trust_root: &TrustRoot,
        signature_format: Option<SignatureFormat>,
        arity: ScanArity,
    ) -> Result<ScanOutcome, VerifyErrorKind> {
        drive_scan_in_mode(
            data,
            subject_digest,
            policies,
            trust_root,
            signature_format,
            arity,
            VerifyContentMode::Signature,
        )
        .await
    }

    /// [`drive_scan`] with the content mode as a parameter.
    ///
    /// The mode is what decides which discovery doors `scan` opens — the
    /// simplesigning sidecar in signature mode, the `.att` sidecar in
    /// attestation mode — so a test of that gate has to be able to vary it.
    async fn drive_scan_in_mode(
        data: crate::oci::client::test_transport::StubTransportData,
        subject_digest: &Digest,
        policies: &[crate::trust::CompiledPolicy],
        trust_root: &TrustRoot,
        signature_format: Option<SignatureFormat>,
        arity: ScanArity,
        content: VerifyContentMode,
    ) -> Result<ScanOutcome, VerifyErrorKind> {
        let mut budget = ScanBudget::new(content.caps());
        drive_scan_with_budget(
            data,
            subject_digest,
            policies,
            trust_root,
            signature_format,
            arity,
            content,
            &mut budget,
        )
        .await
    }

    /// [`drive_scan`] with the budget supplied by the caller, so a test can
    /// hand the scan bounds that are **already spent** — the state the shared
    /// budget is genuinely in when `scan_with_index_fallback` reaches its
    /// second pass, and the only way to observe what a truncated scan does.
    #[expect(
        clippy::too_many_arguments,
        reason = "the seeded registry, the subject, the trust material, and the three run knobs a scan test varies"
    )]
    async fn drive_scan_with_budget(
        data: crate::oci::client::test_transport::StubTransportData,
        subject_digest: &Digest,
        policies: &[crate::trust::CompiledPolicy],
        trust_root: &TrustRoot,
        signature_format: Option<SignatureFormat>,
        arity: ScanArity,
        content: VerifyContentMode,
        budget: &mut ScanBudget,
    ) -> Result<ScanOutcome, VerifyErrorKind> {
        use crate::oci::client::test_transport::StubTransport;

        let client = Client::with_transport(Box::new(StubTransport::new(data)));
        let image: native::Reference = SCAN_IMAGE.parse().expect("stub reference");
        let identifier = verify_id();
        let index = Index::from_impl(IndirectingIndex {
            physical: identifier.clone(),
        });
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = StateStore::new(temp.path());
        let ctx = VerifyContext {
            identifier: &identifier,
            platform: None,
            policies,
            no_cache: true,
            index: &index,
            trust_root,
            rekor_url: &rekor_url,
            state: &state,
            offline: true,
            content,
            verification: VerificationMode::Demand,
            signature_format,
            allow_unlogged_signature: false,
            report_all: arity == ScanArity::All,
        };
        let target = ScanTarget {
            image,
            subject_digest: subject_digest.clone(),
            // `drive_scan` exercises one subject's own referrers; C-008's
            // index fall-through lives above `scan`, in `run_inner`.
            enclosing_index: None,
            index_members: Vec::new(),
        };
        VerifyPipeline::scan(&client, &ctx, &target, arity, budget).await
    }

    /// Seed one subject with the cosign sidecar reachable through **both**
    /// doors: an OCI 1.1 referrer typed `COSIGN_SIG_ARTIFACT_TYPE`, and the
    /// `sha256-<hex>.sig` sidecar tag. Same manifest, same layer, same
    /// signature bytes — one signature, two ways to find it.
    fn seed_sidecar_through_both_doors(
        subject: &Digest,
        via_referrer: bool,
        via_tag: bool,
    ) -> crate::oci::client::test_transport::StubTransportData {
        use crate::oci::client::sibling_tag_reference;
        use crate::oci::client::test_transport::{StubTransportData, referrers_key};

        let image: native::Reference = SCAN_IMAGE.parse().expect("stub reference");
        let manifest_bytes = SIDECAR_MANIFEST.as_bytes().to_vec();
        let descriptor = referrer_descriptor(&manifest_bytes, COSIGN_SIG_ARTIFACT_TYPE);
        let layer = sidecar_layer();

        let data = StubTransportData::new();
        {
            let mut inner = data.write();
            inner.blobs.insert(layer.digest.clone(), SIDECAR_PAYLOAD.to_vec());
            if via_referrer {
                inner
                    .referrers
                    .entry(referrers_key(&image, subject))
                    .or_default()
                    .push(descriptor.clone());
                let referrer_ref = image.clone_with_digest(descriptor.digest.clone());
                inner.manifests.insert(
                    referrer_ref.to_string(),
                    (manifest_bytes.clone(), descriptor.digest.clone()),
                );
            }
            if via_tag {
                let tag_ref = sibling_tag_reference(
                    &image,
                    super::simplesigning_read::sidecar_tag(subject, SidecarKind::Signature),
                );
                inner
                    .manifests
                    .insert(tag_ref.to_string(), (manifest_bytes, descriptor.digest.clone()));
            }
        }
        data
    }

    // ── §WP5: the `.sbom` sidecar tag, and the cosign OCI 1.1 SBOM referrer ──

    /// cosign v3.1.1's own `sha256-<hex>.sbom` manifest and the CycloneDX
    /// document its one layer holds — the committed capture, not a
    /// reconstruction.
    ///
    /// The manifest is served verbatim under the tag, which it can be for any
    /// subject: a `.sbom` sidecar declares no `subject` field, so nothing in it
    /// names the image it hangs off. That absence is the shape under test and is
    /// pinned by `golden/generate.py`'s `_check_sbom_sidecar`.
    const SBOM_SIDECAR_MANIFEST: &[u8] =
        include_bytes!("../../../../../test/tests/fixtures/golden/sbom_sidecar_manifest.json");
    const SBOM_SIDECAR_DOCUMENT: &[u8] =
        include_bytes!("../../../../../test/tests/fixtures/golden/sbom_sidecar_document.json");

    /// A transport serving cosign's committed `.sbom` sidecar and nothing else
    /// — no referrer of any kind, so a document that comes back can only have
    /// arrived through the tag.
    fn sbom_sidecar_transport() -> SbomTransport {
        SbomTransport::new(Vec::new()).with_sidecar(SBOM_SIDECAR_MANIFEST, &[SBOM_SIDECAR_DOCUMENT])
    }

    async fn drive_sidecar_scan(
        transport: SbomTransport,
        predicate_type: Option<PredicateType>,
        verification: VerificationMode,
    ) -> (Result<AttestationScan, VerifyError>, SbomTransport, tempfile::TempDir) {
        let (_key, cert) = self_signed_cert();
        drive_sbom_scan_over_transport(transport, predicate_type, verification, trust_root_of(&[&cert])).await
    }

    /// **C-024.** A referrer manifest carrying two payload layers lists two
    /// documents.
    ///
    /// The reader took `layers.first()` and dropped the rest. Nothing in OCI
    /// makes that safe: the image-manifest schema bounds `layers` below at one
    /// and not above, and a referrer manifest is an ordinary image manifest, so
    /// "one payload layer" was a property of OCX's own writer and not of the
    /// bytes a registry serves. Both documents are asserted by content, because
    /// a reader that returned the first document twice would satisfy a count.
    #[tokio::test]
    async fn a_referrer_with_two_payload_layers_lists_both_documents() {
        let (outcome, _transport, _state) = drive_sbom_scan(
            vec![StubReferrer::sbom("application/vnd.cyclonedx+json", RAW_CYCLONEDX).with_extra_document(RAW_SPDX)],
            None,
            VerificationMode::Permissive,
        )
        .await;

        let scan = outcome.expect("a permissive scan lists unverified documents");
        let documents: Vec<&[u8]> = scan
            .unverified
            .iter()
            .map(|listed| listed.document.as_slice())
            .collect();
        assert_eq!(
            documents,
            vec![RAW_CYCLONEDX.as_bytes(), RAW_SPDX.as_bytes()],
            "every layer the referrer declares is a document, in manifest order",
        );
        // The label comes from the *layer*, and both layers carry the referrer's
        // one media type — so the second document is reported under the type the
        // registry served it as, not under the type its own bytes look like.
        assert!(
            scan.unverified
                .iter()
                .all(|listed| listed.predicate_type == CYCLONEDX_URI),
            "the layer media type labels every document: {:?}",
            scan.unverified,
        );
    }

    /// One candidate slot, however many layers the referrer holds — the slot cap
    /// bounds discovery breadth, and this is still one manifest fetch.
    ///
    /// Measured against a control rather than against a transcribed number: the
    /// same scan over a *single*-layer referrer is the baseline, so the
    /// assertion is "the second layer cost no slot" and not "the scan spends N",
    /// which would drift with every unrelated door the pass opens.
    ///
    /// Paired with the test above so the multi-layer read cannot be paid for out
    /// of a second candidate's allowance: a reader that spent a slot per layer
    /// would silently halve how many *referrers* a scan can look at.
    #[tokio::test]
    async fn a_multi_layer_referrer_still_costs_one_candidate_slot() {
        async fn slots_spent(referrer: StubReferrer) -> usize {
            let mut budget = ScanBudget::new(VerifyContentMode::Attestation { predicate_type: None }.caps());
            let transport = SbomTransport::new(vec![referrer]);
            let client = Client::with_transport(Box::new(transport));
            let target = ScanTarget {
                image: "registry.example/repo:latest".parse().expect("stub reference"),
                subject_digest: indirection_subject_digest(),
                enclosing_index: None,
                index_members: Vec::new(),
            };
            let identifier = verify_id();
            let index = Index::from_impl(IndirectingIndex {
                physical: identifier.clone(),
            });
            let (_key, cert) = self_signed_cert();
            let trust_root = trust_root_of(&[&cert]);
            let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
            let temp = tempfile::TempDir::new().expect("state dir");
            let state = StateStore::new(temp.path());
            let ctx = VerifyContext {
                identifier: &identifier,
                platform: None,
                policies: &[],
                no_cache: true,
                index: &index,
                trust_root: &trust_root,
                rekor_url: &rekor_url,
                state: &state,
                offline: true,
                content: VerifyContentMode::Attestation { predicate_type: None },
                verification: VerificationMode::Permissive,
                signature_format: None,
                allow_unlogged_signature: false,
                report_all: true,
            };
            let (found, refused) = VerifyPipeline::scan_unverified(&client, &ctx, &target, &mut budget)
                .await
                .expect("the permissive pass reads the referrer");
            assert!(refused.is_empty(), "nothing was refused: {refused:?}");
            assert!(!found.is_empty(), "the referrer yields at least one document");
            budget.considered
        }

        let one_layer = slots_spent(StubReferrer::sbom("application/vnd.cyclonedx+json", RAW_CYCLONEDX)).await;
        let two_layers = slots_spent(
            StubReferrer::sbom("application/vnd.cyclonedx+json", RAW_CYCLONEDX).with_extra_document(RAW_SPDX),
        )
        .await;

        assert_eq!(
            two_layers, one_layer,
            "a referrer is one candidate slot whatever it carries: {two_layers} against {one_layer}",
        );
    }

    /// **§WP5, SBOM half.** A permissive scan reaches the `sha256-<hex>.sbom`
    /// tag with no referrer listing to help it, and lists cosign's document.
    ///
    /// The wiring assertion. Every other door on this subject is empty — the
    /// referrers listing returns nothing at all — so a result here came through
    /// the tag or it came from nowhere. Before this reader existed the same
    /// subject answered 79, which is what made a `cosign attach sbom`
    /// attachment invisible to `ocx package sbom`.
    ///
    /// The document is asserted byte-for-byte against the committed capture:
    /// the layer is opaque bytes, so a read path that re-serialized it would be
    /// reporting something the registry never served.
    #[tokio::test]
    async fn a_permissive_scan_lists_a_cosign_sbom_sidecar_tag() {
        let (outcome, transport, _state) =
            drive_sidecar_scan(sbom_sidecar_transport(), None, VerificationMode::Permissive).await;

        let scan = outcome.expect("the `.sbom` tag is the whole discovery story for this subject");
        assert!(scan.matches.is_empty(), "an unsigned sidecar verifies nothing");
        assert_eq!(
            scan.unverified.len(),
            1,
            "one document, from the tag: {:?}",
            scan.unverified
        );
        let listed = &scan.unverified[0];
        assert_eq!(
            listed.document, SBOM_SIDECAR_DOCUMENT,
            "the document must be the bytes the registry served, verbatim",
        );
        assert_eq!(
            listed.predicate_type, CYCLONEDX_URI,
            "the layer's media type is the only claim about the document, and it is what labels it",
        );
        assert_eq!(
            listed.referrer_digest.to_string(),
            crate::oci::Algorithm::Sha256.hash(SBOM_SIDECAR_MANIFEST).to_string(),
            "the row must name the manifest the registry answered the tag with",
        );
        assert!(
            transport.pulled_blobs().len() == 1,
            "exactly the document blob, got: {:?}",
            transport.pulled_blobs(),
        );
    }

    /// cosign's committed `.sbom` manifest with its `layers` array replaced by
    /// one descriptor per document — every other field, the config descriptor
    /// included, exactly as cosign wrote it.
    ///
    /// Built rather than committed because no producer writes it: cosign's
    /// second `attach sbom` *replaces* the tag's manifest. That is the point —
    /// the tag is generic OCI, addressed by name, and the reader does not get to
    /// assume the registry's answer came from cosign.
    fn sbom_sidecar_manifest_of(documents: &[&[u8]]) -> Vec<u8> {
        let mut manifest: serde_json::Value =
            serde_json::from_slice(SBOM_SIDECAR_MANIFEST).expect("cosign's sidecar manifest is JSON");
        let template = manifest["layers"][0].clone();
        let layers: Vec<serde_json::Value> = documents
            .iter()
            .map(|document| {
                let mut layer = template.clone();
                layer["digest"] = serde_json::json!(crate::oci::Algorithm::Sha256.hash(document).to_string());
                layer["size"] = serde_json::json!(document.len());
                layer
            })
            .collect();
        manifest["layers"] = serde_json::Value::Array(layers);
        serde_json::to_vec(&manifest).expect("the rebuilt sidecar manifest serializes")
    }

    /// **S-007 / C-020.** A `.sbom` tag carrying two layers lists two documents,
    /// and neither is silently dropped.
    ///
    /// `ocx package sbom` is a collect-all report, so a reader that took
    /// `layers.first()` did not mean "cosign writes one document" — it meant
    /// every document past the first vanished from the answer with no refusal
    /// and exit 0 (#386).
    ///
    /// Both documents are asserted by content: a reader that listed the first
    /// one twice would satisfy a length check.
    #[tokio::test]
    async fn a_multi_layer_sbom_sidecar_tag_lists_every_document() {
        const SECOND_DOCUMENT: &[u8] = br#"{"bomFormat":"CycloneDX","specVersion":"1.6","components":[{"name":"b"}]}"#;
        let manifest = sbom_sidecar_manifest_of(&[SBOM_SIDECAR_DOCUMENT, SECOND_DOCUMENT]);
        let transport =
            SbomTransport::new(Vec::new()).with_sidecar(&manifest, &[SBOM_SIDECAR_DOCUMENT, SECOND_DOCUMENT]);

        let (outcome, transport, _state) = drive_sidecar_scan(transport, None, VerificationMode::Permissive).await;

        let scan = outcome.expect("the `.sbom` tag is the whole discovery story for this subject");
        let documents: Vec<&[u8]> = scan
            .unverified
            .iter()
            .map(|listed| listed.document.as_slice())
            .collect();
        assert_eq!(
            documents,
            vec![SBOM_SIDECAR_DOCUMENT, SECOND_DOCUMENT],
            "every layer the tag declares is a document, in manifest order",
        );
        assert_eq!(
            transport.pulled_blobs().len(),
            2,
            "one blob per layer, and no layer read twice: {:?}",
            transport.pulled_blobs(),
        );
    }

    /// The subject that carries no `.sbom` tag is unaffected: a 404 is "no
    /// sidecar", never an error.
    ///
    /// The overwhelmingly common case, and the one a new door is most likely to
    /// break — every `ocx package sbom --no-verify` run now opens it. Paired
    /// with the test above so the two halves cannot both be satisfied by a
    /// reader that ignores the tag entirely: this one demands 79 where that one
    /// demands a document, off the same code path.
    #[tokio::test]
    async fn a_subject_with_no_sbom_tag_is_unchanged_by_the_new_door() {
        let (outcome, _transport, _state) =
            drive_sidecar_scan(SbomTransport::new(Vec::new()), None, VerificationMode::Permissive).await;

        let Err(error) = outcome else {
            panic!("a subject with neither a referrer nor a `.sbom` tag carries no SBOM");
        };
        assert!(
            matches!(
                error.kind,
                VerifyErrorKind::AttestationNotFound | VerifyErrorKind::NoSignaturesFound
            ),
            "a missing tag must read as not-found, never as a transport failure: {error}",
        );
        assert_eq!(classify_error(&error), ExitCode::NotFound);
    }

    /// A sidecar document lists **beside** a referrer, not instead of it.
    ///
    /// The reason this door is unconditional where `.att`'s is a fallback:
    /// `ocx package sbom` is collect-all, so a document found through the
    /// Referrers API must not hide one attached through the tag. Gating on
    /// `found.is_empty()` — the obvious way to write it — passes every other
    /// test in this section and fails only here.
    #[tokio::test]
    async fn a_sidecar_document_is_listed_beside_a_referrer_not_instead_of_it() {
        let referrer = StubReferrer::sbom("application/spdx+json", RAW_SPDX);
        let transport =
            SbomTransport::new(vec![referrer]).with_sidecar(SBOM_SIDECAR_MANIFEST, &[SBOM_SIDECAR_DOCUMENT]);

        let (outcome, _transport, _state) = drive_sidecar_scan(transport, None, VerificationMode::Permissive).await;

        let scan = outcome.expect("both doors are open");
        let mut documents: Vec<&[u8]> = scan.unverified.iter().map(|sbom| sbom.document.as_slice()).collect();
        documents.sort_unstable();
        let mut expected: Vec<&[u8]> = vec![RAW_SPDX.as_bytes(), SBOM_SIDECAR_DOCUMENT];
        expected.sort_unstable();
        assert_eq!(documents, expected, "both documents must be listed");
    }

    /// `--type` narrows a sidecar entry exactly as it narrows a referrer one.
    ///
    /// Both arms, because either alone is satisfiable by a reader that does not
    /// narrow at all (the CycloneDX arm) or by one that never opens the door
    /// (the SPDX arm).
    #[tokio::test]
    async fn type_narrowing_applies_to_the_sidecar_document() {
        let cyclonedx = drive_sidecar_scan(
            sbom_sidecar_transport(),
            Some(PredicateType::CycloneDx),
            VerificationMode::Permissive,
        )
        .await
        .0
        .expect("the sidecar document is CycloneDX");
        assert_eq!(cyclonedx.unverified.len(), 1, "the requested type is the one attached");

        let spdx = drive_sidecar_scan(
            sbom_sidecar_transport(),
            Some(PredicateType::SpdxJson),
            VerificationMode::Permissive,
        )
        .await
        .0;
        let Err(error) = spdx else {
            panic!("a CycloneDX sidecar must not answer a request for SPDX");
        };
        assert_eq!(classify_error(&error), ExitCode::NotFound);
    }

    /// A `.sbom` layer typed outside the SBOM set is refused by name, through
    /// the same gate the referrer door uses.
    ///
    /// The gate is the whole security property of the permissive path: nothing
    /// here checks a key, so the layer's media type is the only claim about the
    /// bytes, and without the refusal a `.sbom` tag could carry an executable
    /// and be listed as an SBOM.
    #[tokio::test]
    async fn a_sidecar_layer_outside_the_sbom_set_is_refused_by_name() {
        let mutated = String::from_utf8(SBOM_SIDECAR_MANIFEST.to_vec())
            .expect("the committed manifest is UTF-8")
            .replace("application/vnd.cyclonedx+json", "application/octet-stream");
        let transport = SbomTransport::new(Vec::new()).with_sidecar(mutated.as_bytes(), &[SBOM_SIDECAR_DOCUMENT]);

        let (outcome, transport, _state) = drive_sidecar_scan(transport, None, VerificationMode::Permissive).await;

        let Err(error) = outcome else {
            panic!("a layer typed outside the SBOM set must not be listed as an SBOM");
        };
        let VerifyErrorKind::SbomMediaTypeUnsupported { media_type } = &error.kind else {
            panic!("expected the media-type refusal, got: {error}");
        };
        assert_eq!(media_type, "application/octet-stream");
        assert!(
            transport.pulled_blobs().is_empty(),
            "the refusal lands before the document is fetched, got: {:?}",
            transport.pulled_blobs(),
        );
    }

    /// **The demand-mode half.** `--verify` refuses a `.sbom` sidecar as an
    /// unsigned attachment, and refuses it without reading the document.
    ///
    /// 77, not 79, and the same code an unsigned *referrer* already gets: the
    /// document is plainly there — the permissive test above lists it — and
    /// what happened is that a policy demanded a signer this shape cannot have.
    /// A `.sbom` sidecar is unsigned by construction (`cosign attach sbom`
    /// prints "does not sign them"), so this is the whole of its demand-mode
    /// behaviour; there is no third mode in which it verifies.
    ///
    /// Nothing verified on this subject, so the run's whole answer is one
    /// `VerifyErrorKind` and there is no row to name the refused attachment in.
    /// The digest half of the contract lives where a report exists to carry it
    /// — see
    /// [`a_refused_sbom_sidecar_is_reported_by_its_manifest_digest`].
    #[tokio::test]
    async fn a_demanded_scan_refuses_a_sbom_sidecar_without_reading_it() {
        let (outcome, transport, _state) =
            drive_sidecar_scan(sbom_sidecar_transport(), None, VerificationMode::Demand).await;

        let Err(error) = outcome else {
            panic!("an unsigned sidecar is not an answer to a demanded scan");
        };
        assert!(
            matches!(error.kind, VerifyErrorKind::UnsignedRejectedByPolicy),
            "expected the unsigned refusal, got: {error}",
        );
        assert_eq!(classify_error(&error), ExitCode::PermissionDenied);
        assert!(
            transport.pulled_blobs().is_empty(),
            "a demanded scan refuses without reading the document, got: {:?}",
            transport.pulled_blobs(),
        );
    }

    /// Seed one subject with **both** shapes `cosign attest` + `cosign attach
    /// sbom` leave on an image: the signed `sha256-<hex>.att` sidecar, and an
    /// unsigned `sha256-<hex>.sbom` sidecar beside it.
    ///
    /// Addressed at the repository [`verify_id`] names rather than
    /// `SCAN_IMAGE`, because this fixture is driven through
    /// [`VerifyPipeline::run_attestations`] — which resolves its own image
    /// through the index — not through `scan`, which is handed one.
    fn seed_signed_attestation_beside_an_unsigned_sbom_sidecar(
        subject: &Digest,
    ) -> crate::oci::client::test_transport::StubTransportData {
        use crate::oci::client::sibling_tag_reference;
        use crate::oci::client::test_transport::{StubTransport, StubTransportData};

        let client = Client::with_transport(Box::new(StubTransport::new(StubTransportData::new())));
        let image = client.transport_reference(&verify_id());
        let manifest: crate::oci::ImageManifest =
            serde_json::from_str(ATT_MANIFEST).expect("the `.att` manifest parses");
        let layer = manifest.layers.first().expect("one layer").clone();

        let data = StubTransportData::new();
        {
            let mut inner = data.write();
            inner.blobs.insert(layer.digest.clone(), ATT_ENVELOPE.to_vec());
            let att_tag = sibling_tag_reference(
                &image,
                super::simplesigning_read::sidecar_tag(subject, SidecarKind::Attestation),
            );
            inner.manifests.insert(
                att_tag.to_string(),
                (ATT_MANIFEST.as_bytes().to_vec(), layer.digest.clone()),
            );
            // A registry answers a tag with the manifest's own digest, and that
            // answer is the only name this door can ever be reported under —
            // seeded as the real hash so the assertion below is about what the
            // pipeline carried, not about a literal invented here.
            let sbom_tag = sibling_tag_reference(&image, crate::package::tag::sbom_sidecar_tag(subject));
            inner.manifests.insert(
                sbom_tag.to_string(),
                (
                    SBOM_SIDECAR_MANIFEST.to_vec(),
                    crate::oci::Algorithm::Sha256.hash(SBOM_SIDECAR_MANIFEST).to_string(),
                ),
            );
            let pinned = image.clone_with_digest(subject.to_string());
            inner.manifests.insert(
                pinned.to_string(),
                (GOLDEN_SUBJECT_MANIFEST.as_bytes().to_vec(), subject.to_string()),
            );
        }
        data
    }

    /// Drive [`VerifyPipeline::run_attestations`] — the whole attestation
    /// pipeline, not [`VerifyPipeline::scan`] — over a seeded stub registry.
    ///
    /// `refuse_unsigned` and the `AttestationScan` its refusals travel out in
    /// both live *above* the scan, so no `drive_scan*` harness can reach them.
    async fn drive_attestations_run(
        data: crate::oci::client::test_transport::StubTransportData,
        subject: &Digest,
    ) -> Result<AttestationScan, VerifyError> {
        use crate::oci::client::test_transport::StubTransport;

        let client = Client::with_transport(Box::new(StubTransport::new(data)));
        let identifier = verify_id();
        let index = Index::from_impl(ResolvingIndex {
            physical: identifier.clone(),
            resolved: Some((
                subject.clone(),
                crate::oci::Manifest::Image(crate::oci::ImageManifest::default()),
            )),
        });
        let trust_root =
            TrustRoot::load_trusted_root_json(GOLDEN_TRUSTED_ROOT.as_bytes()).expect("the committed trust root loads");
        let policies = [key_policy(GOLDEN_PUBLIC_KEY_PEM)];
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = StateStore::new(temp.path());
        VerifyPipeline::run_attestations(
            &client,
            VerifyContext {
                identifier: &identifier,
                platform: None,
                policies: &policies,
                no_cache: true,
                index: &index,
                trust_root: &trust_root,
                rekor_url: &rekor_url,
                state: &state,
                offline: true,
                content: VerifyContentMode::Attestation { predicate_type: None },
                verification: VerificationMode::Demand,
                signature_format: None,
                allow_unlogged_signature: false,
                report_all: true,
            },
        )
        .await
    }

    /// **The digest half of the `.sbom` refusal contract.** A refused sidecar
    /// is reported by the manifest digest the registry answered its tag with.
    ///
    /// The sibling above cannot assert this and no test can assert it on that
    /// subject: with nothing verified the run's entire answer is one
    /// `VerifyErrorKind`, which carries no digest, so `refused[].referrer_digest`
    /// — the published field (`api::data::sbom::SbomRefusal`) — has no report to
    /// appear in. It reaches an operator exactly when the subject *also* carries
    /// something that verified, which is the ordinary `cosign attest` + `cosign
    /// attach sbom` pairing seeded here.
    ///
    /// Both halves are asserted because either alone is satisfiable by a broken
    /// run: a pipeline that refused the whole subject would leave `matches`
    /// empty, and one that never opened the `.sbom` door would leave `refused`
    /// empty. The digest is the point — the caller has no descriptor for a
    /// tag-addressed door, so an unnamed row would tell an operator only that
    /// *something* was refused.
    #[tokio::test]
    async fn a_refused_sbom_sidecar_is_reported_by_its_manifest_digest() {
        let subject = crate::oci::Algorithm::Sha256.hash(GOLDEN_SUBJECT_MANIFEST.as_bytes());

        let scan = drive_attestations_run(
            seed_signed_attestation_beside_an_unsigned_sbom_sidecar(&subject),
            &subject,
        )
        .await
        .expect("the signed `.att` attestation verifies, so the run has a report to carry rows in");

        assert_eq!(
            scan.matches.len(),
            1,
            "the premise: the subject carries a verified attestation, got: {:?}",
            scan.matches,
        );
        let named: Vec<&str> = scan
            .refused
            .iter()
            .map(|candidate| candidate.referrer_digest.as_str())
            .collect();
        assert_eq!(
            named,
            vec![
                crate::oci::Algorithm::Sha256
                    .hash(SBOM_SIDECAR_MANIFEST)
                    .to_string()
                    .as_str()
            ],
            "the refusal must name the `.sbom` manifest the registry served",
        );
        assert!(
            matches!(scan.refused[0].reason, VerifyErrorKind::UnsignedRejectedByPolicy),
            "expected the unsigned refusal, got: {:?}",
            scan.refused[0].reason,
        );
    }

    /// A transport fault on the `.sbom` probe must not fail a run the signed
    /// pass verified.
    ///
    /// `refuse_unsigned` runs unconditionally, before the signed scan, on every
    /// target of every `Demand` run — so propagating a fault on a tag the
    /// subject almost never has would let one transient registry error fail a
    /// `--verify` that was about to pass. Paired with the accepting half above,
    /// which shares the fixture: without it a run that always failed would
    /// satisfy nothing here.
    #[tokio::test]
    async fn a_faulting_sbom_probe_does_not_fail_a_run_that_verifies() {
        use crate::oci::client::sibling_tag_reference;
        use crate::oci::client::test_transport::{StubTransport, StubTransportData};

        let subject = crate::oci::Algorithm::Sha256.hash(GOLDEN_SUBJECT_MANIFEST.as_bytes());
        let data = seed_signed_attestation_beside_an_unsigned_sbom_sidecar(&subject);
        let client = Client::with_transport(Box::new(StubTransport::new(StubTransportData::new())));
        let sbom_tag = sibling_tag_reference(
            &client.transport_reference(&verify_id()),
            crate::package::tag::sbom_sidecar_tag(&subject),
        );
        data.write()
            .manifest_errors
            .insert(sbom_tag.to_string(), "503 the registry is having a moment".into());

        let scan = drive_attestations_run(data, &subject)
            .await
            .expect("a fault on the sidecar probe may not fail a verified attestation");

        assert_eq!(scan.matches.len(), 1, "the attestation still verifies");
        assert!(
            scan.refused.is_empty(),
            "a probe that faulted names no attachment, got: {:?}",
            scan.refused,
        );
    }

    /// The other half: the deferred fault is **spent**, not dropped, when
    /// nothing verified.
    ///
    /// "I could not finish looking" must not read as "nothing is attached" —
    /// the same rule the `.sig` door states. Without the deferral the answer
    /// here would be `AttestationNotFound`, which states something this run
    /// never got to check.
    #[tokio::test]
    async fn a_faulting_sbom_probe_is_spent_when_nothing_verified() {
        use crate::oci::client::sibling_tag_reference;
        use crate::oci::client::test_transport::{StubTransport, StubTransportData};

        let subject = crate::oci::Algorithm::Sha256.hash(GOLDEN_SUBJECT_MANIFEST.as_bytes());
        let client = Client::with_transport(Box::new(StubTransport::new(StubTransportData::new())));
        let image = client.transport_reference(&verify_id());
        let data = StubTransportData::new();
        {
            let mut inner = data.write();
            inner.manifests.insert(
                image.clone_with_digest(subject.to_string()).to_string(),
                (GOLDEN_SUBJECT_MANIFEST.as_bytes().to_vec(), subject.to_string()),
            );
            inner.manifest_errors.insert(
                sibling_tag_reference(&image, crate::package::tag::sbom_sidecar_tag(&subject)).to_string(),
                "503 the registry is having a moment".into(),
            );
        }

        let Err(error) = drive_attestations_run(data, &subject).await else {
            panic!("nothing is attached to this subject, so nothing can verify");
        };
        // Positive, not `!matches!(.., AttestationNotFound)`: this subject's
        // empty scan answers `NoSignaturesFound`, so a negative assertion would
        // hold whether or not the fault was ever spent. The registry's own
        // message is in the `#[source]` chain, never in the kind's `Display`.
        let VerifyErrorKind::Internal(cause) = &error.kind else {
            panic!(
                "the registry fault must reach the operator rather than a verdict about a tag \
                 this run never read, got: {:?}",
                error.kind,
            );
        };
        assert!(
            cause.to_string().contains("503 the registry is having a moment"),
            "the deferred fault must be the registry's own, got: {cause}",
        );
    }

    /// The OCI 1.1 half of the same gap: cosign's SBOM **referrer** declares
    /// `artifactType: application/vnd.dev.cosign.artifact.sbom.v1+json` while
    /// typing its layer by the document.
    ///
    /// Measured — `COSIGN_EXPERIMENTAL=1 cosign attach sbom
    /// --registry-referrers-mode oci-1-1` writes exactly that pair — and it is
    /// why filtering the listing on document media types alone dropped every
    /// cosign OCI 1.1 SBOM referrer before its manifest was ever fetched.
    ///
    /// Both modes, because they filter through two different call sites
    /// (`scan_unverified` and `refuse_unsigned`) and fixing one is the easy way
    /// to leave the other blind.
    #[tokio::test]
    async fn a_cosign_oci_1_1_sbom_referrer_is_discovered_in_both_modes() {
        let referrer = StubReferrer::mislabelled_sbom(
            COSIGN_SBOM_ARTIFACT_TYPE,
            "application/vnd.cyclonedx+json",
            RAW_CYCLONEDX,
        );

        let (permissive, _transport, _state) =
            drive_sbom_scan(vec![referrer.clone()], None, VerificationMode::Permissive).await;
        let scan = permissive.expect("cosign's own SBOM referrer must be listed");
        assert_eq!(scan.unverified.len(), 1, "one document: {:?}", scan.unverified);
        assert_eq!(scan.unverified[0].document, RAW_CYCLONEDX.as_bytes());
        assert_eq!(
            scan.unverified[0].predicate_type, CYCLONEDX_URI,
            "the label comes from the layer, never from cosign's artifactType",
        );

        let (demanded, _transport, _state) = drive_sbom_scan(vec![referrer], None, VerificationMode::Demand).await;
        let Err(error) = demanded else {
            panic!("an unsigned cosign SBOM referrer is not an answer to a demanded scan");
        };
        assert!(
            matches!(error.kind, VerifyErrorKind::UnsignedRejectedByPolicy),
            "expected the unsigned refusal, got: {error}",
        );
    }

    // ── §WP5: the `.att` sidecar door, and the mode gate on it ─────────────

    /// cosign's key-mode `.att` sidecar and the DSSE envelope its one layer
    /// holds — the same committed capture `attestation_sidecar`'s own tests
    /// read. The reader is tested there; what is tested *here* is that `scan`
    /// opens the door at all, which no unit test of the reader can observe.
    const ATT_MANIFEST: &str =
        include_str!("../../../../../test/tests/fixtures/golden/attestation_sidecar_key_manifest.json");
    const ATT_ENVELOPE: &[u8] =
        include_bytes!("../../../../../test/tests/fixtures/golden/attestation_sidecar_key_envelope.json");

    /// Seed the `sha256-<hex>.att` tag, its envelope blob, and the subject
    /// manifest the keyless arm would need — no referrer listing anywhere, so a
    /// match can only have come through the tag door.
    fn seed_attestation_sidecar_tag(subject: &Digest) -> crate::oci::client::test_transport::StubTransportData {
        use crate::oci::client::sibling_tag_reference;
        use crate::oci::client::test_transport::StubTransportData;

        let image: native::Reference = SCAN_IMAGE.parse().expect("stub reference");
        let manifest: crate::oci::ImageManifest =
            serde_json::from_str(ATT_MANIFEST).expect("the `.att` manifest parses");
        let layer = manifest.layers.first().expect("one layer").clone();

        let data = StubTransportData::new();
        {
            let mut inner = data.write();
            inner.blobs.insert(layer.digest.clone(), ATT_ENVELOPE.to_vec());
            let tag_ref = sibling_tag_reference(
                &image,
                super::simplesigning_read::sidecar_tag(subject, SidecarKind::Attestation),
            );
            inner.manifests.insert(
                tag_ref.to_string(),
                (ATT_MANIFEST.as_bytes().to_vec(), layer.digest.clone()),
            );
            let pinned = image.clone_with_digest(subject.to_string());
            inner.manifests.insert(
                pinned.to_string(),
                (GOLDEN_SUBJECT_MANIFEST.as_bytes().to_vec(), subject.to_string()),
            );
        }
        data
    }

    /// **§WP5.** An attestation run reaches the `sha256-<hex>.att` tag with no
    /// referrer listing to help it, and reports what it found as an
    /// attestation.
    ///
    /// This is the wiring assertion: the reader's own tests call it directly,
    /// so only a scan-level test can show that attestation mode opens the door
    /// — and that `scan` does not bail out at its "no candidates" early return
    /// before getting there.
    #[tokio::test]
    async fn an_attestation_run_finds_a_cosign_att_sidecar_by_tag() {
        let subject = crate::oci::Algorithm::Sha256.hash(GOLDEN_SUBJECT_MANIFEST.as_bytes());
        let policies = [key_policy(GOLDEN_PUBLIC_KEY_PEM)];
        let trust_root =
            TrustRoot::load_trusted_root_json(GOLDEN_TRUSTED_ROOT.as_bytes()).expect("the committed trust root loads");

        let outcome = drive_scan_in_mode(
            seed_attestation_sidecar_tag(&subject),
            &subject,
            &policies,
            &trust_root,
            None,
            ScanArity::All,
            VerifyContentMode::Attestation { predicate_type: None },
        )
        .await
        .expect("the `.att` sidecar verifies");

        assert_eq!(outcome.matches.len(), 1);
        let (result, attestation) = &outcome.matches[0];
        assert_eq!(result.discovery_method, DiscoveryMethod::SidecarTag);
        assert_eq!(
            attestation
                .as_ref()
                .expect("an attestation run carries the document")
                .predicate_type,
            "https://cyclonedx.org/bom",
        );
    }

    // ── C-022: the run's Rekor log-key memo ────────────────────────────────

    /// **C-022, the trust half.** Two logs, two pinned keys, and the memo
    /// answers each log id with its own.
    ///
    /// `log_id_hex` arrives from an untrusted sidecar manifest, and
    /// [`TrustRoot::rekor_public_key_pem_for`] answers *per log* — so a memo
    /// keyed on nothing would hand the first entry's key to every entry after
    /// it, and a rotated trust root's second log would have its SET checked
    /// against the first log's key. That is the confusion
    /// `the_rekor_key_is_selectable_by_log_id_and_falls_back_when_unknown` pins
    /// out of the selector, re-introduced one layer above it.
    ///
    /// Offline throughout, so no assertion here can be satisfied by a network
    /// round trip: both answers are pinned material, and the claim is that they
    /// are *different* answers.
    #[tokio::test]
    async fn the_rekor_memo_answers_each_log_id_with_its_own_key() {
        let trust_root = TrustRoot::from_material(
            Vec::new(),
            std::collections::BTreeMap::new(),
            std::collections::BTreeMap::from([
                ("aa".to_string(), vec![1_u8, 1, 1]),
                ("bb".to_string(), vec![2_u8, 2, 2]),
            ]),
        );
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let memo = RekorKeyMemo::default();
        let pem_of = |der: &[u8]| pem::encode(&pem::Pem::new("PUBLIC KEY", der.to_vec()));

        let first = memo
            .resolve(&trust_root, &rekor_url, true, "aa")
            .await
            .expect("the first log's key is pinned");
        let second = memo
            .resolve(&trust_root, &rekor_url, true, "bb")
            .await
            .expect("the second log's key is pinned");

        assert_eq!(first, pem_of(&[1, 1, 1]), "the first log resolves to its own key");
        assert_eq!(
            second,
            pem_of(&[2, 2, 2]),
            "the second log must resolve to the SECOND log's key -- an unkeyed memo answers with the first's",
        );
        assert_eq!(
            memo.resolve(&trust_root, &rekor_url, true, "aa")
                .await
                .expect("the first log is still pinned"),
            first,
            "a second look at the first log is unchanged by the second log's resolution",
        );
    }

    /// **C-022, the refetch half (#374, #319).** One log id is fetched once,
    /// however many candidates ask for it.
    ///
    /// Every simplesigning layer of a cosign sidecar and every candidate on the
    /// bundle path resolved the log key independently, so an unpinned run made
    /// one `/api/v1/log/publicKey` request per entry.
    ///
    /// The stub log answers and counts. `Connection: close` on every response
    /// makes one connection one request, so the counter cannot be confused by
    /// keep-alive.
    #[tokio::test]
    async fn one_log_id_is_fetched_once_however_many_candidates_ask() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        use tokio::net::TcpListener;

        const STUB_KEY_PEM: &str = "-----BEGIN PUBLIC KEY-----\nc3R1Yg==\n-----END PUBLIC KEY-----\n";

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind the rekor stub");
        let addr = listener.local_addr().expect("the rekor stub has an address");
        let hits = Arc::new(AtomicUsize::new(0));
        let served = Arc::clone(&hits);
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                served.fetch_add(1, Ordering::SeqCst);
                let mut scratch = [0_u8; 2048];
                let _ = socket.read(&mut scratch).await;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{STUB_KEY_PEM}",
                    STUB_KEY_PEM.len(),
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });

        // No Rekor key at all: `rekor_public_key_pem_for` falls back to the
        // first key and there is none, so every unmemoized resolution is a
        // fetch. A trust root that pins ANY key would never reach the network
        // and the counter could not go red.
        let trust_root = TrustRoot::from_material(
            Vec::new(),
            std::collections::BTreeMap::new(),
            std::collections::BTreeMap::new(),
        );
        let rekor_url = Url::parse(&format!("http://{addr}/")).expect("the rekor stub url parses");
        let memo = RekorKeyMemo::default();

        for _ in 0..4 {
            assert_eq!(
                memo.resolve(&trust_root, &rekor_url, false, "aa")
                    .await
                    .expect("the stub log serves its key"),
                STUB_KEY_PEM,
                "every candidate gets the same key",
            );
        }

        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "four candidates, one fetch -- without the memo this is four (#374, #319)",
        );
    }

    /// A failed resolution is **not** cached: the next candidate tries again.
    ///
    /// The ANY-of half of C-022. One transient Rekor 5xx while resolving
    /// candidate 1 must not decide candidate 2, or a flaky fetch is promoted
    /// into a whole-scan refusal. The stub refuses once and then serves, so a
    /// memo that cached the `Err` leaves the second call refused.
    #[tokio::test]
    async fn a_failed_rekor_resolution_is_not_memoized() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        use tokio::net::TcpListener;

        const STUB_KEY_PEM: &str = "-----BEGIN PUBLIC KEY-----\nc3R1Yg==\n-----END PUBLIC KEY-----\n";

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind the rekor stub");
        let addr = listener.local_addr().expect("the rekor stub has an address");
        let answered = Arc::new(AtomicUsize::new(0));
        let served = Arc::clone(&answered);
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let first = served.fetch_add(1, Ordering::SeqCst) == 0;
                let mut scratch = [0_u8; 2048];
                let _ = socket.read(&mut scratch).await;
                let response = if first {
                    "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
                } else {
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{STUB_KEY_PEM}",
                        STUB_KEY_PEM.len(),
                    )
                };
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });

        let trust_root = TrustRoot::from_material(
            Vec::new(),
            std::collections::BTreeMap::new(),
            std::collections::BTreeMap::new(),
        );
        let rekor_url = Url::parse(&format!("http://{addr}/")).expect("the rekor stub url parses");
        let memo = RekorKeyMemo::default();

        let refused = memo.resolve(&trust_root, &rekor_url, false, "aa").await;
        assert!(
            matches!(refused, Err(VerifyErrorKind::TransparencyLogUnavailable)),
            "the log was down for the first candidate: {refused:?}",
        );
        assert_eq!(
            memo.resolve(&trust_root, &rekor_url, false, "aa")
                .await
                .expect("the log is up again for the second candidate"),
            STUB_KEY_PEM,
            "a cached Err would refuse every later candidate off one transient fault",
        );
    }

    // ── C-023: a sidecar verify populates the offline trust cache ──────────

    /// **C-023, the mechanism.** The one Rekor key a sidecar verify resolved is
    /// written to the trust-root cache, and nothing is written when there is no
    /// single key to write.
    ///
    /// Three states off one helper, because each alone is satisfied by a wrong
    /// implementation: "always writes" passes the first, "never writes" passes
    /// the last two.
    #[tokio::test]
    async fn a_sidecar_verify_caches_the_one_rekor_key_it_resolved() {
        async fn cached_pem(offline: bool, log_ids: &[&str]) -> Option<String> {
            let trust_root = TrustRoot::from_material(
                Vec::new(),
                std::collections::BTreeMap::new(),
                std::collections::BTreeMap::from([
                    ("aa".to_string(), vec![1_u8, 1, 1]),
                    ("bb".to_string(), vec![2_u8, 2, 2]),
                ]),
            );
            let identifier = verify_id();
            let index = Index::from_impl(IndirectingIndex {
                physical: identifier.clone(),
            });
            let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
            let temp = tempfile::TempDir::new().expect("state dir");
            let state = StateStore::new(temp.path());
            let ctx = VerifyContext {
                identifier: &identifier,
                platform: None,
                policies: &[],
                no_cache: true,
                index: &index,
                trust_root: &trust_root,
                rekor_url: &rekor_url,
                state: &state,
                offline,
                content: VerifyContentMode::Signature,
                verification: VerificationMode::Demand,
                signature_format: None,
                allow_unlogged_signature: false,
                report_all: false,
            };

            let memo = RekorKeyMemo::default();
            for log_id in log_ids {
                // Resolved offline whatever the run's posture: these are pinned
                // keys, so the resolution itself never reaches a network and the
                // only thing `offline` decides here is whether the cache is
                // written.
                memo.resolve(&trust_root, &rekor_url, true, log_id)
                    .await
                    .expect("the log is pinned");
            }

            cache_sidecar_trust_material(&ctx, &memo).await;

            TrustRootCache::from_cache(
                &crate::oci::verify::trust_cache::cache_key_for_rekor(&rekor_url),
                &state,
            )
            .await
            .expect("the cache reads")
            .and_then(|entry| entry.rekor_public_key_pem)
        }

        let pem_of = |der: &[u8]| pem::encode(&pem::Pem::new("PUBLIC KEY", der.to_vec()));
        assert_eq!(
            cached_pem(false, &["aa"]).await.as_deref(),
            Some(pem_of(&[1, 1, 1]).as_str()),
            "the key the verify used is what a later offline verify needs",
        );
        assert_eq!(
            cached_pem(false, &[]).await,
            None,
            "a key-mode sidecar resolves no log key, so there is nothing to cache",
        );
        assert_eq!(
            cached_pem(false, &["aa", "bb"]).await,
            None,
            "two logs, one cache slot: guessing would hand a later offline verify the wrong key",
        );
        assert_eq!(
            cached_pem(true, &["aa"]).await,
            None,
            "an offline run learned nothing online and writes nothing",
        );
    }

    /// The golden keyless bundle's own DSSE envelope, re-serialized as the layer
    /// body an `.att` sidecar carries.
    fn golden_dsse_envelope() -> Vec<u8> {
        let bundle: serde_json::Value =
            serde_json::from_str(GOLDEN_KEYLESS_BUNDLE).expect("the golden keyless bundle is JSON");
        serde_json::to_vec(
            bundle
                .get("dsseEnvelope")
                .expect("a keyless bundle carries a DSSE envelope"),
        )
        .expect("the envelope re-serializes")
    }

    /// The golden keyless bundle's own Fulcio leaf, PEM-encoded the way the
    /// `dev.sigstore.cosign/certificate` annotation carries it.
    fn golden_leaf_pem() -> String {
        use base64::Engine as _;

        let bundle: serde_json::Value =
            serde_json::from_str(GOLDEN_KEYLESS_BUNDLE).expect("the golden keyless bundle is JSON");
        let der = base64::engine::general_purpose::STANDARD
            .decode(
                bundle
                    .pointer("/verificationMaterial/certificate/rawBytes")
                    .and_then(serde_json::Value::as_str)
                    .expect("a keyless bundle carries a certificate"),
            )
            .expect("rawBytes is base64");
        pem::encode(&pem::Pem::new("CERTIFICATE", der))
    }

    /// The golden bundle's own `tlogEntries[0]`, re-spelled as cosign's offline
    /// `dev.sigstore.cosign/bundle` annotation.
    ///
    /// Nothing is minted: the body, the instant, the log index, the log id and
    /// the Signed Entry Timestamp are all read out of the committed capture, so
    /// the SET this sidecar carries is one the committed trust root's Rekor key
    /// actually verifies.
    fn golden_offline_bundle_annotation() -> String {
        use base64::Engine as _;

        let base64 = base64::engine::general_purpose::STANDARD;
        let bundle: serde_json::Value =
            serde_json::from_str(GOLDEN_KEYLESS_BUNDLE).expect("the golden keyless bundle is JSON");
        let entry = &bundle["verificationMaterial"]["tlogEntries"][0];
        let number = |field: &str| -> i64 {
            entry[field]
                .as_str()
                .expect("the entry field is a JSON string")
                .parse()
                .expect("the entry field is an integer")
        };
        let log_id = base64
            .decode(entry["logId"]["keyId"].as_str().expect("the entry names a log"))
            .expect("the log id is base64");
        serde_json::json!({
            "SignedEntryTimestamp": entry["inclusionPromise"]["signedEntryTimestamp"],
            "Payload": {
                "body": entry["canonicalizedBody"],
                "integratedTime": number("integratedTime"),
                "logIndex": number("logIndex"),
                "logID": hex::encode(log_id),
            }
        })
        .to_string()
    }

    /// The keyless `.att` sidecar layer cosign 2.x wrote: the golden bundle's
    /// own envelope as the layer body, its own Fulcio leaf as the certificate
    /// annotation, and its own transparency-log entry as the bundle annotation.
    ///
    /// The same construction `attestation_sidecar`'s tests use, repeated here
    /// rather than shared: a test fixture that reaches across module boundaries
    /// couples two suites that are meant to fail independently.
    fn keyless_att_sidecar() -> (String, Vec<u8>) {
        use crate::oci::referrer::media_types::{
            ANNOTATION_COSIGN_BUNDLE, ANNOTATION_COSIGN_CERTIFICATE, DSSE_ENVELOPE_MEDIA_TYPE,
        };

        let envelope = golden_dsse_envelope();
        let manifest = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "config": {
                "mediaType": "application/vnd.oci.image.config.v1+json",
                "size": 0,
                "digest": crate::oci::Algorithm::Sha256.hash(b"").to_string(),
            },
            "layers": [{
                "mediaType": DSSE_ENVELOPE_MEDIA_TYPE,
                "size": envelope.len(),
                "digest": crate::oci::Algorithm::Sha256.hash(&envelope).to_string(),
                "annotations": {
                    ANNOTATION_COSIGN_CERTIFICATE: golden_leaf_pem(),
                    ANNOTATION_COSIGN_BUNDLE: golden_offline_bundle_annotation(),
                },
            }],
        });
        (manifest.to_string(), envelope)
    }

    /// [`drive_scan_in_mode`], but **online** and keeping the state directory,
    /// so a test can read back what the scan wrote into the trust-root cache.
    async fn drive_scan_online(
        data: crate::oci::client::test_transport::StubTransportData,
        subject_digest: &Digest,
        policies: &[crate::trust::CompiledPolicy],
        trust_root: &TrustRoot,
        content: VerifyContentMode,
    ) -> (Result<ScanOutcome, VerifyErrorKind>, tempfile::TempDir, Url) {
        use crate::oci::client::test_transport::StubTransport;

        let client = Client::with_transport(Box::new(StubTransport::new(data)));
        let image: native::Reference = SCAN_IMAGE.parse().expect("stub reference");
        let identifier = verify_id();
        let index = Index::from_impl(IndirectingIndex {
            physical: identifier.clone(),
        });
        // Never dialled: the committed trust root pins this stack's Rekor key,
        // so the resolution is answered from trust material and the URL is only
        // the cache key.
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let temp = tempfile::TempDir::new().expect("state dir");
        let outcome = {
            let state = StateStore::new(temp.path());
            let ctx = VerifyContext {
                identifier: &identifier,
                platform: None,
                policies,
                no_cache: true,
                index: &index,
                trust_root,
                rekor_url: &rekor_url,
                state: &state,
                offline: false,
                content: content.clone(),
                verification: VerificationMode::Demand,
                signature_format: None,
                allow_unlogged_signature: false,
                report_all: true,
            };
            let target = ScanTarget {
                image,
                subject_digest: subject_digest.clone(),
                enclosing_index: None,
                index_members: Vec::new(),
            };
            let mut budget = ScanBudget::new(content.caps());
            VerifyPipeline::scan(&client, &ctx, &target, ScanArity::All, &mut budget).await
        };
        (outcome, temp, rekor_url)
    }

    /// **C-023 / S-008, the wiring.** A keyless sidecar verify populates
    /// `state/trust_root/<authority>.json`, so the next `--offline` verify of
    /// the same subject has the material it needs.
    ///
    /// The bundle path has always cached here; the sidecar doors never did,
    /// because the log key is resolved several frames down in
    /// `simplesigning_read::logged_entry` and nothing it returns carries the key
    /// back out (#374). The mechanism is asserted by
    /// `a_sidecar_verify_caches_the_one_rekor_key_it_resolved`; what this adds
    /// is that a real scan reaches it — a test of the helper alone stays green
    /// with the call site deleted.
    ///
    /// The cached key is compared against the trust root's own pinned key rather
    /// than a transcription, so a cache written from the wrong material reds.
    #[tokio::test]
    async fn a_keyless_sidecar_verify_writes_the_offline_trust_cache() {
        use crate::oci::client::sibling_tag_reference;
        use crate::oci::client::test_transport::StubTransportData;

        let subject = crate::oci::Algorithm::Sha256.hash(GOLDEN_SUBJECT_MANIFEST.as_bytes());
        let (manifest, envelope) = keyless_att_sidecar();
        let image: native::Reference = SCAN_IMAGE.parse().expect("stub reference");
        let layer = {
            let parsed: crate::oci::ImageManifest =
                serde_json::from_str(&manifest).expect("the keyless `.att` manifest parses");
            parsed.layers.first().expect("one layer").clone()
        };
        let data = StubTransportData::new();
        {
            let mut inner = data.write();
            inner.blobs.insert(layer.digest.clone(), envelope.clone());
            let tag_ref = sibling_tag_reference(
                &image,
                super::simplesigning_read::sidecar_tag(&subject, SidecarKind::Attestation),
            );
            inner.manifests.insert(
                tag_ref.to_string(),
                (manifest.as_bytes().to_vec(), layer.digest.clone()),
            );
            let pinned = image.clone_with_digest(subject.to_string());
            inner.manifests.insert(
                pinned.to_string(),
                (GOLDEN_SUBJECT_MANIFEST.as_bytes().to_vec(), subject.to_string()),
            );
        }

        let trust_root =
            TrustRoot::load_trusted_root_json(GOLDEN_TRUSTED_ROOT.as_bytes()).expect("the committed trust root loads");
        let policies = [crate::trust::CompiledPolicy {
            builder: None,
            backends: vec![crate::trust::PolicyBackend::Keyless(crate::trust::CompiledKeyless {
                identity: crate::trust::IdentityRule::Exact(GOLDEN_IDENTITY.to_string()),
                issuer: GOLDEN_ISSUER.to_string(),
            })],
        }];

        let (outcome, temp, rekor_url) = drive_scan_online(
            data,
            &subject,
            &policies,
            &trust_root,
            VerifyContentMode::Attestation { predicate_type: None },
        )
        .await;

        let scan = outcome.expect("the keyless `.att` sidecar verifies against the committed trust root");
        assert_eq!(scan.matches.len(), 1, "one attestation: {:?}", scan.refused);

        let state = StateStore::new(temp.path());
        let cached = TrustRootCache::from_cache(
            &crate::oci::verify::trust_cache::cache_key_for_rekor(&rekor_url),
            &state,
        )
        .await
        .expect("the cache reads")
        .expect("a sidecar verify leaves the trust material behind for the next offline run");
        let bundle: serde_json::Value =
            serde_json::from_str(GOLDEN_KEYLESS_BUNDLE).expect("the golden keyless bundle is JSON");
        let log_id = {
            use base64::Engine as _;
            hex::encode(
                base64::engine::general_purpose::STANDARD
                    .decode(
                        bundle["verificationMaterial"]["tlogEntries"][0]["logId"]["keyId"]
                            .as_str()
                            .expect("the entry names a log"),
                    )
                    .expect("the log id is base64"),
            )
        };
        assert_eq!(
            cached.rekor_public_key_pem,
            trust_root.rekor_public_key_pem_for(&log_id),
            "the cached key must be the one this verify resolved for this log",
        );
        assert_eq!(
            cached.fulcio_der_certs,
            trust_root.der_certs().to_vec(),
            "the Fulcio anchors travel with it, or the offline verify has no chain to build",
        );
    }

    /// **The `.att` door is fail-closed on a refused bundle**, the same rule
    /// the `.sig` door has carried since the fallback gate grew
    /// `refused.is_empty()`.
    ///
    /// A subject can carry a current SLSA provenance as a bundle referrer
    /// *and* a stale but validly-signed `.att` sidecar. Corrupting the bundle
    /// — one flipped byte, no forgery — empties `matches` exactly as a missing
    /// bundle would, and a door gated on `matches.is_empty()` alone then
    /// promotes the sidecar, which passes on its own merits and is reported at
    /// exit 0 as the subject's provenance. That is an attacker *choosing*
    /// which signed attestation OCX answers with by breaking the others, and
    /// `budget.stop_reason()` does not catch it: a cryptographic refusal
    /// stamps no bound.
    ///
    /// Two runs over the same seeded registry, differing only in whether the
    /// refused bundle is listed. The control comes first and is the same
    /// sidecar seed: without it a sidecar that had stopped verifying would
    /// satisfy the refusal half vacuously.
    #[tokio::test]
    async fn a_refused_bundle_does_not_open_the_attestation_sidecar_door() {
        use crate::oci::client::test_transport::referrers_key;

        let subject = crate::oci::Algorithm::Sha256.hash(GOLDEN_SUBJECT_MANIFEST.as_bytes());
        let policies = [key_policy(GOLDEN_PUBLIC_KEY_PEM)];
        let trust_root =
            TrustRoot::load_trusted_root_json(GOLDEN_TRUSTED_ROOT.as_bytes()).expect("the committed trust root loads");

        let seed = |with_refused_bundle: bool| {
            let data = seed_attestation_sidecar_tag(&subject);
            if with_refused_bundle {
                let image: native::Reference = SCAN_IMAGE.parse().expect("stub reference");
                // Refused from its descriptor alone — a declared size past the
                // per-manifest cap. A flipped byte in the bundle blob produces
                // the same `RefusedCandidate` one fetch later; the gate cannot
                // tell the two apart, which is the whole point of gating on
                // the refusal rather than on how it was reached.
                let mut descriptor = referrer_descriptor(b"refused bundle", SIGSTORE_BUNDLE_V03);
                descriptor.size = MAX_REFERRER_MANIFEST_BYTES as i64 + 1;
                data.write()
                    .referrers
                    .entry(referrers_key(&image, &subject))
                    .or_default()
                    .push(descriptor);
            }
            data
        };

        let run = async |with_refused_bundle: bool| {
            drive_scan_in_mode(
                seed(with_refused_bundle),
                &subject,
                &policies,
                &trust_root,
                None,
                ScanArity::All,
                VerifyContentMode::Attestation { predicate_type: None },
            )
            .await
        };

        let control = run(false)
            .await
            .expect("with nothing refused the `.att` door opens and the sidecar verifies");
        assert_eq!(control.matches.len(), 1);
        assert_eq!(control.matches[0].0.discovery_method, DiscoveryMethod::SidecarTag);
        assert!(
            control.refused.is_empty(),
            "the control must refuse nothing, or it proves nothing about the other half: {:?}",
            control.refused,
        );

        // The *kind*, not `is_err()`: the refusal must be the bundle's own
        // verdict travelling out through `finish_scan`'s `best_failure`, not
        // some unrelated failure that would also be satisfied by a door wired
        // shut.
        let suppressed = run(true).await;
        assert!(
            matches!(suppressed, Err(VerifyErrorKind::BundleParseFailed)),
            "a rejected bundle must carry its own verdict out, never be answered around \
             with the sidecar sitting beside it: {suppressed:?}",
        );
    }

    /// The other half of the gate above: **a candidate that recorded no
    /// refusal must not shut the door.**
    ///
    /// A cosign *image signature* bundle met during an attestation run is
    /// discriminated as `CandidateOutcome::ModeMismatch` before any crypto
    /// runs — nothing about an attestation was rejected there — so it spends a
    /// slot and records nothing, and a subject whose only bundle is a
    /// signature must still reach its `.att` sidecar. This is the carve-out
    /// the `.sig` gate documents: gating on `candidates.is_empty()` instead of
    /// `refused.is_empty()` would refuse such a subject, and it would pass the
    /// test above, whose seed makes `candidates` and `refused` non-empty
    /// together. Only the pair separates the two spellings.
    #[tokio::test]
    async fn a_mode_mismatched_bundle_still_leaves_the_attestation_sidecar_door_open() {
        use crate::oci::client::test_transport::referrers_key;

        let subject = crate::oci::Algorithm::Sha256.hash(GOLDEN_SUBJECT_MANIFEST.as_bytes());
        let policies = [key_policy(GOLDEN_PUBLIC_KEY_PEM)];
        let trust_root =
            TrustRoot::load_trusted_root_json(GOLDEN_TRUSTED_ROOT.as_bytes()).expect("the committed trust root loads");

        let data = seed_attestation_sidecar_tag(&subject);
        let blob = serde_json::to_vec(&into_signature_bundle(message_bundle(true, true)))
            .expect("the signature bundle serializes");
        let blob_digest = crate::oci::Algorithm::Sha256.hash(&blob);
        let (descriptor, referrer_bytes) = referrer_with(&subject, &blob_digest, blob.len() as i64, None);
        {
            let image: native::Reference = SCAN_IMAGE.parse().expect("stub reference");
            let mut inner = data.write();
            inner.blobs.insert(blob_digest.to_string(), blob);
            inner
                .referrers
                .entry(referrers_key(&image, &subject))
                .or_default()
                .push(descriptor.clone());
            let referrer_ref = image.clone_with_digest(descriptor.digest.clone());
            inner
                .manifests
                .insert(referrer_ref.to_string(), (referrer_bytes, descriptor.digest.clone()));
        }

        let outcome = drive_scan_in_mode(
            data,
            &subject,
            &policies,
            &trust_root,
            None,
            ScanArity::All,
            VerifyContentMode::Attestation { predicate_type: None },
        )
        .await
        .expect("a signature bundle rejects nothing, so the `.att` door must still open");

        assert!(
            outcome.refused.is_empty(),
            "a mode mismatch records no refusal, or this proves nothing: {:?}",
            outcome.refused,
        );
        assert_eq!(outcome.matches.len(), 1, "the `.att` sidecar must still verify");
        assert_eq!(outcome.matches[0].0.discovery_method, DiscoveryMethod::SidecarTag);
    }

    /// The mode gate, asserted from the other side. The **same** seeded
    /// registry under a *signature* run finds nothing: an `.att` layer is a
    /// DSSE attestation, and letting it answer "is this artifact signed" would
    /// be the defect.
    ///
    /// Paired with the test above deliberately — a door wired open
    /// unconditionally passes the first and fails this one, and only the pair
    /// shows the gate tracks the content mode.
    #[tokio::test]
    async fn a_signature_run_never_reads_the_att_sidecar() {
        let subject = crate::oci::Algorithm::Sha256.hash(GOLDEN_SUBJECT_MANIFEST.as_bytes());
        let policies = [key_policy(GOLDEN_PUBLIC_KEY_PEM)];
        let trust_root =
            TrustRoot::load_trusted_root_json(GOLDEN_TRUSTED_ROOT.as_bytes()).expect("the committed trust root loads");

        let verdict = drive_scan(
            seed_attestation_sidecar_tag(&subject),
            &subject,
            &policies,
            &trust_root,
            None,
            ScanArity::All,
        )
        .await;

        // The *kind*, not "an error": this registry carries no signature
        // referrer at all, so a bare `is_err()` is satisfied by every failure
        // the pipeline can raise — including ones proving the mode gate never
        // ran. `no_signatures_found` is the verdict a subject with nothing
        // signature-shaped must get.
        assert!(
            matches!(verdict, Err(VerifyErrorKind::NoSignaturesFound)),
            "a signature run must not accept an `.att` attestation: {verdict:?}",
        );
    }

    /// Seed an `.att` tag whose manifest lists the **same** golden DSSE layer
    /// descriptor `count` times, over one blob.
    ///
    /// Legal JSON, one ~1 KiB blob, and the shape both the dedup pass and the
    /// truncation refusal are aimed at: repeating a descriptor is the cheapest
    /// way for a registry to inflate `signatures[]` or to push a real
    /// attestation past the candidate cap.
    fn seed_att_tag_with_repeated_layer(
        subject: &Digest,
        count: usize,
    ) -> crate::oci::client::test_transport::StubTransportData {
        use crate::oci::client::sibling_tag_reference;
        use crate::oci::client::test_transport::StubTransportData;

        let image: native::Reference = SCAN_IMAGE.parse().expect("stub reference");
        let parsed: serde_json::Value = serde_json::from_str(ATT_MANIFEST).expect("the `.att` manifest is JSON");
        let layer = parsed["layers"][0].clone();
        let layer_digest = layer["digest"].as_str().expect("the layer names a digest").to_owned();
        let mut repeated = parsed.clone();
        repeated["layers"] = serde_json::Value::Array(vec![layer; count]);
        let manifest = repeated.to_string();

        let data = StubTransportData::new();
        {
            let mut inner = data.write();
            inner.blobs.insert(layer_digest.clone(), ATT_ENVELOPE.to_vec());
            let tag_ref = sibling_tag_reference(
                &image,
                super::simplesigning_read::sidecar_tag(subject, SidecarKind::Attestation),
            );
            inner
                .manifests
                .insert(tag_ref.to_string(), (manifest.into_bytes(), layer_digest));
            let pinned = image.clone_with_digest(subject.to_string());
            inner.manifests.insert(
                pinned.to_string(),
                (GOLDEN_SUBJECT_MANIFEST.as_bytes().to_vec(), subject.to_string()),
            );
        }
        data
    }

    /// **D6 on the `.att` door.** One layer descriptor listed twice, over one
    /// blob, contributes **one** row — the same dedup pass the bundle loop and
    /// the `.sig` door run, which this door skipped entirely.
    ///
    /// The premise is asserted from the other side by the truncation test
    /// below: without dedup the same manifest yields two matches, which is what
    /// makes an inflated `signatures[]` count cheap to manufacture.
    #[tokio::test]
    async fn an_att_layer_listed_twice_contributes_one_attestation() {
        let subject = crate::oci::Algorithm::Sha256.hash(GOLDEN_SUBJECT_MANIFEST.as_bytes());
        let policies = [key_policy(GOLDEN_PUBLIC_KEY_PEM)];
        let trust_root =
            TrustRoot::load_trusted_root_json(GOLDEN_TRUSTED_ROOT.as_bytes()).expect("the committed trust root loads");

        let outcome = drive_scan_in_mode(
            seed_att_tag_with_repeated_layer(&subject, 2),
            &subject,
            &policies,
            &trust_root,
            None,
            ScanArity::All,
            VerifyContentMode::Attestation { predicate_type: None },
        )
        .await
        .expect("the `.att` sidecar verifies");

        assert_eq!(
            outcome.matches.len(),
            1,
            "one blob reached through two descriptors is one attestation: {:?}",
            outcome.matches,
        );
    }

    /// **The truncation refusal, fail-closed.** An `.att` manifest carrying more
    /// DSSE layers than the candidate cap is refused outright, never answered
    /// with the prefix that fitted.
    ///
    /// The attack the pair measures: repeat one genuine layer descriptor
    /// `MAX_ATTESTATION_CANDIDATES` times and put the real attestation after
    /// them. Legal JSON, one blob, ~32 KiB of manifest — and with the reader
    /// truncating behind a `tracing::debug!` the run exits 0 having never
    /// fetched the 33rd. `ocx package sbom` asks "which SBOMs does this carry",
    /// and a scan that stopped early cannot answer it.
    ///
    /// Both halves, over the same fixture and the same door: exactly at the cap
    /// verifies, one past it refuses. Either alone is satisfied by a gate that
    /// always refuses, or by one that never does.
    #[tokio::test]
    async fn an_att_sidecar_over_the_candidate_cap_is_refused_rather_than_truncated() {
        let subject = crate::oci::Algorithm::Sha256.hash(GOLDEN_SUBJECT_MANIFEST.as_bytes());
        let policies = [key_policy(GOLDEN_PUBLIC_KEY_PEM)];
        let trust_root =
            TrustRoot::load_trusted_root_json(GOLDEN_TRUSTED_ROOT.as_bytes()).expect("the committed trust root loads");

        let scan = async |count: usize| {
            drive_scan_in_mode(
                seed_att_tag_with_repeated_layer(&subject, count),
                &subject,
                &policies,
                &trust_root,
                None,
                ScanArity::All,
                VerifyContentMode::Attestation { predicate_type: None },
            )
            .await
        };

        let at_cap = scan(MAX_ATTESTATION_CANDIDATES).await;
        assert_eq!(
            at_cap
                .expect("a sidecar exactly at the cap is not truncated")
                .matches
                .len(),
            1,
            "the accepting half: every layer was looked at, and dedup made them one",
        );

        let over_cap = scan(MAX_ATTESTATION_CANDIDATES + 1).await;
        assert!(
            matches!(
                over_cap,
                Err(VerifyErrorKind::TooManyAttestations { limit }) if limit == MAX_ATTESTATION_CANDIDATES
            ),
            "a truncated `.att` scan must refuse, never report the prefix that fitted: {over_cap:?}",
        );
    }

    /// **S-017 through the door, not just inside the reader.** `--type` is
    /// threaded into the `.att` gate: a run asking for SPDX must not be handed
    /// the sidecar's CycloneDX document.
    ///
    /// The reader's own test covers the narrowing rule; this one covers the
    /// wiring, which is invisible to it — dropping `predicate_type` at the gate
    /// leaves every reader test green while `ocx package sbom --type spdxjson`
    /// reports a CycloneDX attestation as a match.
    ///
    /// Paired: the unnarrowed run over the same registry finds the document, so
    /// "not found" here means the type narrowed it rather than the door being
    /// shut.
    #[tokio::test]
    async fn the_att_door_narrows_on_the_requested_predicate_type() {
        let subject = crate::oci::Algorithm::Sha256.hash(GOLDEN_SUBJECT_MANIFEST.as_bytes());
        let policies = [key_policy(GOLDEN_PUBLIC_KEY_PEM)];
        let trust_root =
            TrustRoot::load_trusted_root_json(GOLDEN_TRUSTED_ROOT.as_bytes()).expect("the committed trust root loads");

        let scan = async |predicate_type: Option<PredicateType>| {
            drive_scan_in_mode(
                seed_attestation_sidecar_tag(&subject),
                &subject,
                &policies,
                &trust_root,
                None,
                ScanArity::All,
                VerifyContentMode::Attestation { predicate_type },
            )
            .await
        };

        let found = scan(Some(PredicateType::CycloneDx))
            .await
            .expect("the sidecar carries exactly this predicateType");
        assert_eq!(found.matches.len(), 1, "the accepting half: the type it does carry");

        let narrowed = scan(Some(PredicateType::SpdxJson)).await;
        assert!(
            matches!(narrowed, Err(VerifyErrorKind::AttestationNotFound)),
            "a `.att` document of another predicateType is a narrowing miss, not a match: {narrowed:?}",
        );
    }

    /// **The `.att` gate probes the bounds; it must not stamp them.**
    ///
    /// A run whose bundle loop spends its *last* candidate slot leaves `stop`
    /// unset — nothing was left unlooked-at — and reports the refusal it
    /// actually collected. Probing that bound with the recording form turns
    /// every such run into a truncation error carrying `unexamined == 0`,
    /// masking the actionable verdict, and it fires on the overwhelmingly
    /// common subject that has no `.att` tag at all (this registry has none).
    ///
    /// Both halves over one seeded registry, varying only the budget handed in:
    /// a budget with one slot left reports the candidate's own refusal, a
    /// budget with none left reports the truncation. Either alone is satisfied
    /// by a scan that always answers the same way.
    #[tokio::test]
    async fn a_spent_last_candidate_slot_is_not_reported_as_a_truncation() {
        use crate::oci::client::test_transport::{StubTransportData, referrers_key};

        let subject = crate::oci::Algorithm::Sha256.hash(GOLDEN_SUBJECT_MANIFEST.as_bytes());
        let policies = [key_policy(GOLDEN_PUBLIC_KEY_PEM)];
        let trust_root =
            TrustRoot::load_trusted_root_json(GOLDEN_TRUSTED_ROOT.as_bytes()).expect("the committed trust root loads");
        let caps = VerifyContentMode::Attestation { predicate_type: None }.caps();

        // One bundle-shaped referrer refused from its descriptor alone (a
        // declared size past the per-manifest cap), so the listing is non-empty
        // and exactly one candidate slot is spent. No `.att` tag is seeded.
        let seed = || {
            let image: native::Reference = SCAN_IMAGE.parse().expect("stub reference");
            let mut descriptor = referrer_descriptor(b"over-cap referrer", SIGSTORE_BUNDLE_V03);
            descriptor.size = MAX_REFERRER_MANIFEST_BYTES as i64 + 1;
            let data = StubTransportData::new();
            {
                let mut inner = data.write();
                inner
                    .referrers
                    .entry(referrers_key(&image, &subject))
                    .or_default()
                    .push(descriptor);
                let pinned = image.clone_with_digest(subject.to_string());
                inner.manifests.insert(
                    pinned.to_string(),
                    (GOLDEN_SUBJECT_MANIFEST.as_bytes().to_vec(), subject.to_string()),
                );
            }
            data
        };

        let run = async |already_examined: usize| {
            let mut budget = ScanBudget::new(caps);
            budget.examined = already_examined;
            budget.considered = already_examined;
            drive_scan_with_budget(
                seed(),
                &subject,
                &policies,
                &trust_root,
                None,
                ScanArity::All,
                VerifyContentMode::Attestation { predicate_type: None },
                &mut budget,
            )
            .await
        };

        let last_slot = run(caps.candidates - 1).await;
        assert!(
            matches!(last_slot, Err(VerifyErrorKind::BundleParseFailed)),
            "a scan that looked at every candidate must report the candidate's own verdict: {last_slot:?}",
        );

        let no_slot = run(caps.candidates).await;
        assert!(
            matches!(no_slot, Err(VerifyErrorKind::TooManyAttestations { .. })),
            "a scan that genuinely left a candidate unexamined must still report the truncation: {no_slot:?}",
        );
    }

    /// **The other half of the `.att` gate.** An exhausted candidate budget
    /// shuts the door even when nothing was refused.
    ///
    /// `run_attestations` hands **one** `ScanBudget` to the platform-manifest
    /// pass and the enclosing-index pass in turn, so the second pass can arrive
    /// with every slot already spent and no refusal of its own to show for it.
    /// Drop `budget.stop_reason().is_none()` from the gate and that run reads
    /// the `.att` tag anyway and exits 0 on a scan whose budget was exhausted —
    /// a fail-**open**, not a cosmetic one.
    ///
    /// Its sibling above cannot see this: that registry seeds a refused
    /// candidate, so `refused.is_empty()` closes the door in both halves
    /// whatever the budget conjunct does. Zero refusals is the whole point of
    /// this fixture.
    ///
    /// Paired, because a scan that always answered `NoSignaturesFound` would
    /// satisfy the first half alone: the same registry with a fresh budget must
    /// find the attestation.
    #[tokio::test]
    async fn an_exhausted_budget_shuts_the_att_door_with_nothing_refused() {
        let subject = crate::oci::Algorithm::Sha256.hash(GOLDEN_SUBJECT_MANIFEST.as_bytes());
        let policies = [key_policy(GOLDEN_PUBLIC_KEY_PEM)];
        let trust_root =
            TrustRoot::load_trusted_root_json(GOLDEN_TRUSTED_ROOT.as_bytes()).expect("the committed trust root loads");
        let caps = VerifyContentMode::Attestation { predicate_type: None }.caps();

        let run = async |already_examined: usize| {
            let mut budget = ScanBudget::new(caps);
            budget.examined = already_examined;
            budget.considered = already_examined;
            drive_scan_with_budget(
                seed_attestation_sidecar_tag(&subject),
                &subject,
                &policies,
                &trust_root,
                None,
                ScanArity::All,
                VerifyContentMode::Attestation { predicate_type: None },
                &mut budget,
            )
            .await
        };

        let fresh = run(0)
            .await
            .expect("the accepting half: a fresh budget reads the `.att` tag and verifies it");
        assert_eq!(fresh.matches.len(), 1, "the sidecar carries exactly one attestation");

        let exhausted = run(caps.candidates).await;
        assert!(
            matches!(exhausted, Err(VerifyErrorKind::NoSignaturesFound)),
            "a run whose sibling pass spent every candidate slot must not spend one more on the \
             `.att` door and report success: {exhausted:?}",
        );
    }

    /// **S-009.** One signature reachable through the OCI 1.1 referrer door and
    /// the cosign sidecar-tag door contributes **one** row, not two.
    ///
    /// The premise is asserted first: each door alone finds the signature. Only
    /// then does "both doors, still one" mean the dedup ran, rather than one
    /// door having quietly found nothing.
    #[tokio::test]
    async fn the_same_signature_through_two_doors_is_reported_once() {
        let subject = sidecar_subject();
        let policies = [key_policy(GOLDEN_PUBLIC_KEY_PEM)];
        let trust_root =
            TrustRoot::load_trusted_root_json(GOLDEN_TRUSTED_ROOT.as_bytes()).expect("the committed trust root loads");

        for (referrer, tag, door) in [(true, false, "referrer"), (false, true, "sidecar tag")] {
            let outcome = drive_scan(
                seed_sidecar_through_both_doors(&subject, referrer, tag),
                &subject,
                &policies,
                &trust_root,
                None,
                ScanArity::All,
            )
            .await
            .unwrap_or_else(|error| panic!("the {door} door alone must find the signature: {error:?}"));
            assert_eq!(outcome.matches.len(), 1, "{door} door");
        }

        let both = drive_scan(
            seed_sidecar_through_both_doors(&subject, true, true),
            &subject,
            &policies,
            &trust_root,
            None,
            ScanArity::All,
        )
        .await
        .expect("both doors must still verify");
        assert_eq!(
            both.matches.len(),
            1,
            "one signature found twice is one row of signatures[], got: {:?}",
            both.matches,
        );
        let (result, _) = &both.matches[0];
        assert_eq!(result.signature_format, SignatureFormat::Simplesigning);
        assert_eq!(
            result.rekor_log_index, None,
            "a sidecar carries no verified transparency evidence, so the dedup ran on the material tuple",
        );
    }

    /// D6's dedup key separates two subjects that share signature bytes.
    ///
    /// The scan-level test above cannot show this: one scan has one subject, so
    /// `subject_digest` is a constant there and dropping it from the tuple
    /// changes nothing. Here it is the only field that differs, which is what
    /// makes removing it a red rather than a no-op.
    #[test]
    fn the_fallback_dedup_key_separates_two_subjects() {
        /// One candidate, varied one field at a time — a literal per case would
        /// make the *difference* the thing the reader has to find rather than
        /// the thing the test states.
        fn candidate(
            subject: &[u8],
            signature: &[u8],
            log_index: Option<u64>,
            via: DiscoveryMethod,
        ) -> VerifiedSignature {
            VerifiedSignature {
                result: VerifyResult {
                    subject_digest: crate::oci::Algorithm::Sha256.hash(subject),
                    referrer_digest: crate::oci::Algorithm::Sha256.hash(b"referrer"),
                    key_backend: KeyBackendKind::File,
                    certificate_identity: None,
                    certificate_oidc_issuer: None,
                    signed_at: None,
                    signature_format: SignatureFormat::Simplesigning,
                    discovery_method: via,
                    rekor_log_index: log_index,
                },
                signature: signature.to_vec(),
            }
        }

        let one = candidate(
            b"subject one",
            b"the same signature bytes",
            None,
            DiscoveryMethod::SidecarTag,
        );
        let other_subject = candidate(
            b"subject two",
            b"the same signature bytes",
            None,
            DiscoveryMethod::SidecarTag,
        );
        assert_ne!(one.result.subject_digest, other_subject.result.subject_digest);
        assert_ne!(
            one.dedup_key(),
            other_subject.dedup_key(),
            "two subjects are two signatures however identical the bytes",
        );

        // The other direction, so the key is not merely "always different": the
        // same signature found through the other door keys identically.
        let same_signature_other_door = candidate(
            b"subject one",
            b"the same signature bytes",
            None,
            DiscoveryMethod::ReferrersApi,
        );
        assert_eq!(one.dedup_key(), same_signature_other_door.dedup_key());

        // And the Rekor branch wins when present, so two doors onto one logged
        // signature collapse even where nothing else about them matched.
        let logged = candidate(b"subject one", b"one", Some(7), DiscoveryMethod::SidecarTag);
        let logged_elsewhere = candidate(b"subject two", b"two", Some(7), DiscoveryMethod::ReferrersApi);
        assert_eq!(logged.dedup_key(), logged_elsewhere.dedup_key());
    }

    /// **S-008.** With only a simplesigning shape present and no flag, D9's
    /// fallback fires and the signature verifies; `--signature-format bundle`
    /// pins the shape away and the same subject answers "no signatures found".
    #[tokio::test]
    async fn the_simplesigning_fallback_fires_only_when_no_pin_excludes_it() {
        let subject = sidecar_subject();
        let policies = [key_policy(GOLDEN_PUBLIC_KEY_PEM)];
        let trust_root =
            TrustRoot::load_trusted_root_json(GOLDEN_TRUSTED_ROOT.as_bytes()).expect("the committed trust root loads");

        let unpinned = drive_scan(
            seed_sidecar_through_both_doors(&subject, false, true),
            &subject,
            &policies,
            &trust_root,
            None,
            ScanArity::FirstMatch,
        )
        .await
        .expect("no bundle verified, so the sidecar fallback must fire");
        assert_eq!(unpinned.matches.len(), 1);
        assert_eq!(unpinned.matches[0].0.signature_format, SignatureFormat::Simplesigning);
        assert_eq!(unpinned.matches[0].0.discovery_method, DiscoveryMethod::SidecarTag);

        let pinned_to_bundle = drive_scan(
            seed_sidecar_through_both_doors(&subject, true, true),
            &subject,
            &policies,
            &trust_root,
            Some(SignatureFormat::Bundle),
            ScanArity::FirstMatch,
        )
        .await;
        assert!(
            matches!(pinned_to_bundle, Err(VerifyErrorKind::NoSignaturesFound)),
            "a bundle pin must not fall back to a sidecar it was told to ignore: {pinned_to_bundle:?}",
        );
    }

    /// **Truncation is not a third door onto the sidecar.** The fallback gate
    /// asks `matches.is_empty() && refused.is_empty()`, and a bundle loop that
    /// `break`s on a spent budget satisfies both — it records neither a match
    /// nor a refusal. What keeps that from promoting the weaker shape is not
    /// the gate but [`VerifyPipeline::scan_simplesigning`]'s own bounds: both
    /// its doors are gated on the *same* [`ScanBudget::may_examine`], whose
    /// three counters only ever grow, so once it has returned `false` it
    /// returns `false` for the rest of the scan and neither door opens.
    ///
    /// Asserted here because that guard is invisible at the gate it protects: a
    /// reader of `scan` sees a condition that a truncated scan passes, and
    /// nothing at that line says why the call beyond it finds nothing. Delete
    /// either `may_examine` inside `scan_simplesigning` and this reds while
    /// every other scan test stays green.
    ///
    /// The control comes first and is the same subject through the same seed:
    /// without it a broken sidecar seed would satisfy the spent-budget half
    /// vacuously.
    #[tokio::test]
    async fn a_truncated_bundle_scan_does_not_promote_the_sidecar() {
        let subject = sidecar_subject();
        let policies = [key_policy(GOLDEN_PUBLIC_KEY_PEM)];
        let trust_root =
            TrustRoot::load_trusted_root_json(GOLDEN_TRUSTED_ROOT.as_bytes()).expect("the committed trust root loads");
        let caps = VerifyContentMode::Signature.caps();

        let mut fresh = ScanBudget::new(caps);
        let control = drive_scan_with_budget(
            seed_sidecar_through_both_doors(&subject, false, true),
            &subject,
            &policies,
            &trust_root,
            None,
            ScanArity::FirstMatch,
            VerifyContentMode::Signature,
            &mut fresh,
        )
        .await
        .expect("with bounds to spare the sidecar door opens and the signature verifies");
        assert_eq!(control.matches.len(), 1);
        assert_eq!(control.matches[0].0.signature_format, SignatureFormat::Simplesigning);
        assert_eq!(
            fresh.stop, None,
            "the control must not itself be truncated, or it proves nothing about the other half"
        );

        // The same registry, reached with the cross-candidate byte budget
        // already spent — what the bundle loop leaves behind when it breaks,
        // and what the second pass of `scan_with_index_fallback` inherits.
        let mut spent = ScanBudget::new(caps);
        spent.charge(caps.total_bytes);
        let truncated = drive_scan_with_budget(
            seed_sidecar_through_both_doors(&subject, false, true),
            &subject,
            &policies,
            &trust_root,
            None,
            ScanArity::FirstMatch,
            VerifyContentMode::Signature,
            &mut spent,
        )
        .await;
        assert!(
            truncated.is_err(),
            "a scan with no bounds left must not verify the sidecar it could not afford to examine: {truncated:?}",
        );
        assert_eq!(
            spent.stop,
            Some(ScanStop::ByteBudget),
            "the refusal must come from the spent budget, not from a seed that stopped working",
        );
    }

    /// **C-007's pin, the other direction.** `--signature-format simplesigning`
    /// against a subject carrying only a bundle answers 79 — the bundle is
    /// never discovered, so it can never be silently verified.
    ///
    /// The control is the same seeded registry with no pin, which *does* verify
    /// the bundle: without it a broken seed would satisfy the pinned half
    /// vacuously.
    #[tokio::test]
    async fn a_simplesigning_pin_never_verifies_a_bundle() {
        use crate::oci::client::test_transport::{StubTransportData, referrers_key};

        let subject_bytes = GOLDEN_SUBJECT_MANIFEST.as_bytes();
        let subject = crate::oci::Algorithm::Sha256.hash(subject_bytes);
        let blob = GOLDEN_KEYLESS_BUNDLE.as_bytes().to_vec();
        let blob_digest = crate::oci::Algorithm::Sha256.hash(&blob);
        let annotated = golden_referrer_field("/annotations/dev.sigstore.bundle.predicateType");
        let (descriptor, referrer_bytes) = referrer_with(&subject, &blob_digest, blob.len() as i64, Some(&annotated));
        let image: native::Reference = SCAN_IMAGE.parse().expect("stub reference");

        let seed = || {
            let data = StubTransportData::new();
            {
                let mut inner = data.write();
                inner.blobs.insert(blob_digest.to_string(), blob.clone());
                inner.manifests.insert(
                    image.clone_with_digest(subject.to_string()).to_string(),
                    (subject_bytes.to_vec(), subject.to_string()),
                );
                inner.manifests.insert(
                    image.clone_with_digest(descriptor.digest.clone()).to_string(),
                    (referrer_bytes.clone(), descriptor.digest.clone()),
                );
                inner
                    .referrers
                    .entry(referrers_key(&image, &subject))
                    .or_default()
                    .push(descriptor.clone());
            }
            data
        };
        let policies = [golden_keyless_policy()];
        let trust_root =
            TrustRoot::load_trusted_root_json(GOLDEN_TRUSTED_ROOT.as_bytes()).expect("the committed trust root loads");

        let unpinned = drive_scan(seed(), &subject, &policies, &trust_root, None, ScanArity::FirstMatch)
            .await
            .expect("the seeded bundle must verify with no pin");
        assert_eq!(unpinned.matches.len(), 1);
        assert_eq!(unpinned.matches[0].0.signature_format, SignatureFormat::Bundle);
        assert_eq!(unpinned.matches[0].0.discovery_method, DiscoveryMethod::ReferrersApi);
        assert!(
            unpinned.matches[0].0.rekor_log_index.is_some(),
            "a keyless bundle's log index is reported only after its SET and proof passed",
        );

        let pinned = drive_scan(
            seed(),
            &subject,
            &policies,
            &trust_root,
            Some(SignatureFormat::Simplesigning),
            ScanArity::FirstMatch,
        )
        .await;
        assert!(
            matches!(pinned, Err(VerifyErrorKind::NoSignaturesFound)),
            "a simplesigning pin must not discover a bundle: {pinned:?}",
        );
    }

    /// Q3. Widening the arity widens the **report**, never the verdict.
    ///
    /// Two genuinely different signatures over one subject — cosign's keyless
    /// bundle and its key-mode bundle, both captured over the same golden
    /// artifact — under a policy set that trusts both. `FirstMatch` reports one,
    /// `All` reports two, and the head is the same candidate in each.
    #[tokio::test]
    async fn widening_the_arity_widens_the_report_and_not_the_verdict() {
        use crate::oci::client::test_transport::{StubTransportData, referrers_key};

        let subject_bytes = GOLDEN_SUBJECT_MANIFEST.as_bytes();
        let subject = crate::oci::Algorithm::Sha256.hash(subject_bytes);
        let image: native::Reference = SCAN_IMAGE.parse().expect("stub reference");

        let bundles = [
            (
                GOLDEN_KEYLESS_BUNDLE,
                golden_referrer_field("/annotations/dev.sigstore.bundle.predicateType"),
            ),
            (
                GOLDEN_KEY_BUNDLE,
                golden_key_referrer_field("/annotations/dev.sigstore.bundle.predicateType"),
            ),
        ];
        let seed = || {
            let data = StubTransportData::new();
            {
                let mut inner = data.write();
                inner.manifests.insert(
                    image.clone_with_digest(subject.to_string()).to_string(),
                    (subject_bytes.to_vec(), subject.to_string()),
                );
                for (bundle_json, annotated) in &bundles {
                    let blob = bundle_json.as_bytes().to_vec();
                    let blob_digest = crate::oci::Algorithm::Sha256.hash(&blob);
                    let (descriptor, referrer_bytes) =
                        referrer_with(&subject, &blob_digest, blob.len() as i64, Some(annotated));
                    inner.blobs.insert(blob_digest.to_string(), blob);
                    inner.manifests.insert(
                        image.clone_with_digest(descriptor.digest.clone()).to_string(),
                        (referrer_bytes, descriptor.digest.clone()),
                    );
                    inner
                        .referrers
                        .entry(referrers_key(&image, &subject))
                        .or_default()
                        .push(descriptor);
                }
            }
            data
        };
        let policies = [golden_keyless_policy(), key_policy(GOLDEN_PUBLIC_KEY_PEM)];
        let trust_root =
            TrustRoot::load_trusted_root_json(GOLDEN_TRUSTED_ROOT.as_bytes()).expect("the committed trust root loads");

        let first = drive_scan(seed(), &subject, &policies, &trust_root, None, ScanArity::FirstMatch)
            .await
            .expect("the first candidate verifies");
        let all = drive_scan(seed(), &subject, &policies, &trust_root, None, ScanArity::All)
            .await
            .expect("both candidates verify");

        assert_eq!(first.matches.len(), 1, "first-match reports exactly the verdict");
        assert_eq!(
            all.matches.len(),
            2,
            "report_all must list both signatures, got: {:?}",
            all.matches,
        );
        assert_eq!(
            first.matches[0].0.referrer_digest, all.matches[0].0.referrer_digest,
            "the verdict is the same candidate under either arity",
        );
        assert_ne!(
            all.matches[0].0.rekor_log_index, all.matches[1].0.rekor_log_index,
            "the two rows must be two distinct log entries, or the dedup collapsed them",
        );
    }

    /// Part 3.1. The CT-log-key gate is a **keyless** requirement.
    ///
    /// `cosign verify --key cosign.pub` needs no trust root at all, so refusing
    /// a key-mode verify for want of a CT log key is a parity gap: it makes an
    /// acceptance-level key verify impossible without `--sigstore-trusted-root`.
    ///
    /// Both halves run over the **same** seeded bundle referrer and the same
    /// empty trust root; only the policy set differs. That is what makes this a
    /// test of the narrowing rather than of two unrelated setups — and the
    /// key-mode half verifies for real (`key_backend == file`), so the gate is
    /// not merely skipped, the whole path completes without trust material.
    ///
    /// The bundle is the golden key capture with its `tlogEntries` stripped:
    /// D10 makes that legal, and it is the one shape that needs no Rekor key
    /// either, so nothing but the CT gate can be what refuses it.
    #[tokio::test]
    async fn the_ct_log_key_gate_applies_to_the_keyless_path_only() {
        use crate::oci::client::test_transport::{StubTransportData, referrers_key};

        let subject_bytes = GOLDEN_SUBJECT_MANIFEST.as_bytes();
        let subject = crate::oci::Algorithm::Sha256.hash(subject_bytes);
        let bundle_json = without_tlog_entries(GOLDEN_KEY_BUNDLE);
        let annotated = golden_key_referrer_field("/annotations/dev.sigstore.bundle.predicateType");
        let image: native::Reference = SCAN_IMAGE.parse().expect("stub reference");

        let seed = || {
            let blob = bundle_json.as_bytes().to_vec();
            let blob_digest = crate::oci::Algorithm::Sha256.hash(&blob);
            let (descriptor, referrer_bytes) =
                referrer_with(&subject, &blob_digest, blob.len() as i64, Some(&annotated));
            let data = StubTransportData::new();
            {
                let mut inner = data.write();
                inner.blobs.insert(blob_digest.to_string(), blob);
                inner.manifests.insert(
                    image.clone_with_digest(subject.to_string()).to_string(),
                    (subject_bytes.to_vec(), subject.to_string()),
                );
                inner.manifests.insert(
                    image.clone_with_digest(descriptor.digest.clone()).to_string(),
                    (referrer_bytes, descriptor.digest.clone()),
                );
                inner
                    .referrers
                    .entry(referrers_key(&image, &subject))
                    .or_default()
                    .push(descriptor);
            }
            data
        };

        let empty_root = TrustRoot::default();
        assert!(
            empty_root.ctfe_key_map().is_empty(),
            "the premise: this root carries no CT log key",
        );

        let under_key = drive_scan(
            seed(),
            &subject,
            &[key_policy(GOLDEN_PUBLIC_KEY_PEM)],
            &empty_root,
            None,
            ScanArity::FirstMatch,
        )
        .await
        .expect("a key-mode verify needs no CT log key and no trust root");
        assert_eq!(under_key.matches.len(), 1);
        assert_eq!(under_key.matches[0].0.key_backend, KeyBackendKind::File);
        assert_eq!(under_key.matches[0].0.certificate_identity, None);

        let under_keyless = drive_scan(
            seed(),
            &subject,
            &[golden_keyless_policy()],
            &empty_root,
            None,
            ScanArity::FirstMatch,
        )
        .await;
        assert!(
            matches!(
                under_keyless,
                Err(VerifyErrorKind::TrustRootLoad(TrustRootLoadReason::NoCtLogKey))
            ),
            "a policy set that admits keyless must still get the remedy up front: {under_keyless:?}",
        );
    }

    // ── D-6: C-008, the index-membership gate ──────────────────────────────

    /// The platform manifest a D-6 fixture pins. Any digest that is not
    /// [`sidecar_subject`] will do — what matters is that the sidecar's payload
    /// binds the *index*, not this.
    fn membership_child_digest() -> Digest {
        Digest::Sha256("1".repeat(64))
    }

    /// An index double that answers one resolution, so a test can say exactly
    /// what the reference resolved to: an image index and its children, a bare
    /// image manifest, or nothing at all.
    #[derive(Clone)]
    struct ResolvingIndex {
        physical: Identifier,
        resolved: Option<(Digest, crate::oci::Manifest)>,
    }

    #[async_trait::async_trait]
    impl crate::oci::index::IndexImpl for ResolvingIndex {
        async fn list_repositories(&self, _: &str) -> crate::Result<Vec<String>> {
            Ok(Vec::new())
        }

        async fn list_tags(&self, _: &Identifier) -> crate::Result<Option<Vec<String>>> {
            Ok(None)
        }

        async fn fetch_manifest(
            &self,
            _: &Identifier,
            _: IndexOperation,
        ) -> crate::Result<Option<(Digest, crate::oci::Manifest)>> {
            Ok(self.resolved.clone())
        }

        async fn fetch_manifest_digest(&self, _: &Identifier, _: IndexOperation) -> crate::Result<Option<Digest>> {
            Ok(self.resolved.as_ref().map(|(digest, _)| digest.clone()))
        }

        async fn fetch_blob(&self, _: &crate::oci::PinnedIdentifier) -> crate::Result<Option<Vec<u8>>> {
            Ok(None)
        }

        async fn physical_reference(&self, _: &Identifier) -> crate::Result<Option<Identifier>> {
            Ok(Some(self.physical.clone()))
        }

        fn box_clone(&self) -> Box<dyn crate::oci::index::IndexImpl> {
            Box::new(self.clone())
        }
    }

    /// An image index listing exactly `children`, so a test states the
    /// `manifests[]` the membership test reads.
    fn image_index_of(children: &[(&str, &Digest)]) -> crate::oci::Manifest {
        crate::oci::Manifest::ImageIndex(crate::oci::native::ImageIndex {
            schema_version: 2,
            media_type: Some(crate::oci::OCI_IMAGE_INDEX_MEDIA_TYPE.to_string()),
            manifests: children
                .iter()
                .map(|(platform, digest)| crate::oci::native::ImageIndexEntry {
                    media_type: crate::oci::OCI_IMAGE_MEDIA_TYPE.to_string(),
                    digest: digest.to_string(),
                    size: 2,
                    platform: Some(crate::oci::native::Platform::from(
                        &platform.parse::<Platform>().expect("test platform parses"),
                    )),
                    artifact_type: None,
                    annotations: None,
                })
                .collect(),
            artifact_type: None,
            annotations: None,
        })
    }

    /// Drive the whole signature pipeline — [`VerifyPipeline::run_inner`], not
    /// [`VerifyPipeline::scan`] — so the C-008 fall-through, which lives above
    /// the scan, is actually exercised.
    async fn drive_run(
        data: crate::oci::client::test_transport::StubTransportData,
        resolved: Option<(Digest, crate::oci::Manifest)>,
        platform: Option<&Platform>,
    ) -> Result<Vec<VerifyResult>, VerifyErrorKind> {
        use crate::oci::client::test_transport::StubTransport;

        let client = Client::with_transport(Box::new(StubTransport::new(data)));
        let identifier = verify_id();
        let index = Index::from_impl(ResolvingIndex {
            physical: identifier.clone(),
            resolved,
        });
        let trust_root =
            TrustRoot::load_trusted_root_json(GOLDEN_TRUSTED_ROOT.as_bytes()).expect("the committed trust root loads");
        let policies = [key_policy(GOLDEN_PUBLIC_KEY_PEM)];
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = StateStore::new(temp.path());
        VerifyPipeline::run_inner(
            &client,
            VerifyContext {
                identifier: &identifier,
                platform,
                policies: &policies,
                no_cache: true,
                index: &index,
                trust_root: &trust_root,
                rekor_url: &rekor_url,
                state: &state,
                offline: true,
                content: VerifyContentMode::Signature,
                verification: VerificationMode::Demand,
                signature_format: None,
                allow_unlogged_signature: false,
                report_all: false,
            },
        )
        .await
    }

    /// The sidecar seeded on `subject`, in the repository `verify_id()` names —
    /// the one the D-6 runs address, unlike the D-5 merge fixtures which use
    /// `SCAN_IMAGE`.
    fn seed_sidecar_on(subject: &Digest) -> crate::oci::client::test_transport::StubTransportData {
        use crate::oci::client::test_transport::{StubTransportData, referrers_key};

        let client = Client::with_transport(Box::new(crate::oci::client::test_transport::StubTransport::new(
            StubTransportData::new(),
        )));
        let image = client.transport_reference(&verify_id());
        let manifest_bytes = SIDECAR_MANIFEST.as_bytes().to_vec();
        let descriptor = referrer_descriptor(&manifest_bytes, COSIGN_SIG_ARTIFACT_TYPE);
        let layer = sidecar_layer();

        let data = StubTransportData::new();
        {
            let mut inner = data.write();
            inner.blobs.insert(layer.digest.clone(), SIDECAR_PAYLOAD.to_vec());
            inner
                .referrers
                .entry(referrers_key(&image, subject))
                .or_default()
                .push(descriptor.clone());
            let referrer_ref = image.clone_with_digest(descriptor.digest.clone());
            inner
                .manifests
                .insert(referrer_ref.to_string(), (manifest_bytes, descriptor.digest.clone()));
        }
        data
    }

    /// **S-007.** A signature that sits on the **enclosing index** verifies a
    /// pinned platform manifest, because the index lists that manifest.
    ///
    /// This is the shape `cosign sign <tag>` produces against a multi-platform
    /// tag: cosign resolves the tag to the *index* digest and signs there, while
    /// OCX installs a *platform* manifest. Without the membership proof, every
    /// cosign-signed multi-platform artifact reads as unsigned.
    ///
    /// The two halves are asserted from **one** seeded registry, so the only
    /// difference between them is whether the resolution proved membership:
    ///
    /// * resolved to the index that lists the child → the index's signature counts;
    /// * resolved straight to the child, no index in hand → it does not, and the
    ///   subject is reported unsigned rather than assumed to be a member.
    #[tokio::test]
    async fn an_index_signature_verifies_the_platform_manifest_the_index_lists() {
        let index_digest = sidecar_subject();
        let child = membership_child_digest();
        let requested: Platform = "linux/amd64".parse().expect("test platform parses");

        // Membership proved: the reference resolved to the index, `--platform`
        // narrowed into the child, and the child is one the index lists.
        let verified = drive_run(
            seed_sidecar_on(&index_digest),
            Some((index_digest.clone(), image_index_of(&[("linux/amd64", &child)]))),
            Some(&requested),
        )
        .await
        .expect("an index signature must cover the platform manifest the index lists");
        assert_eq!(verified.len(), 1, "one signature, found on the index");
        // The signature is over the *index* digest and the report says so —
        // reporting the child's digest would claim the index's signature was
        // made over bytes it never covered.
        assert_eq!(
            verified[0].subject_digest, index_digest,
            "the verified subject is the index the signature was made over",
        );

        // Membership unprovable: the same registry, the same signature, but the
        // reference resolved straight to the child. There is no index in hand,
        // so the index's signature is not considered at all.
        let refused = drive_run(
            seed_sidecar_on(&index_digest),
            Some((
                child.clone(),
                crate::oci::Manifest::Image(crate::oci::ImageManifest::default()),
            )),
            None,
        )
        .await
        .expect_err("a bare platform digest must never borrow an index's signature");
        assert!(
            matches!(refused, VerifyErrorKind::NoSignaturesFound),
            "expected the unsigned verdict, got: {refused:?}",
        );
    }

    /// An index that cannot be fetched — `OCX_OFFLINE`, or simply absent from
    /// the cache — refuses the run outright. It never falls through to a
    /// signature the index might have carried, because it never learns the
    /// index digest to ask about.
    #[tokio::test]
    async fn an_unfetchable_index_resolves_to_nothing_rather_than_assuming_membership() {
        let outcome = drive_run(
            seed_sidecar_on(&sidecar_subject()),
            None,
            Some(&"linux/amd64".parse::<Platform>().expect("test platform parses")),
        )
        .await
        .expect_err("an unresolvable reference cannot verify");
        assert!(
            matches!(outcome, VerifyErrorKind::TargetNotFound { .. }),
            "expected the unresolved verdict, got: {outcome:?}",
        );
    }

    /// Drive [`VerifyPipeline::scan_with_index_fallback`] over a `ScanTarget`
    /// the test states outright, so the **gate's wiring** is exercised on a
    /// state `resolve_target` cannot currently produce.
    ///
    /// `resolve_target` selects the subject out of the very `manifests[]` it
    /// reports, so today `enclosing_index.is_some()` implies membership. That
    /// makes the containment test defence in depth — and a caller that read
    /// `enclosing_index` directly instead of asking the gate would pass every
    /// end-to-end fixture. This is the seam where that goes red.
    async fn drive_index_fallback(
        data: crate::oci::client::test_transport::StubTransportData,
        target: ScanTarget,
    ) -> Result<ScanOutcome, VerifyErrorKind> {
        use crate::oci::client::test_transport::StubTransport;

        let client = Client::with_transport(Box::new(StubTransport::new(data)));
        let identifier = verify_id();
        let index = Index::from_impl(ResolvingIndex {
            physical: identifier.clone(),
            resolved: None,
        });
        let trust_root =
            TrustRoot::load_trusted_root_json(GOLDEN_TRUSTED_ROOT.as_bytes()).expect("the committed trust root loads");
        let policies = [key_policy(GOLDEN_PUBLIC_KEY_PEM)];
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = StateStore::new(temp.path());
        let ctx = VerifyContext {
            identifier: &identifier,
            platform: None,
            policies: &policies,
            no_cache: true,
            index: &index,
            trust_root: &trust_root,
            rekor_url: &rekor_url,
            state: &state,
            offline: true,
            content: VerifyContentMode::Signature,
            verification: VerificationMode::Demand,
            signature_format: None,
            allow_unlogged_signature: false,
            report_all: false,
        };
        let mut budget = ScanBudget::new(ctx.content.caps());
        VerifyPipeline::scan_with_index_fallback(&client, &ctx, &target, ScanArity::FirstMatch, &mut budget).await
    }

    /// **R7.** The fall-through consults the membership gate, not the raw
    /// `enclosing_index`.
    ///
    /// Both halves run against **one** seeded registry carrying **one**
    /// signature, on the index digest, and differ only in whether the index
    /// lists the subject. A fall-through that reached for `enclosing_index`
    /// directly — the shape of the hole this guards — would verify both.
    #[tokio::test]
    async fn the_index_fallback_refuses_a_subject_the_index_does_not_list() {
        let image = Client::with_transport(Box::new(crate::oci::client::test_transport::StubTransport::new(
            crate::oci::client::test_transport::StubTransportData::new(),
        )))
        .transport_reference(&verify_id());
        let index_digest = sidecar_subject();
        let child = membership_child_digest();

        // Member: the index lists the child, so the index's signature counts.
        let verified = drive_index_fallback(
            seed_sidecar_on(&index_digest),
            ScanTarget {
                image: image.clone(),
                subject_digest: child.clone(),
                enclosing_index: Some(index_digest.clone()),
                index_members: vec![child.clone()],
            },
        )
        .await
        .expect("a listed child is covered by the index's signature");
        assert_eq!(verified.matches.len(), 1, "the index's signature counts for a member");

        // Not a member: same index digest, same signature — and refused,
        // because nothing proved this subject is one of its children.
        let refused = drive_index_fallback(
            seed_sidecar_on(&index_digest),
            ScanTarget {
                image,
                subject_digest: child.clone(),
                enclosing_index: Some(index_digest),
                index_members: vec![Digest::Sha256("f".repeat(64))],
            },
        )
        .await
        .expect_err("an unlisted subject must not borrow the index's signature");
        assert!(
            matches!(refused, VerifyErrorKind::NoSignaturesFound),
            "expected the unsigned verdict for a non-member, got: {refused:?}",
        );
    }

    /// **C-010's error half.** `--platform` given against a reference that
    /// resolved to a single manifest is refused, with a slug of its own.
    ///
    /// Not folded into `target_not_found`: "this package ships no such
    /// platform" sends an operator looking for a missing build, where the truth
    /// is "there are no platforms here to choose from — drop the flag".
    #[tokio::test]
    async fn a_platform_request_against_a_bare_manifest_is_refused() {
        let outcome = drive_run(
            seed_sidecar_on(&sidecar_subject()),
            Some((
                membership_child_digest(),
                crate::oci::Manifest::Image(crate::oci::ImageManifest::default()),
            )),
            Some(&"linux/amd64".parse::<Platform>().expect("test platform parses")),
        )
        .await
        .expect_err("a bare manifest cannot be narrowed");
        assert!(
            matches!(outcome, VerifyErrorKind::TargetNotAnIndex { .. }),
            "expected the not-an-index refusal, got: {outcome:?}",
        );
    }

    /// **C-008's containment test**, in isolation and in both directions.
    ///
    /// `resolve_sign_target` *selects* a child and reports the index it came
    /// from; its own contract says it makes no validity decision. So the trust
    /// gate may not infer "the subject is a member" from "the selector picked
    /// it" — it re-derives membership from the index's own `manifests[]`, and
    /// that re-derivation is what these rows pin. Two independent halves, each
    /// with a row that goes red on its own if it is dropped: the enclosing
    /// index must be present, **and** the subject must appear among the members.
    #[test]
    fn the_index_signature_counts_only_for_a_manifest_the_index_lists() {
        let image: native::Reference = SCAN_IMAGE.parse().expect("stub reference");
        let subject = Digest::Sha256("a".repeat(64));
        let sibling = Digest::Sha256("b".repeat(64));
        let enclosing = Digest::Sha256("c".repeat(64));

        let target = |enclosing_index: Option<Digest>, index_members: Vec<Digest>| ScanTarget {
            image: image.clone(),
            subject_digest: subject.clone(),
            enclosing_index,
            index_members,
        };

        /// One membership row: what it is, the enclosing index the resolution
        /// reported, the digests that index lists, and the answer owed.
        type MembershipRow = (&'static str, Option<Digest>, Vec<Digest>, Option<Digest>);

        let rows: [MembershipRow; 5] = [
            (
                "no enclosing index (bare platform digest, or unfetchable) — not considered",
                None,
                Vec::new(),
                None,
            ),
            (
                "no enclosing index, even when some index lists the subject",
                None,
                vec![subject.clone()],
                None,
            ),
            (
                "enclosing index lists the subject — the index's signature counts",
                Some(enclosing.clone()),
                vec![sibling.clone(), subject.clone()],
                Some(enclosing.clone()),
            ),
            (
                "enclosing index does NOT list the subject — refused, not assumed",
                Some(enclosing.clone()),
                vec![sibling.clone()],
                None,
            ),
            (
                "enclosing index lists nothing at all — refused",
                Some(enclosing.clone()),
                Vec::new(),
                None,
            ),
        ];

        for (label, enclosing_index, index_members, expected) in rows {
            let target = target(enclosing_index, index_members);
            assert_eq!(target.index_signature_subject().cloned(), expected, "row '{label}'",);
        }
    }
}
