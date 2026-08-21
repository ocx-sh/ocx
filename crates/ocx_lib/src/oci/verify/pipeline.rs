// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Verify pipeline — full keyless Sigstore verification state machine.
//!
//! Per
//! [`adr_oci_referrers_signing_v1.md`](../../../../../.claude/artifacts/adr_oci_referrers_signing_v1.md)
//! S1-H: resolve target → list referrers (capability cache) → fetch the subject
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

use super::dsse::{self, VerifiedAttestation, VerifiedEnvelope};
use super::error::{TrustRootLoadReason, VerifyError, VerifyErrorKind};
use super::identity::{matching_policies, oidc_issuer, parse_certificate, subject_identity};
use super::tlog;
use super::trust_cache::TrustRootCache;
use super::trust_root::TrustRoot;
use crate::file_structure::StateStore;
use crate::oci::attest::predicate::{PredicateType, sbom_predicate_type_uri};
use crate::oci::attest::{MAX_ATTESTATION_CANDIDATES, MAX_ATTESTATION_ENVELOPE_BYTES, MAX_TOTAL_ATTESTATION_BYTES};
use crate::oci::client::error::ClientError;
use crate::oci::client::{Client, OciTransport};
use crate::oci::index::{Index, IndexOperation, SelectResult};
use crate::oci::referrer::capability::{ReferrersApiCapability, ReferrersSupport};
use crate::oci::referrer::media_types::{
    ANNOTATION_BUNDLE_CONTENT, ANNOTATION_BUNDLE_PREDICATE_TYPE, BUNDLE_CONTENT_DSSE, BUNDLE_CONTENT_MESSAGE_SIGNATURE,
    SIGSTORE_BUNDLE_V03,
};
use crate::oci::sign::bundle::{MAX_BUNDLE_SIZE_BYTES, parse_bundle};
use crate::oci::{Digest, Identifier, Platform, native};
use sigstore_protobuf_specs::dev::sigstore::bundle::v1::{Bundle, bundle, verification_material};
use sigstore_protobuf_specs::dev::sigstore::rekor::v1::InclusionProof as ProtoInclusionProof;

const ACCEPTED_MANIFEST_TYPES: &[&str] = &[
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
const MAX_REFERRER_MANIFEST_BYTES: u64 = 256 * 1024;

/// Maximum number of signature referrers examined during an ANY-of verify.
///
/// Bounds the work a hostile registry can force by listing many candidate
/// referrers; combined with the per-item size caps this bounds total download.
const MAX_SIGNATURE_CANDIDATES: usize = 8;

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
    /// Platform selector for multi-platform manifests.
    pub platform: &'a Platform,
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
}

/// Result emitted by a successful verify pipeline run.
#[derive(Debug)]
pub struct VerifyResult {
    /// Digest of the subject manifest that was verified.
    pub subject_digest: Digest,
    /// Digest of the referrer manifest (the bundle referrer).
    pub referrer_digest: Digest,
    /// Cert SAN that signed the subject.
    pub certificate_identity: String,
    /// Cert OIDC issuer URL.
    pub certificate_oidc_issuer: String,
    /// Rekor integrated time (UTC epoch seconds) of the signature entry.
    pub signed_at: u64,
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
    pub async fn run(client: &Client, ctx: VerifyContext<'_>) -> Result<VerifyResult, VerifyError> {
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

        if ctx.verification == VerificationMode::Permissive {
            // One pass, one budget, and no signed pass at all: with nothing
            // being verified, a bundle referrer is read for its payload exactly
            // as a raw attachment is read for its bytes, and both list as
            // unverified. Reading them in one digest-ordered pass is what makes
            // the two kinds unable to starve each other — there is no second
            // allowance for volume of one kind to spend on behalf of the other.
            let (unverified, refused) = Self::scan_unverified(client, &ctx, &target, &mut budget).await?;
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
            });
        }

        // Demand. Raw attachments are refused wholesale and *without a fetch*,
        // so untrusted volume cannot spend one byte or one candidate slot of
        // the budget the signed pass needs — the starvation question is closed
        // structurally here rather than by rationing a shared allowance.
        let unsigned_refused = Self::refuse_unsigned(client, &ctx, &target).await?;
        let signed = match Self::scan(client, &ctx, &target, ScanArity::All, &mut budget).await {
            Ok(outcome) => outcome,
            // The scan found nothing signed. If unsigned attachments were
            // refused on the way, that refusal is the actionable answer — "this
            // SBOM is attached without a signature and you demanded one" tells
            // an operator what to do, where "none found" states something
            // false about the subject.
            Err(kind @ (VerifyErrorKind::AttestationNotFound | VerifyErrorKind::NoSignaturesFound)) => {
                return Err(best_failure(unsigned_refused).unwrap_or(kind));
            }
            Err(kind) => return Err(kind),
        };

        let matches = signed
            .matches
            .into_iter()
            .map(|(verify, attestation)| {
                // `verify_one_referrer` returns `Some` for every candidate it
                // verified in attestation mode, so `None` here would mean the
                // mode and the outcome had drifted apart. Fail closed rather
                // than report a match with nothing in it.
                attestation
                    .map(|attestation| AttestationMatch { verify, attestation })
                    .ok_or(VerifyErrorKind::AttestationNotFound)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut refused = signed.refused;
        refused.extend(unsigned_refused);
        Ok(AttestationScan {
            matches,
            // Nothing unverified is ever listed under `Demand`: an unsigned
            // attachment was refused above, and every signed match is in
            // `matches`.
            unverified: Vec::new(),
            refused,
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
        let ScanTarget { image, subject_digest } = target;
        Self::ensure_referrers_supported(transport, ctx, image, subject_digest).await?;

        // One unfiltered listing rather than one request per artifact type. The
        // client-side filter below is the real one either way: the OCI spec
        // permits a registry to ignore the server-side `artifactType`
        // parameter, so a filtered listing would still have to be re-filtered
        // here — at several times the requests.
        let listed = transport
            .list_referrers(image, subject_digest, None)
            .await
            .map_err(map_client_error)?;
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
                if sbom_predicate_type_uri(artifact_type).is_some() {
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
                Ok(Some(sbom)) => found.push(sbom),
                // A `--type` narrowing miss: this candidate is fine, it simply
                // is not the document that was asked for. It spent a slot and
                // records no failure, exactly as the signed scan's
                // `TypeNarrowed` does (S-017).
                Ok(None) => {}
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
        Ok((found, refused))
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
    /// be tightened: `pull_bundle_blob_capped` drops the buffer on the error
    /// path, and a registry declaring one byte while streaming the cap would
    /// otherwise be charged one byte.
    ///
    /// `Ok(None)` is a `--type` narrowing miss, not a failure — see the caller.
    async fn read_unverified_referrer(
        transport: &dyn OciTransport,
        ctx: &VerifyContext<'_>,
        budget: &mut ScanBudget,
        target: &ScanTarget,
        descriptor: &crate::oci::Descriptor,
        payload: UnverifiedPayload,
    ) -> Result<Option<UnverifiedSbom>, VerifyErrorKind> {
        let caps = budget.caps;
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
        let layer = manifest.layers.first().ok_or(VerifyErrorKind::NoUsableBundle)?;
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
        let bytes = match pull_bundle_blob_capped(transport, &target.image, &blob_digest, caps.bundle_bytes).await {
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
    ) -> Result<Vec<RefusedCandidate>, VerifyErrorKind> {
        let VerifyContentMode::Attestation { .. } = &ctx.content else {
            return Ok(Vec::new());
        };
        let transport = client.transport();
        let ScanTarget { image, subject_digest } = target;
        Self::ensure_referrers_supported(transport, ctx, image, subject_digest).await?;
        let listed = transport
            .list_referrers(image, subject_digest, None)
            .await
            .map_err(map_client_error)?;

        let mut digests: Vec<String> = listed
            .into_iter()
            .filter(|descriptor| {
                descriptor
                    .artifact_type
                    .as_deref()
                    .and_then(sbom_predicate_type_uri)
                    .is_some()
            })
            .map(|descriptor| descriptor.digest)
            .collect();
        // Digest order, for the reason `order_candidates` sorts: a total order
        // the registry does not choose, so the report is reproducible.
        digests.sort();
        Ok(digests
            .into_iter()
            .take(ctx.content.caps().candidates)
            .map(|referrer_digest| RefusedCandidate {
                referrer_digest,
                reason: VerifyErrorKind::UnsignedRejectedByPolicy,
            })
            .collect())
    }

    async fn run_inner(client: &Client, ctx: VerifyContext<'_>) -> Result<VerifyResult, VerifyErrorKind> {
        let target = Self::resolve_target(client, &ctx).await?;
        let mut budget = ScanBudget::new(ctx.content.caps());
        let found = Self::scan(client, &ctx, &target, ScanArity::FirstMatch, &mut budget).await?;
        // `ScanArity::FirstMatch` returns the moment a candidate passes, so a
        // non-empty result holds exactly that candidate.
        found
            .matches
            .into_iter()
            .next()
            .map(|(verify, _)| verify)
            .ok_or(VerifyErrorKind::NoSignaturesFound)
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

        // 1. Resolve the per-platform target manifest.
        let resolved = match ctx
            .index
            .select(ctx.identifier, ctx.platform, IndexOperation::Resolve)
            .await
            .map_err(|e| VerifyErrorKind::Internal(Box::new(e)))?
        {
            SelectResult::Found(id) => id,
            SelectResult::Ambiguous(_) | SelectResult::NotFound | SelectResult::FeatureMismatch { .. } => {
                return Err(VerifyErrorKind::TargetNotFound {
                    platform: ctx.platform.to_string(),
                });
            }
        };
        let subject_digest = resolved.digest().ok_or_else(|| VerifyErrorKind::TargetNotFound {
            platform: ctx.platform.to_string(),
        })?;
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
        Ok(ScanTarget { image, subject_digest })
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
        let ScanTarget { image, subject_digest } = target;

        // 2. List signature referrers (capability cache short-circuits a known
        //    Unsupported registry without re-listing), then re-filter client-side
        //    to Sigstore-bundle referrers — the OCI spec permits a registry to
        //    ignore the server-side artifactType filter.
        //
        //    The re-filter drops only referrers that declare a *different*
        //    explicit artifactType. A referrer with no artifactType (absent in
        //    the listing, or a transport that does not echo it) is kept: the
        //    bundle parse downstream fail-closes on a non-bundle, so tolerating
        //    an absent type here cannot admit a forged signature — but rejecting
        //    it would drop a genuine server-matched referrer (regression class:
        //    a registry that matched server-side but omits the per-descriptor
        //    artifactType echo).
        let referrers = Self::list_signature_referrers(transport, ctx, image, subject_digest).await?;
        let mut candidates: Vec<crate::oci::Descriptor> = referrers
            .into_iter()
            .filter(|descriptor| match descriptor.artifact_type.as_deref() {
                Some(artifact_type) => artifact_type == SIGSTORE_BUNDLE_V03,
                None => true,
            })
            .collect();
        if candidates.is_empty() {
            // Before the trust-root gate below: a subject with no bundle
            // referrer at all is "not signed", and a missing trust root is not
            // the thing to report about it. The caller promotes any refusal it
            // recorded over this kind.
            return Err(VerifyErrorKind::NoSignaturesFound);
        }
        order_candidates(&mut candidates, &ctx.content);

        // Refused up front rather than at the first signature check: a keyless
        // trust root is a configuration mistake with a fixed remedy, and it
        // would otherwise surface as an opaque SCT failure per candidate.
        // `sigstore` builds an empty CT keyring without complaint.
        if ctx.trust_root.ctfe_key_map().is_empty() {
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

        // The signed artifact's bytes, not just its digest: `Verifier` hashes a
        // preimage. Fetched once per run, after the referrer listing so an
        // unsigned artifact costs no extra request.
        let subject_bytes = pull_subject_manifest_verified(transport, image, subject_digest).await?;

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
        let total_candidates = candidates.len();
        // Every refusal is kept, not folded into one "best" as it arrives: the
        // aggregate error is derived from this at the end (`best_failure`), and
        // a scan that *does* find matches still carries the refusals out so the
        // caller can report them.
        let mut refused: Vec<RefusedCandidate> = Vec::new();
        let mut matches: Vec<(VerifyResult, Option<VerifiedAttestation>)> = Vec::new();
        // Resolved from the requested mode, before the first fetch (D-d).
        let caps = budget.caps;
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
                budget,
            )
            .await
            {
                Ok(CandidateOutcome::Verified { result, attestation }) => {
                    budget.examined();
                    matches.push((result, attestation));
                    if arity == ScanArity::FirstMatch {
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
        Self::finish_scan(ctx, caps, total_candidates, budget, matches, refused)
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
            VerifyContentMode::Signature => {
                // Unchanged: a `FirstMatch` scan reaching here found nothing, so
                // the aggregate is today's, over the candidates actually looked
                // at rather than the ones that spent a slot. The refusals are
                // consumed to build it — nothing survives this arm to report.
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
        budget: &mut ScanBudget,
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
        let bundle_bytes = match pull_bundle_blob_capped(transport, image, &bundle_blob_digest, caps.bundle_bytes).await
        {
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
                VerifyContentMode::Signature => None,
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
        let rekor_key_pem = verify_rekor_set(ctx, &parts).await?;

        // D-d, the tlog half: checklist row 12, over entry material the two
        // calls above have already SET- and Merkle-checked. Splitting it out of
        // the structural half is what lets the structural kinds be precise while
        // this one still runs against a body known to be the logged one.
        if let Some(verified) = verified_envelope.as_ref() {
            dsse::verify_tlog_binding(
                &parts.canonicalized_body,
                &verified.attestation.payload,
                &verified.signatures,
            )?;
        }

        // Identity + issuer match against the resolved trust policies (ANY-of).
        // The matched subset, not a boolean: the `builder` pin below is ANDed
        // within a policy and ORed across the set, so it is decided from the
        // policies this certificate actually satisfied.
        let matched_policies = matching_policies(&parts.leaf_der, ctx.policies)?;

        // #103. Inert in signature mode, and inert on a non-provenance
        // predicate; a refusal — never a skip — when a pin is in force and the
        // provenance names another builder or none that can be read.
        if let Some(verified) = verified_envelope.as_ref() {
            dsse::enforce_builder_pin(&matched_policies, &verified.attestation)?;
        }

        // On a successful online run, cache the trust material for later offline
        // verifies against the same Rekor instance. Best-effort + content-equal
        // skip so a batch does not stampede the file or slide the 24h TTL on use.
        if !ctx.offline {
            cache_trust_material(ctx, rekor_key_pem).await;
        }

        let cert = parse_certificate(&parts.leaf_der)?;

        // Row 13 (CVE-2024-55655), re-asserted over the same parsed leaf the
        // identity extraction below reads. Placed in the tail both content
        // modes share, so "runs for attestations too" is structural.
        tlog::verify_integrated_time_within_certificate(
            // Saturating rather than fallible: `from_bundle` widened a
            // non-negative i64 into this u64, so the conversion cannot trip,
            // and i64::MAX would fail closed against any real window.
            i64::try_from(parts.integrated_time).unwrap_or(i64::MAX),
            &cert,
        )?;

        // Emit the result (identity/issuer read back from the cert).
        Ok(CandidateOutcome::Verified {
            result: VerifyResult {
                subject_digest: subject_digest.clone(),
                referrer_digest,
                certificate_identity: subject_identity(&cert).unwrap_or_default(),
                certificate_oidc_issuer: oidc_issuer(&cert).unwrap_or_default(),
                signed_at: parts.integrated_time,
            },
            attestation: verified_envelope.map(|verified| verified.attestation),
        })
    }

    /// List the Sigstore-bundle referrers for the subject, wiring the
    /// capability cache. `Unsupported` → exit 84; empty → the caller maps to
    /// `NoSignaturesFound` (79).
    async fn list_signature_referrers(
        transport: &dyn OciTransport,
        ctx: &VerifyContext<'_>,
        image: &native::Reference,
        subject_digest: &Digest,
    ) -> Result<Vec<crate::oci::Descriptor>, VerifyErrorKind> {
        Self::ensure_referrers_supported(transport, ctx, image, subject_digest).await?;

        // Fetch the signature referrers (server-side artifactType filter).
        transport
            .list_referrers(image, subject_digest, Some(SIGSTORE_BUNDLE_V03))
            .await
            .map_err(map_client_error)
    }

    /// Confirm the registry serves the OCI Referrers API before any listing.
    ///
    /// Shared by both passes rather than folded into the signed one: a registry
    /// that does not serve referrers has no unsigned SBOMs either, and the
    /// answer is the same 84. Skipping it in the unsigned pass would turn a
    /// cached `Unsupported` into a live request the cache exists to avoid, and
    /// would report the miss as whatever the registry answered instead.
    ///
    /// The probe result is cached, so the second caller in a run normally reads
    /// the entry the first one wrote; `--no-cache` deliberately buys a second
    /// probe rather than a stale answer.
    async fn ensure_referrers_supported(
        transport: &dyn OciTransport,
        ctx: &VerifyContext<'_>,
        image: &native::Reference,
        subject_digest: &Digest,
    ) -> Result<(), VerifyErrorKind> {
        // Capability: a fresh cache entry avoids a re-probe; otherwise probe
        // and persist. `Unsupported` fails hard (no fallback-tag reads, S1-F).
        // Cache key = the host actually probed (`probe` records the same one),
        // so a mirrored registry caches under the mirror, not the upstream.
        let cached = if ctx.no_cache {
            None
        } else {
            ReferrersApiCapability::from_cache(image.resolve_registry(), ctx.state)
                .await
                .ok()
                .flatten()
                .filter(ReferrersApiCapability::is_fresh)
        };
        let capability = match cached {
            Some(hit) => hit,
            None => {
                let probed = ReferrersApiCapability::probe(transport, image, subject_digest)
                    .await
                    .map_err(map_client_error)?;
                // Best-effort cache write; a failure here must not fail the
                // verify. Logged so a permanently unwritable `state/referrers/`
                // presents as a diagnosable line rather than silent per-install
                // re-probing of the registry.
                if let Err(error) = probed.write_cache(ctx.state).await {
                    tracing::debug!("referrers capability cache write skipped: {error}");
                }
                probed
            }
        };
        if capability.supported == ReferrersSupport::Unsupported {
            return Err(VerifyErrorKind::ReferrersUnsupported);
        }
        Ok(())
    }
}

/// The verification-relevant fields extracted from a parsed bundle.
struct BundleParts {
    leaf_der: Vec<u8>,
    signed_entry_timestamp: Vec<u8>,
    canonicalized_body: Vec<u8>,
    integrated_time: u64,
    log_index: u64,
    log_id_hex: String,
    /// The Merkle inclusion proof. Not optional: a bundle without one is
    /// refused in [`BundleParts::from_bundle`], so downstream code cannot
    /// forget to check and cannot silently fall back to the SET alone.
    inclusion_proof: ProtoInclusionProof,
}

impl BundleParts {
    fn from_bundle(
        bundle: &sigstore_protobuf_specs::dev::sigstore::bundle::v1::Bundle,
        mode: &VerifyContentMode,
    ) -> Result<Self, VerifyErrorKind> {
        // The candidate must carry the content kind this run asked for: a DSSE
        // envelope is an attestation, a message signature is an artifact
        // signature, and neither answers the other question. Both directions
        // are a per-candidate verdict, not an abort — the scan records it as
        // `ModeMismatch`, which charges the bytes and spends no candidate slot,
        // and keeps going.
        //
        // Asked FIRST, before the verification material is read, because
        // discriminating the kind needs only the `content` oneof `parse_bundle`
        // has already produced. Reading material first would report a malformed
        // bundle of the *other* kind as this mode's failure, spending a slot on
        // it — the same crowd-out the non-consuming skip exists to prevent,
        // reached through a different door: eight junk DSSE bundles would still
        // exhaust a signature scan.
        let content_matches_mode = matches!(
            (mode, bundle.content.as_ref()),
            (VerifyContentMode::Signature, Some(bundle::Content::MessageSignature(_)))
                | (
                    VerifyContentMode::Attestation { .. },
                    Some(bundle::Content::DsseEnvelope(_))
                )
        );
        if !content_matches_mode {
            return Err(VerifyErrorKind::NoUsableBundle);
        }

        let material = bundle
            .verification_material
            .as_ref()
            .ok_or(VerifyErrorKind::BundleParseFailed)?;
        let leaf_der = match material.content.as_ref() {
            Some(verification_material::Content::X509CertificateChain(chain)) => {
                chain.certificates.first().map(|c| c.raw_bytes.clone())
            }
            Some(verification_material::Content::Certificate(cert)) => Some(cert.raw_bytes.clone()),
            _ => None,
        }
        .ok_or(VerifyErrorKind::BundleParseFailed)?;
        let tlog = material.tlog_entries.first().ok_or(VerifyErrorKind::RekorSetInvalid)?;
        let set = tlog
            .inclusion_promise
            .as_ref()
            .map(|p| p.signed_entry_timestamp.clone())
            .ok_or(VerifyErrorKind::RekorSetAbsentTsaPresent)?;
        let log_id_hex = tlog.log_id.as_ref().map(|l| hex::encode(&l.key_id)).unwrap_or_default();

        // Mandatory. The SET is only a promise to include; the proof is the
        // evidence that the entry is in a tree whose root the log signed.
        // Bundle profile v0.1/v0.2 leaves the proof optional at the schema
        // level, so without this a promise-only bundle would verify on strictly
        // weaker evidence than `sigstore`'s own online branch accepts.
        let inclusion_proof = tlog
            .inclusion_proof
            .clone()
            .ok_or(VerifyErrorKind::RekorInclusionProofAbsent)?;

        Ok(Self {
            leaf_der,
            signed_entry_timestamp: set,
            canonicalized_body: tlog.canonicalized_body.clone(),
            integrated_time: tlog.integrated_time.max(0) as u64,
            log_index: tlog.log_index.max(0) as u64,
            log_id_hex,
            inclusion_proof,
        })
    }
}

/// Verify the Rekor transparency evidence, returning the log key PEM used.
///
/// Checks the Signed Entry Timestamp and the Merkle inclusion proof, both
/// mandatory. Both are computed by `sigstore-rs` in [`tlog`] — no signature,
/// hash-chain or checkpoint parsing lives here.
///
/// Key source, in order:
/// 1. **Pinned** — the trust root carries a Rekor public key (from a TUF root or
///    the trust-root cache). Used with no network; this is the offline path and
///    the fix for #194's trust-on-first-use Rekor-key fetch.
/// 2. **Offline, unpinned** — cannot fetch and no pinned key → fail. (The CLI
///    gates this to an actionable exit-78 error before the pipeline runs; this
///    is the defensive backstop.)
/// 3. **Online, unpinned** — TOFU-fetch from `--rekor-url/api/v1/log/publicKey`
///    (the prior behavior), and return it so the caller can cache it.
async fn verify_rekor_set(ctx: &VerifyContext<'_>, parts: &BundleParts) -> Result<String, VerifyErrorKind> {
    let pem = match ctx.trust_root.rekor_public_key_pem_for(&parts.log_id_hex) {
        Some(pinned) => pinned,
        None if ctx.offline => return Err(VerifyErrorKind::TransparencyLogUnavailable),
        None => fetch_rekor_public_key_pem(ctx.rekor_url).await?,
    };
    let key = tlog::rekor_key(&pem)?;
    tlog::verify_set(
        &key,
        &tlog::TlogEntry {
            canonicalized_body: &parts.canonicalized_body,
            integrated_time: parts.integrated_time,
            log_index: parts.log_index,
            log_id_hex: &parts.log_id_hex,
            signed_entry_timestamp: &parts.signed_entry_timestamp,
        },
    )?;

    // The Merkle proof is independent evidence, and `BundleParts` has already
    // guaranteed it is present — the type carries the invariant so no caller
    // can fall back to the SET alone.
    tlog::verify_inclusion(&key, &parts.inclusion_proof, &parts.canonicalized_body)?;
    Ok(pem)
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

/// Pull the bundle blob with a hard in-memory read cap (CWE-400 defense).
///
/// Reads at most `cap + 1` bytes so an over-cap body is detected and rejected
/// without buffering the whole thing — the pre-download descriptor check bounds
/// the honest case, this bounds a registry that lies about the size. For an
/// honest under-cap blob the native transport's `VerifyingStream` still checks
/// the blob digest at stream end.
///
/// `cap` comes from the run's [`VerifyContentMode`], never from the candidate.
async fn pull_bundle_blob_capped(
    transport: &dyn OciTransport,
    image: &native::Reference,
    bundle_blob_digest: &Digest,
    cap: usize,
) -> Result<Vec<u8>, VerifyErrorKind> {
    use tokio::io::AsyncReadExt as _;
    let reader = transport
        .pull_blob_streaming(image, bundle_blob_digest)
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
        result: VerifyResult,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScanStop {
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
        let stop = if self.examined >= self.caps.candidates {
            ScanStop::CandidateCap
        } else if self.spent >= self.caps.total_bytes {
            ScanStop::ByteBudget
        } else if self.considered >= MAX_REFERRER_LISTING_ITERATION {
            ScanStop::ListingCap
        } else {
            return true;
        };
        self.stop = Some(stop);
        false
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

/// Whether a listed referrer's `dev.sigstore.bundle.content` annotation names a
/// content kind other than the one this run is looking for.
///
/// Absent annotation → `false`: a referrer pushed by a tool that writes no hint,
/// or a transport that does not echo listing annotations, must not be demoted
/// behind one that does. An unrecognised value → `true`: it does not name this
/// run's kind, and demotion costs it nothing but position.
fn annotation_disagrees_with_mode(descriptor: &crate::oci::Descriptor, mode: &VerifyContentMode) -> bool {
    let Some(hint) = descriptor
        .annotations
        .as_ref()
        .and_then(|annotations| annotations.get(ANNOTATION_BUNDLE_CONTENT))
    else {
        return false;
    };
    hint != match mode {
        VerifyContentMode::Signature => BUNDLE_CONTENT_MESSAGE_SIGNATURE,
        VerifyContentMode::Attestation { .. } => BUNDLE_CONTENT_DSSE,
    }
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
struct PolicyDeferredToOcx;

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
fn map_verification_error(error: sigstore::bundle::verify::VerificationError) -> VerifyErrorKind {
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

/// Map an OCI client error into the verify taxonomy.
fn map_client_error(error: ClientError) -> VerifyErrorKind {
    match error {
        ClientError::ReferrersUnsupported { .. } => VerifyErrorKind::ReferrersUnsupported,
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
        let bundle = message_bundle(false, false);
        assert!(matches!(
            BundleParts::from_bundle(&bundle, &VerifyContentMode::Signature),
            Err(VerifyErrorKind::BundleParseFailed)
        ));
    }

    #[test]
    fn from_bundle_requires_a_tlog_entry() {
        let bundle = message_bundle(true, false);
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

    /// Both directions, because a gate that only ever sees one content kind is
    /// indistinguishable from no gate: a DSSE attestation is not an artifact
    /// signature, and a message signature is not an attestation. Each side
    /// asserts the accept *and* the skip, so hard-wiring either answer reds.
    #[test]
    fn bundle_content_must_match_requested_mode() {
        let signature_bundle = message_bundle(true, true);
        let attestation_bundle = dsse_bundle();
        let attestation_mode = VerifyContentMode::Attestation { predicate_type: None };

        assert!(
            BundleParts::from_bundle(&signature_bundle, &VerifyContentMode::Signature).is_ok(),
            "signature mode must accept a message signature"
        );
        assert!(
            BundleParts::from_bundle(&attestation_bundle, &attestation_mode).is_ok(),
            "attestation mode must accept a DSSE envelope"
        );
        assert!(
            matches!(
                BundleParts::from_bundle(&attestation_bundle, &VerifyContentMode::Signature),
                Err(VerifyErrorKind::NoUsableBundle)
            ),
            "signature mode must skip a DSSE envelope"
        );
        assert!(
            matches!(
                BundleParts::from_bundle(&signature_bundle, &attestation_mode),
                Err(VerifyErrorKind::NoUsableBundle)
            ),
            "attestation mode must skip a message signature"
        );
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
        let bundle = message_bundle_with(true, true, false);
        assert!(
            matches!(
                BundleParts::from_bundle(&bundle, &VerifyContentMode::Signature),
                Err(VerifyErrorKind::RekorInclusionProofAbsent)
            ),
            "a bundle with an inclusion promise but no proof must be refused"
        );
    }

    #[test]
    fn from_bundle_extracts_message_signature_parts() {
        let bundle = message_bundle(true, true);
        let parts = BundleParts::from_bundle(&bundle, &VerifyContentMode::Signature).expect("valid message bundle");
        assert_eq!(parts.integrated_time, 100);
        assert_eq!(parts.log_index, 5);
        assert_eq!(parts.log_id_hex, "ab");
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
        let platform = crate::oci::Platform::any();
        let ctx = VerifyContext {
            identifier: &identifier,
            platform: &platform,
            policies: &[],
            no_cache: true,
            index: &index,
            trust_root: &trust_root,
            rekor_url: &rekor_url,
            state: &state,
            offline: true,
            content: VerifyContentMode::Signature,
            verification: VerificationMode::Demand,
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
                budget,
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
    async fn pull_bundle_blob_capped_streams_honest_blob_and_rejects_oversize() {
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

        let streamed = pull_bundle_blob_capped(&transport, &image, &honest_digest, MAX_BUNDLE_SIZE_BYTES)
            .await
            .expect("honest under-cap blob streams back");
        assert_eq!(streamed, honest, "streamed bytes must equal the stored blob");

        assert!(
            matches!(
                pull_bundle_blob_capped(&transport, &image, &oversize_digest, MAX_BUNDLE_SIZE_BYTES).await,
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
    ) -> (Result<VerifyResult, VerifyError>, Vec<String>, tempfile::TempDir) {
        let (_key, cert) = self_signed_cert();
        drive_verify_with_trust_root(physical, mirrors, trust_root_of(&[&cert])).await
    }

    /// `drive_verify_at` with the trust root made an argument, so a test can
    /// drive the run with material the pipeline is expected to refuse.
    async fn drive_verify_with_trust_root(
        physical: &str,
        mirrors: crate::oci::client::MirrorMap,
        trust_root: TrustRoot,
    ) -> (Result<VerifyResult, VerifyError>, Vec<String>, tempfile::TempDir) {
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
        let platform = crate::oci::Platform::any();

        let outcome = VerifyPipeline::run(
            &client,
            VerifyContext {
                identifier: &logical,
                platform: &platform,
                policies: &[],
                no_cache: true,
                index: &index,
                trust_root: &trust_root,
                rekor_url: &rekor_url,
                state: &state,
                offline: true,
                content: VerifyContentMode::Signature,
                verification: VerificationMode::Demand,
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
        // both the capability probe and the referrer listing follow the mirror.
        // The write assertion is the standing guard — a push added here would
        // hit a read-only mirror (ADR Q5).
        let mirrors = crate::oci::client::MirrorMap::new([(
            "8.8.8.8".to_string(),
            crate::config::mirror::ParsedMirror {
                protocol: "https".to_string(),
                host: "mirror.example".to_string(),
                path_prefix: "proxy".to_string(),
            },
        )]);
        let (calls, state_dir) = run_recorded_verify(mirrors).await;

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

        // The capability record must be keyed on the host actually probed.
        // `from_cache` returns None when the stored `registry` disagrees with
        // the lookup key, so these two assertions pin the key exactly.
        let state = StateStore::new(state_dir.path());
        assert!(
            ReferrersApiCapability::from_cache("mirror.example", &state)
                .await
                .expect("cache read")
                .is_some(),
            "verify must cache the capability under the mirror it probed",
        );
        assert!(
            ReferrersApiCapability::from_cache("8.8.8.8", &state)
                .await
                .expect("cache read")
                .is_none(),
            "verify must not cache a mirror's verdict under the canonical host",
        );
    }

    // NOTE: the pipeline-wire E2E adversarial cases — ANY-of key rotation,
    //   malformed-first-referrer DoS, and the cross-subject splice — need a
    //   transport that serves `list_referrers` + referrer manifests + bundle blobs
    //   plus real Fulcio-minted certs and a real Rekor SET. `StubTransport`
    //   deliberately leaves `list_referrers` `unimplemented!()`, and minting that
    //   crypto material in Rust would mean reimplementing a Sigstore CA here.
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
        platform: &'a crate::oci::Platform,
        index: &'a Index,
        trust_root: &'a TrustRoot,
        rekor_url: &'a Url,
        state: &'a StateStore,
    ) -> VerifyContext<'a> {
        VerifyContext {
            identifier,
            platform,
            policies: &[],
            no_cache: true,
            index,
            trust_root,
            rekor_url,
            state,
            offline: true,
            content: VerifyContentMode::Attestation { predicate_type: None },
            verification: VerificationMode::Demand,
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
        let platform = crate::oci::Platform::any();
        let index = Index::from_impl(IndirectingIndex {
            physical: identifier.clone(),
        });
        let trust_root = trust_root_of(&[]);
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = StateStore::new(temp.path());
        let ctx = attestation_ctx(&identifier, &platform, &index, &trust_root, &rekor_url, &state);
        let caps = ctx.content.caps();

        let verified = || {
            (
                VerifyResult {
                    subject_digest: crate::oci::Algorithm::Sha256.hash(b"subject"),
                    referrer_digest: crate::oci::Algorithm::Sha256.hash(b"referrer"),
                    certificate_identity: String::new(),
                    certificate_oidc_issuer: String::new(),
                    signed_at: 0,
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
        let platform = crate::oci::Platform::any();
        let index = Index::from_impl(IndirectingIndex {
            physical: identifier.clone(),
        });
        let trust_root = trust_root_of(&[]);
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = StateStore::new(temp.path());
        let ctx = attestation_ctx(&identifier, &platform, &index, &trust_root, &rekor_url, &state);
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
        let platform = crate::oci::Platform::any();
        let index = Index::from_impl(IndirectingIndex {
            physical: identifier.clone(),
        });
        let trust_root = trust_root_of(&[]);
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = StateStore::new(temp.path());
        let ctx = attestation_ctx(&identifier, &platform, &index, &trust_root, &rekor_url, &state);
        let caps = ctx.content.caps();

        let matches: Vec<(VerifyResult, Option<VerifiedAttestation>)> = (0..3u8)
            .map(|n| {
                (
                    VerifyResult {
                        subject_digest: crate::oci::Algorithm::Sha256.hash(b"subject"),
                        referrer_digest: crate::oci::Algorithm::Sha256.hash([n]),
                        certificate_identity: String::new(),
                        certificate_oidc_issuer: String::new(),
                        signed_at: 0,
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
        let platform = crate::oci::Platform::any();
        let index = Index::from_impl(IndirectingIndex {
            physical: identifier.clone(),
        });
        let trust_root = trust_root_of(&[]);
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = StateStore::new(temp.path());
        let ctx = attestation_ctx(&identifier, &platform, &index, &trust_root, &rekor_url, &state);
        let caps = ctx.content.caps();

        let passing = (
            VerifyResult {
                subject_digest: crate::oci::Algorithm::Sha256.hash(b"subject"),
                referrer_digest: crate::oci::Algorithm::Sha256.hash(b"good"),
                certificate_identity: String::new(),
                certificate_oidc_issuer: String::new(),
                signed_at: 0,
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
        let platform = crate::oci::Platform::any();
        let index = Index::from_impl(IndirectingIndex {
            physical: identifier.clone(),
        });
        let trust_root = trust_root_of(&[]);
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = StateStore::new(temp.path());
        let ctx = VerifyContext {
            identifier: &identifier,
            platform: &platform,
            policies: &[],
            no_cache: true,
            index: &index,
            trust_root: &trust_root,
            rekor_url: &rekor_url,
            state: &state,
            offline: true,
            content: VerifyContentMode::Signature,
            verification: VerificationMode::Demand,
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

        let signature_without_material = message_bundle(false, false);
        assert!(
            matches!(
                BundleParts::from_bundle(&signature_without_material, &attestation_mode),
                Err(VerifyErrorKind::NoUsableBundle)
            ),
            "and symmetrically in the other direction",
        );

        // The gate moving earlier must not swallow a genuine malformed-bundle
        // report for a candidate that IS the requested kind.
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
    /// Both modes asserted, so a demotion wired to one answer reds.
    #[test]
    fn a_content_hint_naming_the_other_kind_sorts_behind_every_other_candidate() {
        let crowd = || {
            let mut candidates: Vec<crate::oci::Descriptor> = (0..MAX_SIGNATURE_CANDIDATES)
                .map(|n| listed(&format!("sha256:0{n}"), Some(BUNDLE_CONTENT_DSSE)))
                .collect();
            // Sorts last by digest, which is what makes it unreachable without
            // the demotion: every attestation above it spends a slot first.
            candidates.push(listed("sha256:ff", Some(BUNDLE_CONTENT_MESSAGE_SIGNATURE)));
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
            listed("sha256:a", Some(BUNDLE_CONTENT_MESSAGE_SIGNATURE)),
            listed("sha256:b", None),
            // Sorts second by digest; the hint is what moves it to the tail.
            listed("sha256:a0", Some(BUNDLE_CONTENT_DSSE)),
        ];
        order_candidates(&mut candidates, &VerifyContentMode::Signature);
        assert_eq!(
            digests(&candidates),
            ["sha256:a", "sha256:b", "sha256:c", "sha256:a0"],
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
            listed("sha256:cc", Some(BUNDLE_CONTENT_MESSAGE_SIGNATURE)),
        ];
        order_candidates(&mut candidates, &VerifyContentMode::Signature);
        assert_eq!(
            digests(&candidates),
            ["sha256:cc", "sha256:aa", "sha256:bb"],
            "a matching hint leads; mismatched and unrecognised hints follow in digest order",
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
        let platform = crate::oci::Platform::any();
        let index = Index::from_impl(IndirectingIndex {
            physical: identifier.clone(),
        });
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = StateStore::new(temp.path());
        let ctx = attestation_ctx(&identifier, &platform, &index, &trust_root, &rekor_url, &state);

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
                &mut budget,
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
        let platform = crate::oci::Platform::any();
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
                platform: &platform,
                policies: &[],
                no_cache: true,
                index: &index,
                trust_root: &trust_root,
                rekor_url: &rekor_url,
                state: &state,
                offline: true,
                content: content.clone(),
                verification: VerificationMode::Demand,
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
                &mut budget,
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
        let platform = crate::oci::Platform::any();
        let index = Index::from_impl(IndirectingIndex {
            physical: identifier.clone(),
        });
        let trust_root = trust_root_of(&[]);
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = StateStore::new(temp.path());
        let ctx = attestation_ctx(&identifier, &platform, &index, &trust_root, &rekor_url, &state);
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
                declared_layer_size: None,
            }
        }

        fn document_digest(&self) -> Digest {
            crate::oci::Algorithm::Sha256.hash(&self.document)
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
            crate::oci::referrer::ReferrerManifest::build(subject, &self.artifact_type, payload, None)
                .to_canonical_json()
                .expect("referrer manifest json")
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

    /// Serves a caller-chosen referrer set and records what was asked for.
    ///
    /// Honours the server-side `artifactType` filter, unlike the
    /// spec-permitted registry that ignores it — which is the point: the signed
    /// pass must ask for bundles and get only bundles, so an unsigned referrer
    /// reaching a verification candidate would be this double's fault to expose
    /// and not to hide.
    #[derive(Clone)]
    struct SbomTransport {
        referrers: Vec<StubReferrer>,
        listing_filters: std::sync::Arc<std::sync::Mutex<Vec<Option<String>>>>,
        pulled_blobs: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl SbomTransport {
        fn new(referrers: Vec<StubReferrer>) -> Self {
            Self {
                referrers,
                listing_filters: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
                pulled_blobs: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            }
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
            let wanted = image.digest().unwrap_or_default().to_string();
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
            let referrer = self
                .referrers
                .iter()
                .find(|stub| &stub.document_digest() == digest)
                .expect("the scan only asks for blobs a listed referrer named");
            Ok(Box::new(std::io::Cursor::new(referrer.document.clone())))
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
            _: &Digest,
            artifact_type: Option<&str>,
        ) -> std::result::Result<Vec<crate::oci::Descriptor>, ClientError> {
            self.listing_filters
                .lock()
                .expect("recorder lock")
                .push(artifact_type.map(str::to_string));
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
        let logical = Identifier::parse("ocx.sh/acme/tool:1.0").expect("logical identifier");
        // A public IP literal, not a name: the dial-site SSRF guard resolves the
        // physical host, and a DNS name here would make this unit test open a
        // socket.
        let physical = Identifier::parse("8.8.8.8/acme/tool:1.0").expect("physical identifier");

        let transport = SbomTransport::new(referrers);
        let client = Client::with_transport(Box::new(transport.clone()));
        let index = Index::from_impl(IndirectingIndex { physical });
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = StateStore::new(temp.path());
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let platform = crate::oci::Platform::any();

        let outcome = VerifyPipeline::run_attestations(
            &client,
            VerifyContext {
                identifier: &logical,
                platform: &platform,
                policies: &[],
                no_cache: true,
                index: &index,
                trust_root: &trust_root,
                rekor_url: &rekor_url,
                state: &state,
                offline: true,
                content: VerifyContentMode::Attestation { predicate_type },
                verification,
            },
        )
        .await;
        (outcome, transport, temp)
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
        let platform = crate::oci::Platform::any();
        let (_key, cert) = self_signed_cert();
        let trust_root = trust_root_of(&[&cert]);

        let outcome = VerifyPipeline::run(
            &client,
            VerifyContext {
                identifier: &logical,
                platform: &platform,
                policies: &[],
                no_cache: true,
                index: &index,
                trust_root: &trust_root,
                rekor_url: &rekor_url,
                state: &state,
                offline: true,
                content: VerifyContentMode::Attestation { predicate_type: None },
                verification: VerificationMode::Demand,
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
}
