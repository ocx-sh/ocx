// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Verify pipeline: resolve, list referrers, verify each candidate (ANY-of), emit [`VerifyResult`].
//!
//! `sigstore`'s `Verifier` owns X.509, signature and log-body binding; the Rekor SET and Merkle proof are checked
//! here ([`tlog`]) because `sigstore` 0.14 leaves them `TODO` (sigstore-rs#285).

use sigstore::bundle::verify::Verifier;
use sigstore::bundle::verify::policy::{PolicyResult, VerificationPolicy};
use sigstore::rekor::apis::configuration::Configuration as RekorConfiguration;
use url::Url;

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use super::DiscoveryMethod;
use super::attestation_sidecar;
use super::dsse::{self, VerifiedAttestation, VerifiedEnvelope};
use super::error::{TrustRootLoadReason, VerifyError, VerifyErrorKind};
use super::identity::{self, matching_policies, oidc_issuer, parse_certificate, subject_identity};
use super::signing_instant::SigningInstant;
use super::simplesigning_read::{self, SidecarKind, SidecarScan};
use super::tlog;
use super::trust_cache::TrustRootCache;
use super::trust_root::TrustRoot;
use crate::attest::predicate::{PredicateType, sbom_predicate_type_uri};
use crate::attest::{
    COSIGN_SIGN_PREDICATE_TYPE, MAX_ATTESTATION_CANDIDATES, MAX_ATTESTATION_ENVELOPE_BYTES, MAX_TOTAL_ATTESTATION_BYTES,
};
use crate::sign::SignatureFormat;
use crate::sign::bundle::{MAX_BUNDLE_SIZE_BYTES, parse_bundle};
use crate::sign::state::SigningStatePaths;
use ocx_oci::client::error::ClientError;
use ocx_oci::client::{Client, OciTransport, ReferrersListing, sibling_tag_reference};
use ocx_oci::referrer::media_types::{
    ANNOTATION_BUNDLE_CONTENT, ANNOTATION_BUNDLE_PREDICATE_TYPE, BUNDLE_CONTENT_DSSE, COSIGN_SBOM_ARTIFACT_TYPE,
    COSIGN_SIG_ARTIFACT_TYPE, SIGSTORE_BUNDLE_V03,
};
use ocx_oci::resolve_target::{ResolveTargetError, ResolvedSubject, SignTarget};
use ocx_oci::ssrf::DialPolicy;
use ocx_oci::{Digest, ImageManifest, PackageRef, Platform, native};
use ocx_trust::PolicyBackend;
use ocx_trust::key_ref::KeyBackendKind;
use sigstore_protobuf_specs::dev::sigstore::bundle::v1::{Bundle, bundle, verification_material};
use sigstore_protobuf_specs::dev::sigstore::rekor::v1::InclusionProof as ProtoInclusionProof;

pub(super) const ACCEPTED_MANIFEST_TYPES: &[&str] = &[
    ocx_oci::OCI_IMAGE_MEDIA_TYPE,
    "application/vnd.docker.distribution.manifest.v2+json",
];

/// Maximum accepted size of a referrer manifest, in bytes (a real one is a few hundred).
pub(super) const MAX_REFERRER_MANIFEST_BYTES: u64 = 256 * 1024;

/// Maximum number of signature referrers examined during an ANY-of verify.
pub(super) const MAX_SIGNATURE_CANDIDATES: usize = 8;

/// Cross-candidate byte budget over referrer-manifest descriptor sizes.
const MAX_TOTAL_REFERRER_BYTES: u64 = 4 * 1024 * 1024;

/// Hard backstop on listed referrers one scan iterates; other-kind candidates consume no candidate slot.
const MAX_REFERRER_LISTING_ITERATION: usize = 256;

/// How many answers a scan is looking for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScanArity {
    /// Stop at the first candidate that fully passes.
    FirstMatch,
    /// Examine every candidate the caps allow.
    All,
}

/// What kind of signed content a verify run is looking for; selects the caps before the first fetch
/// (`adr_sbom_attestations.md` D-d).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyContentMode {
    /// An artifact message signature.
    Signature,
    /// An in-toto attestation carried in a DSSE envelope.
    Attestation {
        /// Narrows the search to one predicate type; `None` accepts any.
        predicate_type: Option<PredicateType>,
    },
}

/// Whether a run demands cryptographic verification, or merely reads.
// Resolved once by the caller and carried in, so the pipeline cannot disagree with the mode the CLI reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationMode {
    /// Every document must verify against the resolved policies; an unsigned attachment is refused.
    Demand,
    /// No cryptography runs; every document is read and reported as unverified, never as verified.
    Permissive,
}

/// The untrusted-byte bounds one verify run enforces, chosen by content mode.
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
    // Keep the attestation bounds on their own arm: shared, they would silently relax `ocx package verify`.
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

/// A caller-supplied resolution: [`SubjectResolver`](crate::sign::pipeline::SubjectResolver) in the verify taxonomy.
pub type VerifySubjectResolver<'a> = dyn Fn(
        &'a PackageRef,
        Option<&'a Platform>,
    ) -> Pin<Box<dyn Future<Output = Result<ResolvedSubject, VerifyErrorKind>> + Send + 'a>>
    + Send
    + Sync
    + 'a;

/// Context passed into [`VerifyPipeline::run`] — all external dependencies.
pub struct VerifyContext<'a> {
    /// Target identifier (`registry/repo:tag[@digest]`).
    pub identifier: &'a PackageRef,
    /// Platform to narrow into; `None` acts on whatever the reference resolved to, `Some` requires an index.
    pub platform: Option<&'a Platform>,
    /// Resolved ANY-of trust policies the signature must satisfy.
    pub policies: &'a [ocx_trust::CompiledPolicy],
    /// When true, bypass the referrers-capability cache.
    pub no_cache: bool,
    /// The dial-site SSRF floor, resolved for this run's logical registry.
    pub dial: DialPolicy<'a>,
    /// The caller-supplied resolution.
    pub resolve: Box<VerifySubjectResolver<'a>>,
    /// Trust root (Fulcio CA certs + optional pinned Rekor key); injection seam.
    pub trust_root: &'a TrustRoot,
    /// Rekor URL injection seam. Default: `https://rekor.sigstore.dev`.
    pub rekor_url: &'a Url,
    /// The signing tier of the state-root layout.
    pub state: SigningStatePaths,
    /// No Sigstore trust-services network: the Rekor key must come from the trust root; the registry is still read
    /// (`adr_offline_verify_trust_cache.md`).
    pub offline: bool,
    /// Which content kind to look for.
    pub content: VerifyContentMode,
    /// Whether this run demands verification; inert for [`VerifyContentMode::Signature`].
    pub verification: VerificationMode,
    /// The cosign wire shape this run pins; `None` prefers a bundle, falling back to a sidecar only when no bundle
    /// candidate matched or was refused. A pinned shape never discovers the other.
    pub signature_format: Option<SignatureFormat>,
    /// Accept a keyless simplesigning sidecar with **no** transparency-log evidence (`--allow-unlogged-signature`).
    ///
    /// Inert for bundles, whose evidence stays mandatory under keyless and optional under a key.
    pub allow_unlogged_signature: bool,
    /// Widen the scan from first-match to every candidate the caps allow; widens the report, never the verdict.
    pub report_all: bool,
}

/// Result emitted by a successful verify pipeline run.
#[derive(Debug)]
pub struct VerifyResult {
    /// Digest of the subject manifest that was verified.
    pub subject_digest: Digest,
    /// What carried this signature: the referrer manifest under [`SignatureFormat::Bundle`], the layer **blob**
    /// under [`SignatureFormat::Simplesigning`]. Key on [`Self::signature_format`], never [`Self::discovery_method`].
    pub referrer_digest: Digest,
    /// What produced the signature: a Fulcio certificate, or a pinned key.
    pub key_backend: KeyBackendKind,
    /// Cert SAN that signed the subject; absent under a key.
    pub certificate_identity: Option<String>,
    /// Cert OIDC issuer URL; absent under a key.
    pub certificate_oidc_issuer: Option<String>,
    /// Rekor integrated time (UTC epoch seconds); absent without a transparency entry, which only a key allows.
    pub signed_at: Option<u64>,
    /// Which cosign wire shape carried this signature.
    pub signature_format: SignatureFormat,
    /// Which discovery door this signature came through.
    pub discovery_method: DiscoveryMethod,
    /// Rekor log index of the **verified** entry, and the primary dedup key when present.
    ///
    /// Only ever set from an entry that passed [`verify_rekor_set`], never a synthesized one.
    pub rekor_log_index: Option<u64>,
}

/// A verified signature plus the raw bytes the dedup key falls back on, kept off the reported [`VerifyResult`].
#[derive(Debug)]
pub struct VerifiedSignature {
    /// The verification facts, as reported.
    pub result: VerifyResult,
    /// The raw signature bytes this candidate carried.
    pub signature: Vec<u8>,
}

/// The dedup key that makes two discovery doors onto one signature one `signatures[]` row: the Rekor log index when
/// present, otherwise the material (key mode usually has no log index).
#[derive(Debug, PartialEq, Eq)]
enum SignatureKey {
    /// The transparency log's own identifier for this entry.
    RekorLogIndex(u64),
    /// No verified transparency evidence.
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

/// One verified attestation plus the verification facts about the candidate it came from.
#[derive(Debug)]
pub struct AttestationMatch {
    /// Verification facts about the referrer this attestation came from.
    pub verify: VerifyResult,
    /// The attestation itself, as the publisher signed it.
    pub attestation: VerifiedAttestation,
}

/// A candidate that was examined and refused, kept so a caller can report it.
#[derive(Debug)]
pub struct RefusedCandidate {
    /// The referrer's digest, verbatim as the registry listed it.
    pub referrer_digest: String,
    /// Why this candidate was refused.
    pub reason: VerifyErrorKind,
}

/// Everything an attestation scan found; refusals travel beside the matches, so one malformed referrer cannot hide
/// every valid attestation.
#[derive(Debug)]
pub struct AttestationScan {
    /// Every candidate that verified, in listing order.
    pub matches: Vec<AttestationMatch>,
    /// Every **unsigned** SBOM referrer found, in digest order; kept apart from `matches` so it is never mistaken
    /// for a verified one.
    pub unverified: Vec<UnverifiedSbom>,
    /// Every candidate that was examined and refused, in listing order.
    pub refused: Vec<RefusedCandidate>,
    /// The platform manifest `--platform` narrowed to, when this scan also read the enclosing index behind it;
    /// `None` when a single subject was read.
    pub platform_subject: Option<Digest>,
}

/// An SBOM attached without a signature; nothing here is proven, so every consumer must label it unverified.
#[derive(Debug)]
pub struct UnverifiedSbom {
    /// Digest of the OCI referrer manifest carrying the document.
    pub referrer_digest: Digest,
    /// The subject the referrer claims, rather than proves, to be attached to.
    pub subject_digest: Digest,
    /// The predicateType URI the referrer's `artifactType` stands for.
    pub predicate_type: String,
    /// The document, verbatim as served; not a `RawValue`, since `text/spdx` is not JSON.
    pub document: Vec<u8>,
}

/// Verify pipeline entry point.
pub struct VerifyPipeline;

impl VerifyPipeline {
    /// Run the verify pipeline against a [`VerifyContext`].
    ///
    /// Returns every verified signature, scan-ordered and deduplicated; **the verdict is the first element**.
    pub async fn run(client: &Client, ctx: VerifyContext<'_>) -> Result<Vec<VerifyResult>, VerifyError> {
        let identifier = ctx.identifier.clone();
        Self::run_inner(client, ctx)
            .await
            .map_err(|kind| VerifyError::new(identifier, kind))
    }

    /// Collect **every** verified attestation on the target, bounded by the attestation-mode caps.
    ///
    /// # Errors
    ///
    /// [`VerifyErrorKind::AttestationNotFound`] (79) when no match was found, else the most actionable recorded
    /// refusal, and the fail-closed cap refusals when a bound truncated the scan.
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
        // A second subject, not a fallback: cosign attests at the index while OCX pins a platform manifest.
        let index_target = target.index_signature_subject().map(|index_digest| ScanTarget {
            image: target.image.clone(),
            subject_digest: index_digest.clone(),
            // No further indirection: a nested index would need a membership proof nothing here has.
            enclosing_index: None,
            index_members: Vec::new(),
        });
        let platform_subject = index_target.as_ref().map(|_| target.subject_digest.clone());
        let passes = || std::iter::once(&target).chain(index_target.as_ref());

        if ctx.verification == VerificationMode::Permissive {
            let mut unverified = Vec::new();
            let mut refused = Vec::new();
            for pass in passes() {
                let (found, pass_refused) = Self::scan_unverified(client, &ctx, pass, &mut budget).await?;
                unverified.extend(found);
                refused.extend(pass_refused);
            }
            if unverified.is_empty() {
                // A recorded refusal beats "none found", which would deny an attach that happened.
                return Err(best_failure(refused).unwrap_or(VerifyErrorKind::AttestationNotFound));
            }
            return Ok(AttestationScan {
                matches: Vec::new(),
                unverified,
                refused,
                platform_subject,
            });
        }

        // Demand: raw attachments are refused without a fetch, so they spend none of the signed pass's budget.
        let mut matches = Vec::new();
        let mut signed_refused = Vec::new();
        let mut unsigned_refused = Vec::new();
        // The first (user-named) pass's verdict wins the empty-scan ladder below.
        let mut scan_failure = None;
        // Deferred: it may neither fail a run that verifies nor be spent silently as "nothing attached".
        let mut sidecar_fault = None;
        for pass in passes() {
            let (pass_refused, pass_fault) = Self::refuse_unsigned(client, &ctx, pass).await?;
            unsigned_refused.extend(pass_refused);
            sidecar_fault = sidecar_fault.or(pass_fault);
            match Self::scan(client, &ctx, pass, ScanArity::All, &mut budget).await {
                Ok(outcome) => {
                    for (verify, attestation) in outcome.matches {
                        // `None` means mode and outcome drifted apart; fail closed rather than report an empty match.
                        let attestation = attestation.ok_or(VerifyErrorKind::AttestationNotFound)?;
                        matches.push(AttestationMatch { verify, attestation });
                    }
                    signed_refused.extend(outcome.refused);
                }
                // The other subject may still match, so the kind is spent only if every pass is empty.
                Err(kind @ (VerifyErrorKind::AttestationNotFound | VerifyErrorKind::NoSignaturesFound)) => {
                    scan_failure.get_or_insert(kind);
                }
                Err(kind) => return Err(kind),
            }
        }

        if matches.is_empty() {
            // Ladder: unsigned-attachment refusal, then `.sbom` probe fault, then the scan's own verdict.
            let kind = scan_failure.unwrap_or(VerifyErrorKind::AttestationNotFound);
            return Err(best_failure(unsigned_refused).or(sidecar_fault).unwrap_or(kind));
        }

        let mut refused = signed_refused;
        refused.extend(unsigned_refused);
        Ok(AttestationScan {
            matches,
            unverified: Vec::new(),
            refused,
            platform_subject,
        })
    }

    /// Read every SBOM the target carries, raw attachments and bundle payloads alike, **without verifying any of it**.
    // Only `run_attestations` may call this, never `run`, or an unverified document becomes a verification candidate.
    async fn scan_unverified(
        client: &Client,
        ctx: &VerifyContext<'_>,
        target: &ScanTarget,
        budget: &mut ScanBudget,
    ) -> Result<(Vec<UnverifiedSbom>, Vec<RefusedCandidate>), VerifyErrorKind> {
        let VerifyContentMode::Attestation { .. } = &ctx.content else {
            return Ok((Vec::new(), Vec::new()));
        };
        let transport = client.transport();
        let ScanTarget {
            image, subject_digest, ..
        } = target;

        // Unfiltered: a registry may ignore the server-side `artifactType` filter, so the client-side one below is real
        // (`adr_oci_referrers_signing_v1.md`, Amendment 10).
        let ReferrersListing {
            descriptors: listed,
            via,
        } = transport
            .list_referrers_with_fallback(image, subject_digest, None)
            .await
            .map_err(map_client_error)?;
        tracing::debug!("unverified SBOM referrers discovered via {via}");
        // No `--type` here: the listing's `artifactType` is unchecked against the manifest, so narrowing on it
        // misfilters.
        let mut candidates: Vec<(ocx_oci::Descriptor, UnverifiedPayload)> = listed
            .into_iter()
            .filter_map(|descriptor| {
                // Drop an untyped referrer, or it is listed under a predicate type nothing claimed.
                let artifact_type = descriptor.artifact_type.as_deref()?;
                if is_unsigned_sbom_artifact_type(artifact_type) {
                    return Some((descriptor, UnverifiedPayload::Raw));
                }
                (artifact_type == SIGSTORE_BUNDLE_V03).then_some((descriptor, UnverifiedPayload::Bundle))
            })
            .collect();
        // Digest order: a total order the registry does not choose.
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
                // Empty is a `--type` narrowing miss: a spent slot, not a failure.
                Ok(sboms) => found.extend(sboms),
                Err(reason) => refused.push(RefusedCandidate {
                    referrer_digest: descriptor.digest.clone(),
                    reason,
                }),
            }
        }

        // A refusal, not an error: raising would let attached junk turn a working listing into a hard failure.
        if let Some(stop) = budget.stop {
            refused.push(RefusedCandidate {
                referrer_digest: first_unexamined.unwrap_or_default(),
                reason: truncation_failure(budget.caps, stop, total_candidates.saturating_sub(processed)),
            });
        }

        // The `.sbom` sidecar tag, which no listing reaches: read even when the listing found SBOMs,
        // but never once a bound stopped the pass.
        if budget.stop.is_none() {
            budget.examined();
            match Self::read_sbom_sidecar_tag(transport, ctx, budget, target).await {
                Ok(sboms) => found.extend(sboms),
                Err((referrer_digest, reason)) => refused.push(RefusedCandidate {
                    referrer_digest: referrer_digest.map(|d| d.to_string()).unwrap_or_default(),
                    reason,
                }),
            }
        }
        Ok((found, refused))
    }

    /// Read **every** layer behind cosign's `sha256-<hex>.sbom` sidecar tag; empty for no tag or a `--type` miss.
    ///
    /// The error carries the manifest digest once learned, since a tag-addressed door has no descriptor to name.
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
            // Not a `ReferrerManifest`: its required `artifactType`/`subject` would reject cosign's own bytes.
            let manifest: ImageManifest =
                serde_json::from_slice(&manifest_bytes).map_err(|_| VerifyErrorKind::BundleParseFailed)?;
            // A present tag holding nothing is a refusal, not "no tag".
            if manifest.layers.is_empty() {
                return Err(VerifyErrorKind::NoUsableBundle);
            }
            // `Raw`: a `.sbom` tag never carries a bundle, so `Bundle` would have `parse_bundle` read an SBOM.
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

    /// Fetch one referrer and read **every** layer into an unverified document; empty is a `--type` miss.
    async fn read_unverified_referrer(
        transport: &dyn OciTransport,
        ctx: &VerifyContext<'_>,
        budget: &mut ScanBudget,
        target: &ScanTarget,
        descriptor: &ocx_oci::Descriptor,
        payload: UnverifiedPayload,
    ) -> Result<Vec<UnverifiedSbom>, VerifyErrorKind> {
        let referrer_digest =
            Digest::try_from(descriptor.digest.as_str()).map_err(|e| VerifyErrorKind::Internal(Box::new(e)))?;

        // The declared size is untrusted: only a pre-fetch reject, the body is re-checked after the read.
        if descriptor.size < 0 || descriptor.size as u64 > MAX_REFERRER_MANIFEST_BYTES {
            return Err(VerifyErrorKind::BundleParseFailed);
        }
        // Digest-only (`clone_with_digest` drops the tag): a `repo:tag@digest` reference 404s.
        let referrer_ref = target.image.clone_with_digest(descriptor.digest.clone());
        let referrer_bytes = match pull_referrer_manifest_capped(transport, &referrer_ref).await {
            Ok(bytes) => bytes,
            Err(kind) => {
                // Charge the cap on failure: the buffer is dropped, so a lying registry would otherwise pay one byte.
                budget.charge(MAX_REFERRER_MANIFEST_BYTES);
                return Err(kind);
            }
        };
        budget.charge(referrer_bytes.len() as u64);

        let manifest: ocx_oci::referrer::ReferrerManifest =
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

    /// Read each payload layer into a document, dropping `--type` misses; one layer's refusal refuses the manifest,
    /// so a partial list is never reported as complete.
    async fn read_every_layer(
        transport: &dyn OciTransport,
        ctx: &VerifyContext<'_>,
        budget: &mut ScanBudget,
        target: &ScanTarget,
        referrer_digest: &Digest,
        layers: &[ocx_oci::Descriptor],
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

    /// Turn one located payload layer into an unverified document; `Ok(None)` is a `--type` miss.
    async fn read_unverified_layer(
        transport: &dyn OciTransport,
        ctx: &VerifyContext<'_>,
        budget: &mut ScanBudget,
        target: &ScanTarget,
        referrer_digest: Digest,
        layer: &ocx_oci::Descriptor,
        payload: UnverifiedPayload,
    ) -> Result<Option<UnverifiedSbom>, VerifyErrorKind> {
        let caps = budget.caps;
        // The layer media type, never the listing's unchecked echo, gates and labels a raw attachment,
        // or an executable passes as an SBOM.
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
        // Narrow before the fetch, so a narrowed-out raw referrer costs no payload bytes.
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

    /// Read a Sigstore bundle's predicateType and predicate with **nothing verified**; `Ok(None)` is a `--type` miss.
    // Never carry signer identity out: an unverified SAN would sit in the column operators read as provenance.
    async fn extract_bundle_payload(
        ctx: &VerifyContext<'_>,
        bundle_bytes: Vec<u8>,
        target: &ScanTarget,
    ) -> Result<Option<(String, Vec<u8>)>, VerifyErrorKind> {
        let VerifyContentMode::Attestation { predicate_type } = &ctx.content else {
            return Ok(None);
        };
        // `spawn_blocking`: up to 32 MiB of JSON parsing with no await would stall the runtime worker.
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

    /// List the raw SBOM attachments on the target and refuse every one **without fetching any**, capped at the
    /// mode's candidate count.
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

        let ReferrersListing {
            descriptors: listed,
            via,
        } = transport
            .list_referrers_with_fallback(image, subject_digest, None)
            .await
            .map_err(map_client_error)?;
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
        // The `.sbom` tag is fetched on every `Demand` run: no listing reports it, so skipping hides an unsigned
        // sidecar.
        let mut sidecar_fault = None;
        match pull_sbom_sidecar_manifest(transport, image, subject_digest).await {
            Ok(Some((_, referrer_digest))) => digests.push(referrer_digest.to_string()),
            Ok(None) => {}
            // Deferred: a transient fault here must not fail a run the signed pass would pass.
            Err(kind) => sidecar_fault = Some(kind),
        }
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

    /// Scan the pinned subject, then the enclosing index (where cosign signs) when the membership proof holds.
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
        // The whole membership gate: read `enclosing_index` directly and unproven membership is assumed.
        let Some(index_digest) = target.index_signature_subject() else {
            return Err(subject_failure);
        };
        let index_target = ScanTarget {
            image: target.image.clone(),
            subject_digest: index_digest.clone(),
            // No further indirection: a nested index would need a membership proof nothing here has.
            enclosing_index: None,
            index_members: Vec::new(),
        };
        // One budget across both passes, or stuffing the platform manifest buys a second allowance.
        Self::scan(client, ctx, &index_target, arity, budget)
            .await
            .map_err(|_| subject_failure)
    }

    /// Resolve the target once: the trust-service SSRF floor, the subject digest, and the registry reference.
    async fn resolve_target(client: &Client, ctx: &VerifyContext<'_>) -> Result<ScanTarget, VerifyErrorKind> {
        // SSRF floor on where the Rekor URL resolves (CWE-918); skipped offline, or an air-gapped verify needs DNS.
        if !ctx.offline {
            ocx_oci::endpoint::resolve_sigstore_url(ctx.rekor_url, ctx.dial.trusted_hosts)
                .await
                .map_err(|error| VerifyErrorKind::InvalidEndpointUrl {
                    endpoint: "--rekor-url".into(),
                    reason: ocx_oci::endpoint::UrlRejection::from(error),
                })?;
        }

        // Through the index chain, never the registry transport, which bypasses the SSRF guard and mirror map.
        let ResolvedSubject {
            target: SignTarget {
                subject_digest,
                enclosing_index,
            },
            index_members,
            physical,
        } = (ctx.resolve)(ctx.identifier, ctx.platform).await?;
        // Transport calls target `physical`; trust policy scope stays on the logical identifier.
        let resolved = ctx.identifier.clone_with_digest(subject_digest.clone());
        // Fail-closed dial-site re-check: the resolve-time guard tolerates DNS failure and the client has no resolver.
        ocx_oci::ssrf::guard_physical_dial(&ctx.dial, &resolved, &physical)
            .await
            .map_err(|error| VerifyErrorKind::ForbiddenRegistryTarget {
                reason: error.to_string(),
            })?;
        // Every registry-facing reference goes through `transport_reference`, or `[mirrors]` stops applying.
        let image = client.transport_reference(&physical);
        Ok(ScanTarget {
            image,
            subject_digest,
            enclosing_index,
            index_members,
        })
    }

    /// List the target's signature referrer candidates and verify them under the requested content mode.
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

        // The pin decides what is discovered, never what is ignored after: a pinned shape never builds the other.
        let discover_bundles = ctx.signature_format != Some(SignatureFormat::Simplesigning);
        let discover_simplesigning = ctx.signature_format != Some(SignatureFormat::Bundle)
            && matches!(ctx.content, VerifyContentMode::Signature);
        let discover_attestation_sidecar = ctx.signature_format != Some(SignatureFormat::Bundle)
            && matches!(ctx.content, VerifyContentMode::Attestation { .. });

        // A server-side hint only; the client-side re-filter below is the real one.
        let server_filter = (!discover_simplesigning).then_some(SIGSTORE_BUNDLE_V03);
        let ReferrersListing {
            descriptors: referrers,
            via,
        } = Self::list_signature_referrers(transport, image, subject_digest, server_filter).await?;
        tracing::debug!("signature referrers discovered via {via}");
        let mut candidates: Vec<ocx_oci::Descriptor> = Vec::new();
        let mut sidecar_referrers: Vec<ocx_oci::Descriptor> = Vec::new();
        for descriptor in referrers {
            // Keep an absent artifactType: some registries omit the echo, and the bundle parse fail-closes anyway.
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
            // Before the trust-root gate: "not signed" is the thing to report, not a missing trust root.
            return Err(VerifyErrorKind::NoSignaturesFound);
        }
        order_candidates(&mut candidates, &ctx.content);

        // Refused up front, or a missing CT log key surfaces as an opaque SCT failure per candidate.
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
        // The Rekor configuration is inert: `sigstore` 0.14 never dials it.
        let verifier = Verifier::new(RekorConfiguration::default(), ctx.trust_root.clone()).map_err(|e| {
            VerifyErrorKind::TrustRootLoad(TrustRootLoadReason::AssetReadFailed {
                source: Box::new(std::io::Error::other(format!("trust root unusable: {e}"))),
            })
        })?;

        let anything_listed = !candidates.is_empty() || !sidecar_referrers.is_empty();
        let mut total_candidates = candidates.len();
        let mut refused: Vec<RefusedCandidate> = Vec::new();
        let mut matches: Vec<(VerifyResult, Option<VerifiedAttestation>)> = Vec::new();
        // Per run: a longer-lived memo would outlive the trust root its keys were resolved against.
        let rekor_keys = RekorKeyMemo::default();
        // ponytail: O(n^2) over <= 16 candidates; switch to a set if the cap grows.
        let mut seen: Vec<SignatureKey> = Vec::new();
        let caps = budget.caps;
        // The bytes, not the digest: `Verifier` hashes a preimage.
        let subject_bytes = if candidates.is_empty() {
            Vec::new()
        } else {
            pull_subject_manifest_verified(transport, image, subject_digest).await?
        };
        for descriptor in candidates {
            if !budget.may_examine() {
                break;
            }
            // The declared size is untrusted: only a pre-fetch reject, the body is re-checked after the read.
            if descriptor.size < 0 || descriptor.size as u64 > MAX_REFERRER_MANIFEST_BYTES {
                budget.examined();
                refused.push(RefusedCandidate {
                    referrer_digest: descriptor.digest.clone(),
                    reason: VerifyErrorKind::BundleParseFailed,
                });
                continue;
            }
            // Digest-only (`clone_with_digest` drops the tag): a `repo:tag@digest` reference 404s.
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
                // No refusal recorded, so the simplesigning fallback still fires beside an attestation-only bundle.
                Ok(CandidateOutcome::ModeMismatch) => budget.skipped_other_mode(),
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

        // `refused.is_empty()`: a refused bundle must exit with its own code, never be answered by a weaker sidecar.
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

        let mut examined_anything = anything_listed;
        // `stop_reason`, never `may_examine`, which stamps a truncation onto a run that spent exactly its slots;
        // kept before `refused` so `a_spent_last_candidate_slot_is_not_reported_as_a_truncation` can red.
        if discover_attestation_sidecar && matches.is_empty() && budget.stop_reason().is_none() && refused.is_empty() {
            // Without `refused.is_empty()` a tampered current bundle opens this door onto a stale signed `.att` at exit
            // 0.
            let predicate_type = match &ctx.content {
                VerifyContentMode::Attestation { predicate_type } => predicate_type.as_ref(),
                VerifyContentMode::Signature => None,
            };
            total_candidates = total_candidates.saturating_add(1);
            budget.examined();
            let subject_bytes = if subject_bytes.is_empty() {
                pull_subject_manifest_verified(transport, image, subject_digest).await?
            } else {
                subject_bytes
            };
            let remaining = caps.total_bytes.saturating_sub(budget.spent);
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
                // Carry the reader's stop across, or a truncated sidecar read reports a partial list as complete.
                if let Some(stop) = sidecar.stop {
                    budget.record_stop(stop);
                }
                refused.extend(sidecar.refused);
                // Dedup, or one layer descriptor repeated N times yields N rows off a single blob.
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
            return Err(VerifyErrorKind::NoSignaturesFound);
        }
        Self::finish_scan(ctx, caps, total_candidates, budget, matches, refused)
    }

    /// Scan both cosign sidecar doors (OCI 1.1 referrer, then the `.sig` tag).
    ///
    /// Returns the slots spent so `total_candidates` and `budget.considered` stay in step:
    /// `aggregate_failure` reads their difference as truncation.
    #[expect(
        clippy::too_many_arguments,
        reason = "both sidecar doors, their shared budget, and the run-scoped Rekor key memo"
    )]
    async fn scan_simplesigning(
        transport: &dyn OciTransport,
        ctx: &VerifyContext<'_>,
        verifier: &Verifier,
        target: &ScanTarget,
        referrers: Vec<ocx_oci::Descriptor>,
        via: DiscoveryMethod,
        budget: &mut ScanBudget,
        rekor_keys: &RekorKeyMemo,
    ) -> Result<(SidecarScan, usize), VerifyErrorKind> {
        let ScanTarget {
            image, subject_digest, ..
        } = target;
        let mut scan = SidecarScan::default();
        let mut examined = 0usize;
        let verify = simplesigning_read::SidecarVerification {
            verifier,
            policies: ctx.policies,
            trust_root: ctx.trust_root,
            rekor_url: ctx.rekor_url,
            offline: ctx.offline,
            allow_unlogged: ctx.allow_unlogged_signature,
            rekor_keys: rekor_keys.clone(),
        };

        // Door 1: the OCI 1.1 referrer, reported under the listing's `via`.
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

        // Door 2: the `sha256-<hex>.sig` tag. Only an absent tag is `Ok(None)`; any other transport
        // fault must propagate, or "could not finish looking" reads as "not signed".
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

    /// Turn a finished scan into its answer, or the one failure that best explains its absence.
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
            // An `All` scan arrives here with its matches: an unconditional `Err` would report
            // "no signatures found" about a subject that verified.
            VerifyContentMode::Signature if !matches.is_empty() => Ok(ScanOutcome { matches, refused }),
            VerifyContentMode::Signature => Err(aggregate_failure(
                total_candidates,
                budget.considered,
                best_failure(refused),
            )),
            // Truncation fails closed before any per-candidate failure: a partial list would
            // understate what the subject carries.
            VerifyContentMode::Attestation { .. } => {
                match budget.stop.map(|stop| truncation_failure(caps, stop, unexamined)) {
                    Some(kind) => Err(kind),
                    // A narrowing miss records no refusal, so nothing recorded is genuinely "not found".
                    None if matches.is_empty() => {
                        Err(best_failure(refused).unwrap_or(VerifyErrorKind::AttestationNotFound))
                    }
                    // Failing on a recorded refusal would let one malformed referrer hide every valid
                    // attestation beside it.
                    None => Ok(ScanOutcome { matches, refused }),
                }
            }
        }
    }

    /// Verify one signature-referrer candidate end-to-end from its fetched manifest bytes.
    ///
    /// Any failure is this candidate's verdict, which the ANY-of loop aggregates. `budget` is
    /// charged here for the bundle blob on success and failure alike, or total download is unbounded.
    #[expect(
        clippy::too_many_arguments,
        reason = "one candidate, its context, and the run-scoped material"
    )]
    async fn verify_one_referrer(
        transport: &dyn OciTransport,
        ctx: &VerifyContext<'_>,
        verifier: &Verifier,
        descriptor: &ocx_oci::Descriptor,
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

        let referrer_manifest: ocx_oci::referrer::ReferrerManifest =
            serde_json::from_slice(&referrer_bytes).map_err(|_| VerifyErrorKind::BundleParseFailed)?;
        let bundle_layer = referrer_manifest
            .layers
            .first()
            .ok_or(VerifyErrorKind::NoUsableBundle)?;
        let bundle_blob_digest =
            Digest::try_from(bundle_layer.digest.as_str()).map_err(|_| VerifyErrorKind::BundleParseFailed)?;

        // CWE-400: the declared size is untrusted, so reject it pre-fetch and also cap the read itself.
        let caps = ctx.content.caps();
        if bundle_layer.size < 0 || bundle_layer.size as u64 > caps.bundle_bytes as u64 {
            // Attestation mode names the tripped bound; "malformed referrer" would misdirect an SBOM author.
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
                // A rejected read still cost up to the cap (the bounded read stops at cap + 1).
                budget.charge(caps.bundle_bytes as u64);
                return Err(kind);
            }
        };

        // Blocking thread: up to 32 MiB of serde passes inline would starve the scan's sibling blob pulls.
        let bundle_cap = caps.bundle_bytes;
        let content = ctx.content.clone();
        let target_digest = subject_digest.clone();
        let parsed = tokio::task::spawn_blocking(move || {
            // The mode's cap, not the 512 KiB signature constant, or every larger SBOM is refused post-download.
            let bundle = parse_bundle(&bundle_bytes, bundle_cap).ok_or(VerifyErrorKind::BundleParseFailed)?;
            let parts = match BundleParts::from_bundle(&bundle, &content) {
                Ok(parts) => parts,
                // Content of the other mode, not a failure: charging it a slot lets attestations crowd out a signature.
                Err(VerifyErrorKind::NoUsableBundle) => return Ok(ParsedCandidate::ModeMismatch),
                Err(kind) => return Err(kind),
            };

            // Runs before the delegated call, or its one generic error hides these precise kinds.
            let envelope = match &content {
                // The predicate type is fixed here, so a mismatch is a refusal, never a narrowing miss.
                VerifyContentMode::Signature => {
                    let cosign_signature = PredicateType::Uri(COSIGN_SIGN_PREDICATE_TYPE.to_owned());
                    Some(dsse::verify_envelope(&bundle, &target_digest, Some(&cosign_signature))?)
                }
                VerifyContentMode::Attestation { predicate_type } => {
                    match dsse::verify_envelope(&bundle, &target_digest, predicate_type.as_ref()) {
                        Ok(verified) => Some(verified),
                        // A narrowing miss: sound, just not the requested document.
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
        // Logged: `Internal` ranks lowest in `failure_rank`, so any sibling refusal would hide the panic.
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

        // An unsigned annotation may order the scan, never decide it: a disagreement is a failure, not a
        // narrowing miss, or a registry rewriting it turns a signed SBOM into "none found".
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

        // Set on both arms only from an entry `verify_rekor_set` accepted.
        let mut rekor_log_index = None;
        let signer = match parts {
            BundleParts::Keyless { leaf_der, tlog } => {
                // `offline: true` skips no check only while `verify_rekor_set` keeps the inclusion proof mandatory.
                verifier
                    .verify(subject_bytes, bundle, &PolicyDeferredToOcx, true)
                    .await
                    .map_err(map_verification_error)?;

                let rekor_key_pem = verify_rekor_set(ctx, &tlog, rekor_keys).await?;
                // Only after the SET and inclusion proof passed, so a reported index is one the log vouched for.
                rekor_log_index = Some(tlog.log_index);

                // After the SET and Merkle checks, so the binding runs against the logged body.
                if let Some(verified) = verified_envelope.as_ref() {
                    dsse::verify_tlog_binding(
                        &tlog.canonicalized_body,
                        &verified.attestation.payload,
                        &verified.signatures,
                    )?;
                }

                // The matched subset, not a boolean: the builder pin is decided per satisfied policy.
                let matched_policies = matching_policies(&leaf_der, ctx.policies)?;

                if let Some(verified) = verified_envelope.as_ref() {
                    dsse::enforce_builder_pin(&matched_policies, &verified.attestation)?;
                }

                if !ctx.offline {
                    cache_trust_material(ctx, rekor_key_pem).await;
                }

                let cert = parse_certificate(&leaf_der)?;

                // CVE-2024-55655, in the tail both content modes share, over the leaf identity is read from.
                tlog::verify_integrated_time_within_certificate(
                    // Saturating is safe: the value is a widened non-negative i64, and i64::MAX fails closed.
                    SigningInstant::TransparencyLog(i64::try_from(tlog.integrated_time).unwrap_or(i64::MAX)),
                    &cert,
                )?;

                VerifiedSigner {
                    key_backend: KeyBackendKind::Keyless,
                    // Off the verified leaf, not merely one the bundle carried.
                    certificate_identity: subject_identity(&cert),
                    certificate_oidc_issuer: oidc_issuer(&cert),
                    signed_at: Some(tlog.integrated_time),
                }
            }

            // `sigstore::bundle::verify` reads its key only from a certificate, so key mode checks the DSSE
            // signature against a key the policy named instead.
            BundleParts::Key { hint, tlog } => {
                // Defensive floor: the key path has no second source of signature bytes.
                let envelope = verified_envelope.as_ref().ok_or(VerifyErrorKind::NoUsableBundle)?;
                let signature = &envelope
                    .signatures
                    .first()
                    .ok_or(VerifyErrorKind::SignatureInvalid)?
                    .sig;

                // The PAE, never the bare payload: a payload-only check is forgeable across payload types.
                let pae = crate::attest::dsse::pae(crate::attest::DSSE_PAYLOAD_TYPE, &envelope.attestation.payload);
                let matched_policies =
                    identity::matching_key_policies(&pae, signature, ctx.policies).inspect_err(|_| {
                        // The hint is unauthenticated and decides nothing; it is logged for the operator only.
                        tracing::debug!(
                            "no trusted key verified referrer {referrer_digest}; \
                             the bundle's public-key hint is {hint}"
                        );
                    })?;

                // Optional in key mode, but when present checked in full like the keyless arm.
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
                    // No logged instant exists, and none is invented.
                    None => None,
                };

                dsse::enforce_builder_pin(&matched_policies, &envelope.attestation)?;

                VerifiedSigner {
                    // Only local PEM keys are admitted today; a KMS backend must widen this.
                    key_backend: KeyBackendKind::File,
                    // No certificate, so the identity fields are absent rather than empty.
                    certificate_identity: None,
                    certificate_oidc_issuer: None,
                    signed_at,
                }
            }
        };

        // The dedup key: the verified DSSE signature, so two doors onto one bundle key identically.
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

    /// List the signature referrers for the subject, and how they were found.
    ///
    /// `artifact_type` is only a server-side hint: the caller must re-filter, since a registry may ignore it.
    async fn list_signature_referrers(
        transport: &dyn OciTransport,
        image: &native::Reference,
        subject_digest: &Digest,
        artifact_type: Option<&str>,
    ) -> Result<ReferrersListing, VerifyErrorKind> {
        // The fallback index is a mutable tag anyone with push access authors
        // (`adr_oci_referrers_signing_v1.md`, Amendment 10).
        transport
            .list_referrers_with_fallback(image, subject_digest, artifact_type)
            .await
            .map_err(map_client_error)
    }
}

/// The verification facts the two key models establish differently; named fields, since two
/// adjacent `Option<String>`s swap silently.
struct VerifiedSigner {
    key_backend: KeyBackendKind,
    certificate_identity: Option<String>,
    certificate_oidc_issuer: Option<String>,
    signed_at: Option<u64>,
}

/// The Rekor transparency evidence one bundle carries, already structurally checked.
struct BundleTlog {
    signed_entry_timestamp: Vec<u8>,
    canonicalized_body: Vec<u8>,
    integrated_time: u64,
    log_index: u64,
    log_id_hex: String,
    /// Not optional, so no caller can fall back to the SET alone.
    inclusion_proof: ProtoInclusionProof,
}

impl BundleTlog {
    /// Read one `tlogEntries` element, refusing a promise-only or proof-only entry.
    fn from_entry(
        entry: &sigstore_protobuf_specs::dev::sigstore::rekor::v1::TransparencyLogEntry,
    ) -> Result<Self, VerifyErrorKind> {
        let signed_entry_timestamp = entry
            .inclusion_promise
            .as_ref()
            .map(|promise| promise.signed_entry_timestamp.clone())
            .ok_or(VerifyErrorKind::RekorSetAbsentTsaPresent)?;

        // Mandatory though v0.1/v0.2 schemas allow omitting it, or a promise-only bundle verifies on weaker evidence.
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

/// The verification material a parsed bundle offers, and its transparency evidence.
///
/// An enum so an empty certificate and a keyless bundle without a tlog entry are unrepresentable.
enum BundleParts {
    /// A Fulcio leaf certificate (DER) plus its mandatory transparency evidence.
    Keyless { leaf_der: Vec<u8>, tlog: BundleTlog },
    /// No certificate; transparency evidence only if the signer uploaded one.
    Key {
        /// Unauthenticated, so it decides nothing; only logged on refusal.
        hint: String,
        tlog: Option<BundleTlog>,
    },
}

impl BundleParts {
    fn from_bundle(
        bundle: &sigstore_protobuf_specs::dev::sigstore::bundle::v1::Bundle,
        mode: &VerifyContentMode,
    ) -> Result<Self, VerifyErrorKind> {
        // Mode first: reading the material first charges a malformed other-kind bundle as this mode's failure.
        let content_matches_mode = match bundle.content.as_ref() {
            Some(bundle::Content::DsseEnvelope(envelope)) => {
                let image_signature = dsse::is_cosign_image_signature(envelope);
                match mode {
                    VerifyContentMode::Signature => image_signature,
                    VerifyContentMode::Attestation { .. } => !image_signature,
                }
            }
            // `messageSignature`, the pre-parity shape nothing here reads, is refused in both modes.
            _ => false,
        };
        if !content_matches_mode {
            return Err(VerifyErrorKind::NoUsableBundle);
        }

        let material = bundle
            .verification_material
            .as_ref()
            .ok_or(VerifyErrorKind::BundleParseFailed)?;
        // A malformed entry is refused under either key model; only absence differs.
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
            // An absent tlog entry must stay legal, or every offline `cosign sign --key` signature is refused.
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
/// Checks the SET and the Merkle inclusion proof, both mandatory. Offline with no pinned key
/// fails here as the backstop to the CLI's exit-78 gate.
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

    tlog::verify_inclusion(&key, &entry.inclusion_proof, &entry.canonicalized_body)?;
    Ok(pem)
}

/// The Rekor log public keys this verify run has resolved, so N candidates cost one fetch.
///
/// Keyed on `log_id_hex`, or a second log's SET is checked against the first log's key
/// (`the_rekor_key_is_selectable_by_log_id_and_falls_back_when_unknown` pins the selector).
/// Successes only: a cached `Err` would turn one flaky fetch into a whole-scan refusal.
#[derive(Clone, Default)]
pub struct RekorKeyMemo {
    /// `logId` hex → PEM. Never hold the `std::sync::Mutex` guard across the fetch's `.await`, or it deadlocks.
    resolved: Arc<Mutex<HashMap<String, String>>>,
}

impl RekorKeyMemo {
    /// The Rekor public key PEM for `log_id_hex`: memo, then pinned trust material, then an online
    /// fetch unless offline. The one ladder for both doors, so neither forgets the offline refusal.
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
    /// The cache holds one key per authority; writing either of two would hand a later offline verify the wrong one.
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
/// `pub` so the auto-verify hook fetches the key once per batch.
pub async fn fetch_rekor_public_key_pem(rekor_url: &Url) -> Result<String, VerifyErrorKind> {
    let endpoint = rekor_url
        .join("api/v1/log/publicKey")
        .map_err(|e| VerifyErrorKind::Internal(Box::new(e)))?;
    let response = ocx_oci::endpoint::sigstore_http_client()
        .get(endpoint)
        .send()
        .await
        .map_err(|_| VerifyErrorKind::TransparencyLogUnavailable)?;
    if !response.status().is_success() {
        return Err(VerifyErrorKind::TransparencyLogUnavailable);
    }
    // Capped: a PEM public key is under a kilobyte.
    let raw = ocx_oci::endpoint::read_body_capped(response)
        .await
        .ok_or(VerifyErrorKind::TransparencyLogUnavailable)?;
    String::from_utf8(raw).map_err(|_| VerifyErrorKind::TransparencyLogUnavailable)
}

/// Pull a referrer payload blob, reading at most `cap + 1` bytes (CWE-400).
///
/// `cap` is always the run's, never the candidate's declared size, which is untrusted.
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

/// Fetch a referrer manifest, rejecting a body over [`MAX_REFERRER_MANIFEST_BYTES`] before parsing.
///
/// The listed descriptor size is untrusted, so the actual body length is the bound that matters.
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

/// Whether a referrers-listing `artifactType` names an **unsigned** SBOM attachment.
///
/// Both conventions, since neither is the other's superset: the document's own media type, and
/// [`COSIGN_SBOM_ARTIFACT_TYPE`], without which every cosign OCI 1.1 SBOM referrer is dropped.
/// Admitting is not accepting: `read_unverified_layer` still gates on the layer's media type.
fn is_unsigned_sbom_artifact_type(artifact_type: &str) -> bool {
    sbom_predicate_type_uri(artifact_type).is_some() || artifact_type == COSIGN_SBOM_ARTIFACT_TYPE
}

/// Fetch the manifest and digest behind the `sha256-<hex>.sbom` sidecar tag, or `None` on a 404.
async fn pull_sbom_sidecar_manifest(
    transport: &dyn OciTransport,
    image: &native::Reference,
    subject_digest: &Digest,
) -> Result<Option<(Vec<u8>, Digest)>, VerifyErrorKind> {
    let target = sibling_tag_reference(image, ocx_oci::tag::sbom_sidecar_tag(subject_digest));
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
/// Not a `Result`: collapsing the skips into errors lets attestations crowd out a signature and
/// reports a missing SBOM as a data error.
#[derive(Debug)]
// Boxing `Verified` would allocate on the hot success path to save stack on two empty variants.
#[expect(
    clippy::large_enum_variant,
    reason = "the large variant is the hot one; see the note above"
)]
enum CandidateOutcome {
    /// Verified; `attestation` is `Some` iff the mode was [`VerifyContentMode::Attestation`].
    Verified {
        verified: VerifiedSignature,
        attestation: Option<VerifiedAttestation>,
    },
    /// The other content kind: costs bytes, never a candidate slot.
    ModeMismatch,
    /// Verified, but not the requested predicate type: a not-found, not a data error.
    TypeNarrowed,
}

/// What the blocking structural pass produced, before any crypto has run.
enum ParsedCandidate {
    /// Everything the crypto tail needs. Boxed because the skip variants are the common case.
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

/// What a scan produced: what verified, and what was examined and refused.
#[derive(Debug, Default)]
struct ScanOutcome {
    matches: Vec<(VerifyResult, Option<VerifiedAttestation>)>,
    refused: Vec<RefusedCandidate>,
}

/// Which referrer shape a candidate is, decided from its `artifactType` before any fetch.
///
/// Shape only: the unchecked `artifactType` must never decide the reported label, which comes from the layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnverifiedPayload {
    /// The document itself, typed by the layer's own media type.
    Raw,
    /// A Sigstore bundle, read for its DSSE payload with nothing verified.
    Bundle,
}

/// The subject one scan runs against, resolved once.
///
/// One value, so the right host is never addressed for the wrong subject by a transposition.
struct ScanTarget {
    /// Registry reference every referrer-facing call is addressed with.
    image: native::Reference,
    /// The per-platform subject manifest digest.
    subject_digest: Digest,
    /// The image-index digest [`subject_digest`](Self::subject_digest) was reached through.
    ///
    /// `None` when membership cannot be proved (no index, or an unfetchable one), which fails closed.
    enclosing_index: Option<Digest>,
    /// Every digest the enclosing index lists. Empty when there is no index.
    ///
    /// Never derive membership from the selection: `resolve_sign_target` makes no validity decision.
    index_members: Vec<Digest>,
}

impl ScanTarget {
    /// The digest whose signatures may also count for this subject, or `None` when they may not.
    ///
    /// An index signature covers this subject only once it is found among
    /// [`index_members`](Self::index_members); never assume membership.
    fn index_signature_subject(&self) -> Option<&Digest> {
        let enclosing = self.enclosing_index.as_ref()?;
        self.index_members.contains(&self.subject_digest).then_some(enclosing)
    }
}

/// Which bound ended a scan before its candidates ran out.
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
/// A seam for the silent regression: an other-kind candidate must never consume this mode's slot.
struct ScanBudget {
    caps: ContentCaps,
    /// Candidates examined in the requested mode; the candidate cap bounds this.
    examined: usize,
    /// Candidates processed at all, mode-mismatched ones included; truncation is read from this.
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
    /// Charged from bytes read, never a declared size, or advertising size 0 buys extra fetches.
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
    /// Probe with this, not [`Self::may_examine`], whose stamp `finish_scan` reads as a truncation.
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
    /// Without it a truncated `.att` scan, bounded by its own caps, returns its partial list as success.
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
    /// Never touches [`Self::examined`], or attestations push a valid signature past the candidate cap.
    fn skipped_other_mode(&mut self) {
        self.considered = self.considered.saturating_add(1);
    }
}

/// Put the candidate list in scan order: two stable sorts, lexicographic in (disagrees-with-mode, digest).
///
/// Digest order keeps the verdict reproducible; the hint demotes other-kind candidates, or SBOMs push a
/// signature past [`MAX_SIGNATURE_CANDIDATES`]. The hint is untrusted, so this orders and never filters.
fn order_candidates(candidates: &mut [ocx_oci::Descriptor], mode: &VerifyContentMode) {
    candidates.sort_by(|a, b| a.digest.cmp(&b.digest));
    candidates.sort_by_key(|descriptor| annotation_disagrees_with_mode(descriptor, mode));
}

/// Whether a listed referrer's annotations positively name something other than what this
/// run is looking for.
///
/// An absent annotation is `false`, so a tool that writes no hint is not demoted behind one that does.
fn annotation_disagrees_with_mode(descriptor: &ocx_oci::Descriptor, mode: &VerifyContentMode) -> bool {
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
fn truncation_failure(caps: ContentCaps, stop: ScanStop, unexamined: usize) -> VerifyErrorKind {
    match stop {
        ScanStop::CandidateCap => VerifyErrorKind::TooManyAttestations { limit: caps.candidates },
        ScanStop::ByteBudget => VerifyErrorKind::AttestationBudgetExhausted {
            limit: caps.total_bytes,
        },
        ScanStop::ListingCap => VerifyErrorKind::CandidateLimitExhausted { unexamined },
    }
}

/// Pick the most actionable failure across the refused candidates (see [`failure_rank`]).
fn best_failure(refused: Vec<RefusedCandidate>) -> Option<VerifyErrorKind> {
    // Not `max_by_key`: it returns the last maximum, reordering which of two equal-ranked refusals is shown.
    refused
        .into_iter()
        .min_by_key(|candidate| std::cmp::Reverse(failure_rank(&candidate.reason)))
        .map(|candidate| candidate.reason)
}

/// Decide the aggregate ANY-of failure once no candidate has passed.
///
/// Unexamined candidates report [`VerifyErrorKind::CandidateLimitExhausted`], since a valid signature may
/// sort past the cap and an examined candidate's error would misattribute the failure.
fn aggregate_failure(total: usize, examined: usize, best: Option<VerifyErrorKind>) -> VerifyErrorKind {
    let unexamined = total.saturating_sub(examined);
    if unexamined > 0 {
        return VerifyErrorKind::CandidateLimitExhausted { unexamined };
    }
    best.unwrap_or(VerifyErrorKind::NoSignaturesFound)
}

/// Rank verify failures for the aggregate error; higher is more actionable.
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

/// Cache the trust material a **simplesigning** verify used, at the exits the bundle path caches at.
///
/// Reads the key from the run's memo; silent when it resolved none or two ([`RekorKeyMemo::single_key`]).
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
/// Best-effort: a cache-write failure never fails a valid verify.
async fn cache_trust_material(ctx: &VerifyContext<'_>, rekor_key_pem: String) {
    let cache_key = super::trust_cache::cache_key_for_rekor(ctx.rekor_url);
    let der_certs = ctx.trust_root.der_certs().to_vec();
    // Without the CT log keys, an offline verify off this entry cannot check the SCT.
    let ctfe_keys = ctx.trust_root.ctfe_key_map().clone();

    // Skip a content-equal rewrite, or every use slides the 24h TTL and a batch stampedes the file.
    if let Ok(Some(existing)) = TrustRootCache::from_cache(&cache_key, &ctx.state).await
        && existing.fulcio_der_certs == der_certs
        && existing.ctfe_keys == ctfe_keys
        && existing.rekor_public_key_pem.as_deref() == Some(rekor_key_pem.as_str())
    {
        return;
    }

    let entry = TrustRootCache::new(cache_key, der_certs, ctfe_keys, rekor_key_pem);
    if let Err(e) = entry.write_cache(&ctx.state).await {
        tracing::debug!("trust-root cache write skipped: {e}");
    }
}

/// Fetch the subject manifest and prove it hashes to the resolved digest, or a registry can swap
/// in a different artifact for the verifier.
async fn pull_subject_manifest_verified(
    transport: &dyn OciTransport,
    image: &native::Reference,
    subject_digest: &Digest,
) -> Result<Vec<u8>, VerifyErrorKind> {
    // Digest-addressed, not whatever the tag points at now.
    let pinned = image.clone_with_digest(subject_digest.to_string());
    let (bytes, _) = transport
        .pull_manifest_raw(&pinned, ACCEPTED_MANIFEST_TYPES)
        .await
        .map_err(map_client_error)?;
    // No size cap: the body is already allocated, so a cap would refuse only a genuine oversized manifest.
    let actual = subject_digest.algorithm().hash(&bytes);
    if !actual.hex().eq_ignore_ascii_case(subject_digest.hex()) {
        return Err(VerifyErrorKind::SubjectDigestMismatch);
    }
    Ok(bytes)
}

/// A `sigstore` verification policy that accepts every certificate, because ocx
/// enforces identity itself in [`matching_policies`].
///
/// Safe only while `matching_policies` runs unconditionally on the same leaf afterwards.
pub(super) struct PolicyDeferredToOcx;

impl VerificationPolicy for PolicyDeferredToOcx {
    fn verify(&self, _cert: &x509_cert::Certificate) -> PolicyResult {
        Ok(())
    }
}

/// Map a `sigstore` verification failure into the ocx verify taxonomy.
pub(super) fn map_verification_error(error: sigstore::bundle::verify::VerificationError) -> VerifyErrorKind {
    use sigstore::bundle::verify::VerificationError as E;
    match error {
        E::Bundle(_) => VerifyErrorKind::BundleParseFailed,
        E::Certificate(_) => VerifyErrorKind::CertChainInvalid,
        // Also a tlog-body splice (GHSA-whqx): `sigstore` cannot tell the two apart, so it reports as
        // `signature_invalid`.
        E::Signature(_) => VerifyErrorKind::SignatureInvalid,
        // Unreachable: `PolicyDeferredToOcx` never rejects and the input is a slice.
        E::Policy(_) | E::Input(_) => VerifyErrorKind::Internal(Box::new(error)),
    }
}

/// Decide what a verify run acts on, given a resolution outcome.
///
/// No sha256 floor, unlike signing: verify must keep reading whatever a registry holds.
///
/// # Errors
///
/// [`VerifyErrorKind::TargetNotFound`] when the reference resolves to nothing,
/// or to an index with no single compatible child;
/// [`VerifyErrorKind::TargetNotAnIndex`] when a platform was requested and the
/// resolution is a bare manifest.
pub fn verify_target_from_resolution(
    resolved: Option<&(Digest, ocx_oci::Manifest)>,
    platform: Option<&Platform>,
) -> Result<(SignTarget, Vec<Digest>), VerifyErrorKind> {
    let Some((resolved_digest, manifest)) = resolved else {
        return Err(VerifyErrorKind::TargetNotFound {
            platform: platform_label(platform),
        });
    };
    let members = ocx_oci::resolve_target::index_members(manifest);
    let target = SignTarget::from_resolved(resolved_digest, manifest, platform).map_err(map_resolve_target_error)?;
    Ok((target, members))
}

/// How a `--platform` request reads in an error message; `any` when none was made.
fn platform_label(platform: Option<&Platform>) -> String {
    platform.map_or_else(|| "any".to_string(), Platform::to_string)
}

/// Map the shared `--platform` decision's refusals into the verify taxonomy.
fn map_resolve_target_error(error: ResolveTargetError) -> VerifyErrorKind {
    match error {
        ResolveTargetError::NotAnIndex { platform } => VerifyErrorKind::TargetNotAnIndex { platform },
        ResolveTargetError::PlatformNotFound { platform } | ResolveTargetError::AmbiguousPlatform { platform } => {
            VerifyErrorKind::TargetNotFound { platform }
        }
    }
}

/// Map an OCI client error into the verify taxonomy.
pub fn map_client_error(error: ClientError) -> VerifyErrorKind {
    match error {
        // Keep this arm: the catch-all would reclassify it to exit 1. Verify answers 79; 84 is write-path only.
        ClientError::ReferrersUnsupported { .. } => VerifyErrorKind::NoSignaturesFound,
        ClientError::ManifestNotFound(_) | ClientError::BlobNotFound { .. } => VerifyErrorKind::NoSignaturesFound,
        // Malformed signature data: exit 65, not the catch-all's exit 1.
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

    use ocx_oci::testing::{RecordingTransport, SbomTransport};
    use p256::ecdsa::SigningKey;
    use p256::elliptic_curve::rand_core::OsRng;
    use sigstore_protobuf_specs::dev::sigstore::bundle::v1::Bundle;
    use sigstore_protobuf_specs::dev::sigstore::common::v1::{
        HashAlgorithm, HashOutput, LogId, MessageSignature, X509Certificate, X509CertificateChain,
    };
    use sigstore_protobuf_specs::dev::sigstore::rekor::v1::{InclusionPromise, TransparencyLogEntry};

    fn verify_id() -> PackageRef {
        PackageRef::parse("registry.example/pkg:1.0").expect("parse test identifier")
    }

    /// [`verify_id`], routed as the physical transport identifier — these
    /// fixtures never model an index rewrite, so identity routing is exact.
    fn verify_physical() -> ocx_oci::OciIdentifier {
        ocx_oci::OciIdentifier::passthrough(&verify_id())
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
            media_type: crate::sign::bundle::BUNDLE_V03_MEDIA_TYPE.to_string(),
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
            payload_type: crate::attest::DSSE_PAYLOAD_TYPE.to_string(),
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
        use ocx_oci::client::test_transport::{StubTransport, StubTransportData};

        let blob = vec![7u8; 4096];
        let blob_digest = ocx_oci::Algorithm::Sha256.hash(&blob);
        // One byte over the cap, so the bounded read rejects it after paying for
        // it — the failure path, which must charge the cap rather than nothing.
        let oversize_digest = ocx_oci::Algorithm::Sha256.hash(b"lying-descriptor");
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
        let subject_digest = ocx_oci::Algorithm::Sha256.hash(b"subject");
        // A real CA cert: `Verifier::new` compiles the trust root into a
        // certificate pool and rejects the placeholder DER the routing tests use.
        let ca_der = super::super::tlog::fixture_certificate_der();
        let trust_root = trust_root_of(&[&ca_der]);
        let verifier = Verifier::new(RekorConfiguration::default(), trust_root.clone()).expect("verifier");
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = SigningStatePaths::new(temp.path());
        let dial = TestDial::new();
        let identifier = PackageRef::parse("ocx.sh/acme/tool:1.0").expect("logical identifier");
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let ctx = VerifyContext {
            identifier: &identifier,
            platform: None,
            policies: &[],
            no_cache: true,
            dial: dial.policy(),
            resolve: indirecting_resolver(PackageRef::parse("registry.example/repo:1.0").expect("physical identifier")),
            trust_root: &trust_root,
            rekor_url: &rekor_url,
            state: state.clone(),
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
            let payload = ocx_oci::Descriptor {
                media_type: SIGSTORE_BUNDLE_V03.to_string(),
                digest: digest.to_string(),
                size,
                ..ocx_oci::Descriptor::default()
            };
            let subject = ocx_oci::Descriptor {
                media_type: ocx_oci::OCI_IMAGE_MEDIA_TYPE.to_string(),
                digest: subject_digest.to_string(),
                size: 7,
                ..ocx_oci::Descriptor::default()
            };
            let manifest = ocx_oci::referrer::ReferrerManifest::build(subject, SIGSTORE_BUNDLE_V03, payload, None);
            let bytes = manifest.to_canonical_json().expect("referrer manifest serializes");
            let descriptor = ocx_oci::Descriptor {
                media_type: ocx_oci::OCI_IMAGE_MEDIA_TYPE.to_string(),
                digest: ocx_oci::Algorithm::Sha256.hash(&bytes).to_string(),
                size: bytes.len() as i64,
                ..ocx_oci::Descriptor::default()
            };
            (descriptor, bytes)
        };

        let mut budget = ScanBudget::new(ctx.content.caps());
        let verify_once = async |descriptor: &ocx_oci::Descriptor, bytes: Vec<u8>, budget: &mut ScanBudget| {
            VerifyPipeline::verify_one_referrer(
                &transport,
                &ctx,
                &verifier,
                descriptor,
                bytes,
                &subject_digest,
                b"subject",
                &image,
                crate::verify::DiscoveryMethod::ReferrersApi,
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
        use ocx_oci::client::test_transport::{StubTransport, StubTransportData};

        let data = StubTransportData::new();
        let honest = b"a genuine under-cap sigstore bundle payload".to_vec();
        let honest_digest = ocx_oci::Algorithm::Sha256.hash(&honest);
        // One byte over the cap: the stub keys blobs by digest string, so the
        // digest need not match the (deliberately oversized) content.
        let oversize = vec![0u8; MAX_BUNDLE_SIZE_BYTES + 1];
        let oversize_digest = ocx_oci::Algorithm::Sha256.hash(b"lying-descriptor");
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
        use ocx_oci::client::test_transport::{StubTransport, StubTransportData};

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
        ocx_oci::Algorithm::Sha256.hash(INDIRECTION_SUBJECT_MANIFEST)
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
        let payload = ocx_oci::Descriptor {
            media_type: crate::sign::bundle::BUNDLE_V03_MEDIA_TYPE.to_string(),
            digest: ocx_oci::Algorithm::Sha256.hash(STUB_BUNDLE_BLOB).to_string(),
            size: STUB_BUNDLE_BLOB.len() as i64,
            ..ocx_oci::Descriptor::default()
        };
        let subject = ocx_oci::Descriptor {
            media_type: ocx_oci::OCI_IMAGE_MEDIA_TYPE.to_string(),
            digest: indirection_subject_digest().to_string(),
            size: 2,
            ..ocx_oci::Descriptor::default()
        };
        ocx_oci::referrer::ReferrerManifest::build(subject, SIGSTORE_BUNDLE_V03, payload, None)
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
        use ocx_oci::client::test_transport::{StubTransport, StubTransportData};

        let honest = br#"{"schemaVersion":2,"config":{},"layers":[]}"#.to_vec();
        let honest_digest = ocx_oci::Algorithm::Sha256.hash(&honest);
        // Same digest key, different body: exactly the substitution the re-hash
        // exists to catch.
        let substituted_digest = ocx_oci::Algorithm::Sha256.hash(b"the manifest the index resolved");

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
        let (outcome, calls, _state) =
            drive_verify_with_trust_root("8.8.8.8/acme/tool:1.0", ocx_oci::client::MirrorMap::default(), keyless).await;
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
        // Refused before the subject preimage is fetched: the guard is there to
        // stop per-candidate work, so reaching the manifest pull would mean it
        // sits in the wrong place.
        assert!(
            !calls.iter().any(|call| call.starts_with("pull_blob_streaming")),
            "the run must not reach signature material, got: {calls:?}",
        );
    }

    /// The resolution the indirecting fixture stands for: the logical name
    /// resolves to a bare image manifest at `indirection_subject_digest()`, and
    /// the subject rewrites onto a DIFFERENT physical registry — the
    /// `index.ocx.sh` shape (`ocx.sh/<ns>/<pkg>` pointing at
    /// `oci://8.8.8.8/<org>/<repo>`) reduced to what the pipeline consumes.
    ///
    /// An `IndexImpl` double stood here until inversion 1.9 took the index off
    /// the pipeline; the answer it produced is unchanged, and it now arrives
    /// through the seam the caller supplies.
    fn indirecting_resolver<'a>(physical: PackageRef) -> Box<VerifySubjectResolver<'a>> {
        resolving_resolver(
            physical,
            Some((
                indirection_subject_digest(),
                ocx_oci::Manifest::Image(ocx_oci::ImageManifest::default()),
            )),
        )
    }

    /// A resolver that answers one resolution, so a test says exactly what the
    /// reference resolved to: an image index and its children, a bare image
    /// manifest, or nothing at all.
    ///
    /// The taxonomy is applied by the same helper the real resolver calls, so a
    /// fixture cannot disagree with production about what a resolution means.
    fn resolving_resolver<'a>(
        physical: PackageRef,
        resolved: Option<(Digest, ocx_oci::Manifest)>,
    ) -> Box<VerifySubjectResolver<'a>> {
        Box::new(move |_identifier, platform| {
            let physical = ocx_oci::OciIdentifier::passthrough(&physical);
            let resolved = resolved.clone();
            Box::pin(async move {
                let (target, index_members) = verify_target_from_resolution(resolved.as_ref(), platform)?;
                Ok(ResolvedSubject {
                    target,
                    index_members,
                    physical,
                })
            })
        })
    }

    /// Owns the borrows a [`DialPolicy`] needs. Every unit test here runs with
    /// no `insecure` host and no `trusted_hosts` entry — what an unconfigured
    /// registry looks like, and what the `IndexImpl` doubles these fixtures
    /// used to hand the pipeline answered.
    struct TestDial(std::sync::Arc<ocx_oci::ssrf::ProxyRules>);

    impl TestDial {
        fn new() -> Self {
            Self(ocx_oci::ssrf::proxy_rules())
        }

        fn policy(&self) -> DialPolicy<'_> {
            DialPolicy {
                insecure_hosts: &[],
                trusted_hosts: &[],
                rules: &self.0,
            }
        }
    }

    /// The shared recording transport, configured the way this pipeline's
    /// indirection tests need it: the subject manifest under its own digest
    /// (the pipeline re-hashes it, so it must be the real preimage), a
    /// signature referrer for every other read, and a junk bundle blob — so a
    /// run walks the full read chain (probe -> list -> referrer manifest ->
    /// bundle blob) and ends in `BundleParseFailed`. Verify never writes, and
    /// the transport refuses a push rather than quietly accepting one.
    fn recording_transport() -> RecordingTransport {
        let referrer = stub_referrer_manifest();
        RecordingTransport::default()
            .serving_subject_digest(indirection_subject_digest().to_string())
            .serving_subject_manifest(INDIRECTION_SUBJECT_MANIFEST)
            .serving_manifest(&referrer)
            .serving_blob(STUB_BUNDLE_BLOB)
            .serving_referrers(vec![ocx_oci::Descriptor {
                media_type: ocx_oci::OCI_IMAGE_MEDIA_TYPE.to_string(),
                digest: ocx_oci::Algorithm::Sha256.hash(&referrer).to_string(),
                size: referrer.len() as i64,
                artifact_type: Some(SIGSTORE_BUNDLE_V03.to_string()),
                ..ocx_oci::Descriptor::default()
            }])
            .refusing_pushes()
    }

    /// Drive a verify run against the recording transport for the logical name
    /// `ocx.sh/acme/tool:1.0`, indirected to the physical `8.8.8.8/acme/tool:1.0`.
    /// Returns the `"<method>:<registry>"` log plus the state dir, so a caller
    /// can read back the persisted capability record. The transport serves one
    /// referrer whose bundle blob is junk, so the run walks the full read chain
    /// (probe → list → referrer manifest → bundle blob) and ends in
    /// `BundleParseFailed`.
    async fn run_recorded_verify(mirrors: ocx_oci::client::MirrorMap) -> (Vec<String>, tempfile::TempDir) {
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
        mirrors: ocx_oci::client::MirrorMap,
    ) -> (Result<Vec<VerifyResult>, VerifyError>, Vec<String>, tempfile::TempDir) {
        let (_key, cert) = self_signed_cert();
        drive_verify_with_trust_root(physical, mirrors, trust_root_of(&[&cert])).await
    }

    /// `drive_verify_at` with the trust root made an argument, so a test can
    /// drive the run with material the pipeline is expected to refuse.
    async fn drive_verify_with_trust_root(
        physical: &str,
        mirrors: ocx_oci::client::MirrorMap,
        trust_root: TrustRoot,
    ) -> (Result<Vec<VerifyResult>, VerifyError>, Vec<String>, tempfile::TempDir) {
        let logical = PackageRef::parse("ocx.sh/acme/tool:1.0").expect("logical identifier");
        let physical = PackageRef::parse(physical).expect("physical identifier");

        let transport = recording_transport();
        let mut client = Client::with_transport(Box::new(transport.clone()));
        client.set_mirrors(mirrors);
        let dial = TestDial::new();
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = SigningStatePaths::new(temp.path());
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
                dial: dial.policy(),
                resolve: indirecting_resolver(physical.clone()),
                trust_root: &trust_root,
                rekor_url: &rekor_url,
                state: state.clone(),
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
        let (calls, _state_dir) = run_recorded_verify(ocx_oci::client::MirrorMap::default()).await;
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
        let mirrors = ocx_oci::client::MirrorMap::new([(
            "8.8.8.8".to_string(),
            ocx_oci::client::mirror_map::ParsedMirror {
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
        let (outcome, calls, _state) =
            drive_verify_at("169.254.169.254/acme/tool:1.0", ocx_oci::client::MirrorMap::default()).await;
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
        identifier: &'a PackageRef,
        resolve: Box<VerifySubjectResolver<'a>>,
        dial: DialPolicy<'a>,
        trust_root: &'a TrustRoot,
        rekor_url: &'a Url,
        state: SigningStatePaths,
    ) -> VerifyContext<'a> {
        VerifyContext {
            identifier,
            // `IndirectingIndex` resolves to a bare image manifest, so there is
            // nothing to narrow into: C-010's "act on whatever resolved".
            platform: None,
            policies: &[],
            no_cache: true,
            dial,
            resolve,
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
        let dial = TestDial::new();
        let trust_root = trust_root_of(&[]);
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = SigningStatePaths::new(temp.path());
        let ctx = attestation_ctx(
            &identifier,
            indirecting_resolver(identifier.clone()),
            dial.policy(),
            &trust_root,
            &rekor_url,
            state.clone(),
        );
        let caps = ctx.content.caps();

        let verified = || {
            (
                VerifyResult {
                    subject_digest: ocx_oci::Algorithm::Sha256.hash(b"subject"),
                    referrer_digest: ocx_oci::Algorithm::Sha256.hash(b"referrer"),
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

        for (stop, _expected) in [
            (ScanStop::CandidateCap, "too_many_attestations"),
            (ScanStop::ByteBudget, "attestation_budget_exhausted"),
            (ScanStop::ListingCap, "candidate_limit_exhausted"),
        ] {
            let mut budget = ScanBudget::new(caps);
            budget.stop = Some(stop);
            budget.considered = 3;
            let outcome = VerifyPipeline::finish_scan(&ctx, caps, 9, &budget, vec![verified()], Vec::new());
            let _error = outcome.expect_err("a truncated scan never returns a partial list");
        }
    }

    /// An untruncated scan that verified nothing and recorded no defect is
    /// genuinely not-found (exit 79), not a data error. A narrowing miss —
    /// `--type` asked for something this artifact does not carry — records no
    /// failure at all, which is what makes this the reachable outcome for it.
    #[test]
    fn an_untruncated_attestation_scan_with_nothing_recorded_is_not_found() {
        let identifier = verify_id();
        let dial = TestDial::new();
        let trust_root = trust_root_of(&[]);
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = SigningStatePaths::new(temp.path());
        let ctx = attestation_ctx(
            &identifier,
            indirecting_resolver(identifier.clone()),
            dial.policy(),
            &trust_root,
            &rekor_url,
            state.clone(),
        );
        let caps = ctx.content.caps();

        let outcome = VerifyPipeline::finish_scan(&ctx, caps, 2, &ScanBudget::new(caps), Vec::new(), Vec::new());
        assert!(matches!(outcome, Err(VerifyErrorKind::AttestationNotFound)));

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
        let dial = TestDial::new();
        let trust_root = trust_root_of(&[]);
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = SigningStatePaths::new(temp.path());
        let ctx = attestation_ctx(
            &identifier,
            indirecting_resolver(identifier.clone()),
            dial.policy(),
            &trust_root,
            &rekor_url,
            state.clone(),
        );
        let caps = ctx.content.caps();

        let matches: Vec<(VerifyResult, Option<VerifiedAttestation>)> = (0..3u8)
            .map(|n| {
                (
                    VerifyResult {
                        subject_digest: ocx_oci::Algorithm::Sha256.hash(b"subject"),
                        referrer_digest: ocx_oci::Algorithm::Sha256.hash([n]),
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
        let dial = TestDial::new();
        let trust_root = trust_root_of(&[]);
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = SigningStatePaths::new(temp.path());
        let ctx = attestation_ctx(
            &identifier,
            indirecting_resolver(identifier.clone()),
            dial.policy(),
            &trust_root,
            &rekor_url,
            state.clone(),
        );
        let caps = ctx.content.caps();

        let passing = (
            VerifyResult {
                subject_digest: ocx_oci::Algorithm::Sha256.hash(b"subject"),
                referrer_digest: ocx_oci::Algorithm::Sha256.hash(b"good"),
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
    }

    /// The aggregate failure keeps the first strictly-highest-ranked refusal, so
    /// two equally-ranked refusals resolve in listing order rather than by
    /// whichever the fold happened to see last.
    #[test]
    fn the_aggregate_failure_is_the_first_most_actionable_refusal() {
        // `SignatureInvalid` outranks `BundleParseFailed`, and the two
        // `SignatureInvalid`s tie — the first must win.
        let _picked = best_failure(vec![
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
        assert!(best_failure(Vec::new()).is_none(), "nothing refused, nothing to report");
    }

    /// The signature arm is untouched by all of the above: a `FirstMatch` scan
    /// that found nothing still aggregates today's failure, over the candidates
    /// actually looked at.
    #[test]
    fn the_signature_arm_still_aggregates_its_failure() {
        let identifier = verify_id();
        let dial = TestDial::new();
        let trust_root = trust_root_of(&[]);
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = SigningStatePaths::new(temp.path());
        let ctx = VerifyContext {
            identifier: &identifier,
            platform: None,
            policies: &[],
            no_cache: true,
            dial: dial.policy(),
            resolve: indirecting_resolver(identifier.clone()),
            trust_root: &trust_root,
            rekor_url: &rekor_url,
            state: state.clone(),
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
    fn listed(digest: &str, hint: Option<&str>) -> ocx_oci::Descriptor {
        ocx_oci::Descriptor {
            media_type: ocx_oci::OCI_IMAGE_MEDIA_TYPE.to_string(),
            digest: digest.to_string(),
            size: 512,
            annotations: hint.map(|hint| {
                std::collections::BTreeMap::from([(ANNOTATION_BUNDLE_CONTENT.to_string(), hint.to_string())])
            }),
            ..ocx_oci::Descriptor::default()
        }
    }

    fn digests(candidates: &[ocx_oci::Descriptor]) -> Vec<&str> {
        candidates.iter().map(|candidate| candidate.digest.as_str()).collect()
    }

    /// [`listed`] plus the `dev.sigstore.bundle.predicateType` hint — the
    /// annotation that tells a signature from an attestation now that both are
    /// `dsse-envelope`.
    fn listed_typed(digest: &str, content: &str, predicate_type: &str) -> ocx_oci::Descriptor {
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
            let mut candidates: Vec<ocx_oci::Descriptor> = (0..MAX_SIGNATURE_CANDIDATES)
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
    ) -> (ocx_oci::Descriptor, Vec<u8>) {
        let payload = ocx_oci::Descriptor {
            media_type: SIGSTORE_BUNDLE_V03.to_string(),
            digest: blob_digest.to_string(),
            size: blob_size,
            ..ocx_oci::Descriptor::default()
        };
        let subject = ocx_oci::Descriptor {
            media_type: ocx_oci::OCI_IMAGE_MEDIA_TYPE.to_string(),
            digest: subject_digest.to_string(),
            size: 7,
            ..ocx_oci::Descriptor::default()
        };
        let annotations = annotation.map(|predicate_type| {
            std::collections::BTreeMap::from([(
                ANNOTATION_BUNDLE_PREDICATE_TYPE.to_string(),
                predicate_type.to_string(),
            )])
        });
        let manifest = ocx_oci::referrer::ReferrerManifest::build(subject, SIGSTORE_BUNDLE_V03, payload, annotations);
        let bytes = manifest.to_canonical_json().expect("referrer manifest serializes");
        let descriptor = ocx_oci::Descriptor {
            media_type: ocx_oci::OCI_IMAGE_MEDIA_TYPE.to_string(),
            digest: ocx_oci::Algorithm::Sha256.hash(&bytes).to_string(),
            size: bytes.len() as i64,
            ..ocx_oci::Descriptor::default()
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
            payload_type: crate::attest::DSSE_PAYLOAD_TYPE.to_string(),
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
        use ocx_oci::client::test_transport::{StubTransport, StubTransportData};

        const SIGNED: &str = "https://cyclonedx.org/bom";
        const CYCLONEDX_PREDICATE: &str = r#"{"bomFormat":"CycloneDX"}"#;
        const REWRITTEN: &str = "https://slsa.dev/provenance/v1";

        let subject_bytes = b"the artifact under attestation";
        let subject_digest = ocx_oci::Algorithm::Sha256.hash(subject_bytes);
        let blob = serde_json::to_vec(&dsse_bundle_binding(&subject_digest, SIGNED, CYCLONEDX_PREDICATE))
            .expect("bundle serializes");
        let blob_digest = ocx_oci::Algorithm::Sha256.hash(&blob);

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
        let dial = TestDial::new();
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = SigningStatePaths::new(temp.path());
        let ctx = attestation_ctx(
            &identifier,
            indirecting_resolver(identifier.clone()),
            dial.policy(),
            &trust_root,
            &rekor_url,
            state.clone(),
        );

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
                crate::verify::DiscoveryMethod::ReferrersApi,
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
    const GOLDEN_KEYLESS_BUNDLE: &str = include_str!("../../../../test/tests/fixtures/golden/keyless_bundle.json");

    /// The referrer manifest cosign pushed alongside it — the source of every
    /// digest and annotation this test pins, so nothing is transcribed by hand.
    const GOLDEN_KEYLESS_REFERRER: &str =
        include_str!("../../../../test/tests/fixtures/golden/keyless_referrer_manifest.json");

    /// The whole local Sigstore trust root: Fulcio CA, CT log key, Rekor key.
    /// Committed and deterministic, which is what makes this test offline.
    const GOLDEN_TRUSTED_ROOT: &str = include_str!("../../../../test/sigstore/trusted_root.json");

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
            ocx_oci::Algorithm::Sha256
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
        use ocx_oci::client::test_transport::{StubTransport, StubTransportData};

        let subject_bytes = GOLDEN_SUBJECT_MANIFEST.as_bytes();
        let subject_digest = ocx_oci::Algorithm::Sha256.hash(subject_bytes);

        // The committed bundle is pretty-printed, so its digest is not the one
        // cosign's referrer names; the referrer is rebuilt around these bytes
        // and carries the predicateType annotation cosign wrote, which the
        // annotation-direction check then cross-examines.
        let blob = GOLDEN_KEYLESS_BUNDLE.as_bytes().to_vec();
        let blob_digest = ocx_oci::Algorithm::Sha256.hash(&blob);
        let annotated_predicate_type = golden_referrer_field("/annotations/dev.sigstore.bundle.predicateType");

        let data = StubTransportData::new();
        data.write().blobs.insert(blob_digest.to_string(), blob.clone());
        let transport = StubTransport::new(data);
        let image: native::Reference = "registry.example/repo:latest".parse().expect("stub reference");

        let trust_root =
            TrustRoot::load_trusted_root_json(GOLDEN_TRUSTED_ROOT.as_bytes()).expect("the committed trust root loads");
        let verifier = Verifier::new(RekorConfiguration::default(), trust_root.clone()).expect("verifier");
        let policies = [ocx_trust::CompiledPolicy {
            builder: None,
            backends: vec![ocx_trust::PolicyBackend::Keyless(ocx_trust::CompiledKeyless {
                identity: ocx_trust::IdentityRule::Exact(GOLDEN_IDENTITY.to_string()),
                issuer: GOLDEN_ISSUER.to_string(),
            })],
        }];
        let identifier = verify_id();
        let dial = TestDial::new();
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = SigningStatePaths::new(temp.path());
        let ctx = VerifyContext {
            identifier: &identifier,
            platform: None,
            policies: &policies,
            no_cache: true,
            dial: dial.policy(),
            resolve: indirecting_resolver(identifier.clone()),
            trust_root: &trust_root,
            rekor_url: &rekor_url,
            state: state.clone(),
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
            crate::verify::DiscoveryMethod::ReferrersApi,
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
    const GOLDEN_KEY_BUNDLE: &str = include_str!("../../../../test/tests/fixtures/golden/key_bundle.json");

    /// The public half of the cosign key pair that signed it. `include_str!` so
    /// a moved fixture is a compile error, same as every constant above.
    const GOLDEN_PUBLIC_KEY_PEM: &str = include_str!("../../../../test/tests/fixtures/golden/keys/cosign.pub");

    /// A second, unrelated SPKI public key — the local Rekor log's. Used as
    /// "a key the policy trusts that did not sign this artifact", so that case
    /// is exercised against a real key rather than a synthesized one.
    const UNRELATED_PUBLIC_KEY_PEM: &str = include_str!("../../../../test/sigstore/keys/rekor.pub.pem");

    /// The referrer manifest cosign pushed alongside the key bundle — the
    /// source of its predicateType annotation, so nothing is transcribed.
    const GOLDEN_KEY_REFERRER: &str = include_str!("../../../../test/tests/fixtures/golden/key_referrer_manifest.json");

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
            ocx_oci::Algorithm::Sha256
                .hash(GOLDEN_SUBJECT_MANIFEST.as_bytes())
                .to_string(),
            golden_key_referrer_field("/subject/digest"),
            "the reproduced subject manifest must hash to the digest cosign's key-mode referrer names",
        );
    }

    fn key_policy(pem: &str) -> ocx_trust::CompiledPolicy {
        ocx_trust::CompiledPolicy {
            builder: None,
            backends: vec![ocx_trust::PolicyBackend::Key(
                sigstore::crypto::CosignVerificationKey::try_from_pem(pem.as_bytes())
                    .expect("the fixture is an SPKI PEM"),
            )],
        }
    }

    fn golden_keyless_policy() -> ocx_trust::CompiledPolicy {
        ocx_trust::CompiledPolicy {
            builder: None,
            backends: vec![ocx_trust::PolicyBackend::Keyless(ocx_trust::CompiledKeyless {
                identity: ocx_trust::IdentityRule::Exact(GOLDEN_IDENTITY.to_string()),
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
        policies: &[ocx_trust::CompiledPolicy],
    ) -> Result<CandidateOutcome, VerifyErrorKind> {
        use ocx_oci::client::test_transport::{StubTransport, StubTransportData};

        let subject_bytes = GOLDEN_SUBJECT_MANIFEST.as_bytes();
        let subject_digest = ocx_oci::Algorithm::Sha256.hash(subject_bytes);
        let blob = bundle_json.as_bytes().to_vec();
        let blob_digest = ocx_oci::Algorithm::Sha256.hash(&blob);

        let data = StubTransportData::new();
        data.write().blobs.insert(blob_digest.to_string(), blob.clone());
        let transport = StubTransport::new(data);
        let image: native::Reference = "registry.example/repo:latest".parse().expect("stub reference");

        let trust_root =
            TrustRoot::load_trusted_root_json(GOLDEN_TRUSTED_ROOT.as_bytes()).expect("the committed trust root loads");
        let verifier = Verifier::new(RekorConfiguration::default(), trust_root.clone()).expect("verifier");
        let identifier = verify_id();
        let dial = TestDial::new();
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = SigningStatePaths::new(temp.path());
        let ctx = VerifyContext {
            identifier: &identifier,
            platform: None,
            policies,
            no_cache: true,
            dial: dial.policy(),
            resolve: indirecting_resolver(identifier.clone()),
            trust_root: &trust_root,
            rekor_url: &rekor_url,
            state: state.clone(),
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
            crate::verify::DiscoveryMethod::ReferrersApi,
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
        use ocx_oci::client::test_transport::{StubTransport, StubTransportData};

        let subject_bytes = b"the artifact under signature";
        let subject_digest = ocx_oci::Algorithm::Sha256.hash(subject_bytes);
        let blob = serde_json::to_vec(&message_bundle(true, true)).expect("bundle serializes");
        let blob_digest = ocx_oci::Algorithm::Sha256.hash(&blob);

        let data = StubTransportData::new();
        data.write().blobs.insert(blob_digest.to_string(), blob.clone());
        let transport = StubTransport::new(data);
        let image: native::Reference = "registry.example/repo:latest".parse().expect("stub reference");

        let ca_der = super::super::tlog::fixture_certificate_der();
        let trust_root = trust_root_of(&[&ca_der]);
        let verifier = Verifier::new(RekorConfiguration::default(), trust_root.clone()).expect("verifier");
        let identifier = verify_id();
        let dial = TestDial::new();
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = SigningStatePaths::new(temp.path());
        let ctx = VerifyContext {
            content: VerifyContentMode::Signature,
            ..attestation_ctx(
                &identifier,
                indirecting_resolver(identifier.clone()),
                dial.policy(),
                &trust_root,
                &rekor_url,
                state.clone(),
            )
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
            crate::verify::DiscoveryMethod::ReferrersApi,
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
        use ocx_oci::client::test_transport::{StubTransport, StubTransportData};

        let transport = StubTransport::new(StubTransportData::new());
        let image: native::Reference = "registry.example/repo:latest".parse().expect("stub reference");
        let subject_digest = ocx_oci::Algorithm::Sha256.hash(b"subject");
        let blob_digest = ocx_oci::Algorithm::Sha256.hash(b"a bundle nobody will fetch");
        let ca_der = super::super::tlog::fixture_certificate_der();
        let trust_root = trust_root_of(&[&ca_der]);
        let verifier = Verifier::new(RekorConfiguration::default(), trust_root.clone()).expect("verifier");
        let identifier = verify_id();
        let dial = TestDial::new();
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = SigningStatePaths::new(temp.path());

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
                dial: dial.policy(),
                resolve: indirecting_resolver(identifier.clone()),
                trust_root: &trust_root,
                rekor_url: &rekor_url,
                state: state.clone(),
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
                crate::verify::DiscoveryMethod::ReferrersApi,
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
        let dial = TestDial::new();
        let trust_root = trust_root_of(&[]);
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = SigningStatePaths::new(temp.path());
        let ctx = attestation_ctx(
            &identifier,
            indirecting_resolver(identifier.clone()),
            dial.policy(),
            &trust_root,
            &rekor_url,
            state.clone(),
        );
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
                layer_media_type: crate::sign::bundle::BUNDLE_V03_MEDIA_TYPE.to_string(),
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
                layer_media_type: crate::sign::bundle::BUNDLE_V03_MEDIA_TYPE.to_string(),
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
            ocx_oci::Algorithm::Sha256.hash(&self.document)
        }

        /// This referrer as the plain data an [`SbomTransport`] serves: how it
        /// appears in a listing, the manifest behind it, and every layer's body
        /// in manifest order.
        fn served(&self) -> ocx_oci::testing::ServedReferrer {
            let blobs = std::iter::once(self.document.clone())
                .chain(self.extra_documents.iter().cloned())
                .collect();
            ocx_oci::testing::ServedReferrer::new(self.descriptor(), self.manifest_bytes(), blobs)
        }

        /// Built through the production builder, so the fixture cannot drift
        /// from the shape the read path parses.
        fn manifest_bytes(&self) -> Vec<u8> {
            let payload = ocx_oci::Descriptor {
                media_type: self.layer_media_type.clone(),
                digest: self.document_digest().to_string(),
                size: self.declared_layer_size.unwrap_or(self.document.len() as i64),
                ..ocx_oci::Descriptor::default()
            };
            let subject = ocx_oci::Descriptor {
                media_type: ocx_oci::OCI_IMAGE_MEDIA_TYPE.to_string(),
                digest: indirection_subject_digest().to_string(),
                size: INDIRECTION_SUBJECT_MANIFEST.len() as i64,
                ..ocx_oci::Descriptor::default()
            };
            let built = ocx_oci::referrer::ReferrerManifest::build(subject, &self.artifact_type, payload, None)
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
                layer["digest"] = serde_json::json!(ocx_oci::Algorithm::Sha256.hash(document).to_string());
                layer["size"] = serde_json::json!(document.len());
                layers.push(layer);
            }
            manifest["layers"] = serde_json::Value::Array(layers);
            serde_json::to_vec(&manifest).expect("the spliced referrer manifest serializes")
        }

        fn descriptor(&self) -> ocx_oci::Descriptor {
            let bytes = self.manifest_bytes();
            ocx_oci::Descriptor {
                media_type: ocx_oci::OCI_IMAGE_MEDIA_TYPE.to_string(),
                digest: ocx_oci::Algorithm::Sha256.hash(&bytes).to_string(),
                size: bytes.len() as i64,
                artifact_type: Some(self.artifact_type.clone()),
                ..ocx_oci::Descriptor::default()
            }
        }
    }

    /// The shared SBOM transport over this pipeline's subject and a referrer
    /// set described by [`StubReferrer`].
    fn sbom_transport(referrers: Vec<StubReferrer>) -> SbomTransport {
        SbomTransport::new(
            INDIRECTION_SUBJECT_MANIFEST,
            referrers.iter().map(StubReferrer::served).collect(),
        )
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
        drive_sbom_scan_over_transport(sbom_transport(referrers), predicate_type, verification, trust_root).await
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
        let logical = PackageRef::parse("ocx.sh/acme/tool:1.0").expect("logical identifier");
        // A public IP literal, not a name: the dial-site SSRF guard resolves the
        // physical host, and a DNS name here would make this unit test open a
        // socket.
        let physical = PackageRef::parse("8.8.8.8/acme/tool:1.0").expect("physical identifier");

        let client = Client::with_transport(Box::new(transport.clone()));
        let dial = TestDial::new();
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = SigningStatePaths::new(temp.path());
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");

        let outcome = VerifyPipeline::run_attestations(
            &client,
            VerifyContext {
                identifier: &logical,
                platform: None,
                policies: &[],
                no_cache: true,
                dial: dial.policy(),
                resolve: indirecting_resolver(physical.clone()),
                trust_root: &trust_root,
                rekor_url: &rekor_url,
                state: state.clone(),
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
        resolved: Option<(Digest, ocx_oci::Manifest)>,
        platform: Option<&Platform>,
    ) -> (Result<AttestationScan, VerifyError>, SbomTransport, tempfile::TempDir) {
        let identifier = PackageRef::parse("ocx.sh/acme/tool:1.0").expect("logical identifier");
        let transport = sbom_transport(referrers);
        let client = Client::with_transport(Box::new(transport.clone()));
        // Physical == logical: not a rewrite, so the dial-site SSRF guard's
        // own carve-out applies and this unit test resolves no name.
        let dial = TestDial::new();
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = SigningStatePaths::new(temp.path());
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let outcome = VerifyPipeline::run_attestations(
            &client,
            VerifyContext {
                identifier: &identifier,
                platform,
                policies: &[],
                no_cache: true,
                dial: dial.policy(),
                resolve: resolving_resolver(identifier.clone(), resolved),
                trust_root: &TrustRoot::default(),
                rekor_url: &rekor_url,
                state: state.clone(),
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
        let enclosing = ocx_oci::Algorithm::Sha256.hash(b"the enclosing image index");
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

        let logical = PackageRef::parse("ocx.sh/acme/tool:1.0").expect("logical identifier");
        let physical = PackageRef::parse("8.8.8.8/acme/tool:1.0").expect("physical identifier");
        let transport = sbom_transport(vec![sbom]);
        let client = Client::with_transport(Box::new(transport.clone()));
        let dial = TestDial::new();
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = SigningStatePaths::new(temp.path());
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
                dial: dial.policy(),
                resolve: indirecting_resolver(physical.clone()),
                trust_root: &trust_root,
                rekor_url: &rekor_url,
                state: state.clone(),
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
            ocx_oci::referrer::media_types::SBOM_SPDX_TEXT,
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
            ocx_oci::referrer::media_types::SBOM_SPDX_TEXT,
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
            ocx_oci::referrer::media_types::SBOM_SPDX_TEXT,
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
        include_str!("../../../../test/tests/fixtures/golden/simplesigning_key_manifest.json");
    const SIDECAR_PAYLOAD: &[u8] =
        include_bytes!("../../../../test/tests/fixtures/golden/simplesigning_key_payload.json");

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
    fn sidecar_layer() -> ocx_oci::Descriptor {
        let manifest: ocx_oci::ImageManifest =
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
    fn referrer_descriptor(manifest_bytes: &[u8], artifact_type: &str) -> ocx_oci::Descriptor {
        ocx_oci::Descriptor {
            media_type: ocx_oci::OCI_IMAGE_MEDIA_TYPE.to_string(),
            digest: ocx_oci::Algorithm::Sha256.hash(manifest_bytes).to_string(),
            size: manifest_bytes.len() as i64,
            artifact_type: Some(artifact_type.to_string()),
            ..ocx_oci::Descriptor::default()
        }
    }

    /// Drive [`VerifyPipeline::scan`] over a seeded stub registry.
    ///
    /// The scan, not one candidate: the merge of the four discovery shapes into
    /// one candidate list happens here and nowhere else, so a test that drove
    /// `verify_one_referrer` directly could not observe it.
    async fn drive_scan(
        data: ocx_oci::client::test_transport::StubTransportData,
        subject_digest: &Digest,
        policies: &[ocx_trust::CompiledPolicy],
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
        data: ocx_oci::client::test_transport::StubTransportData,
        subject_digest: &Digest,
        policies: &[ocx_trust::CompiledPolicy],
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
        data: ocx_oci::client::test_transport::StubTransportData,
        subject_digest: &Digest,
        policies: &[ocx_trust::CompiledPolicy],
        trust_root: &TrustRoot,
        signature_format: Option<SignatureFormat>,
        arity: ScanArity,
        content: VerifyContentMode,
        budget: &mut ScanBudget,
    ) -> Result<ScanOutcome, VerifyErrorKind> {
        use ocx_oci::client::test_transport::StubTransport;

        let client = Client::with_transport(Box::new(StubTransport::new(data)));
        let image: native::Reference = SCAN_IMAGE.parse().expect("stub reference");
        let identifier = verify_id();
        let dial = TestDial::new();
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = SigningStatePaths::new(temp.path());
        let ctx = VerifyContext {
            identifier: &identifier,
            platform: None,
            policies,
            no_cache: true,
            dial: dial.policy(),
            resolve: indirecting_resolver(identifier.clone()),
            trust_root,
            rekor_url: &rekor_url,
            state: state.clone(),
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
    ) -> ocx_oci::client::test_transport::StubTransportData {
        use ocx_oci::client::sibling_tag_reference;
        use ocx_oci::client::test_transport::{StubTransportData, referrers_key};

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
        include_bytes!("../../../../test/tests/fixtures/golden/sbom_sidecar_manifest.json");
    const SBOM_SIDECAR_DOCUMENT: &[u8] =
        include_bytes!("../../../../test/tests/fixtures/golden/sbom_sidecar_document.json");

    /// A transport serving cosign's committed `.sbom` sidecar and nothing else
    /// — no referrer of any kind, so a document that comes back can only have
    /// arrived through the tag.
    fn sbom_sidecar_transport() -> SbomTransport {
        sbom_transport(Vec::new()).with_sidecar(SBOM_SIDECAR_MANIFEST, &[SBOM_SIDECAR_DOCUMENT])
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
            let transport = sbom_transport(vec![referrer]);
            let client = Client::with_transport(Box::new(transport));
            let target = ScanTarget {
                image: "registry.example/repo:latest".parse().expect("stub reference"),
                subject_digest: indirection_subject_digest(),
                enclosing_index: None,
                index_members: Vec::new(),
            };
            let identifier = verify_id();
            let dial = TestDial::new();
            let (_key, cert) = self_signed_cert();
            let trust_root = trust_root_of(&[&cert]);
            let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
            let temp = tempfile::TempDir::new().expect("state dir");
            let state = SigningStatePaths::new(temp.path());
            let ctx = VerifyContext {
                identifier: &identifier,
                platform: None,
                policies: &[],
                no_cache: true,
                dial: dial.policy(),
                resolve: indirecting_resolver(identifier.clone()),
                trust_root: &trust_root,
                rekor_url: &rekor_url,
                state: state.clone(),
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
            ocx_oci::Algorithm::Sha256.hash(SBOM_SIDECAR_MANIFEST).to_string(),
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
                layer["digest"] = serde_json::json!(ocx_oci::Algorithm::Sha256.hash(document).to_string());
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
        let transport = sbom_transport(Vec::new()).with_sidecar(&manifest, &[SBOM_SIDECAR_DOCUMENT, SECOND_DOCUMENT]);

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
            drive_sidecar_scan(sbom_transport(Vec::new()), None, VerificationMode::Permissive).await;

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
        let transport = sbom_transport(vec![referrer]).with_sidecar(SBOM_SIDECAR_MANIFEST, &[SBOM_SIDECAR_DOCUMENT]);

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
        let Err(_error) = spdx else {
            panic!("a CycloneDX sidecar must not answer a request for SPDX");
        };
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
        let transport = sbom_transport(Vec::new()).with_sidecar(mutated.as_bytes(), &[SBOM_SIDECAR_DOCUMENT]);

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
    ) -> ocx_oci::client::test_transport::StubTransportData {
        use ocx_oci::client::sibling_tag_reference;
        use ocx_oci::client::test_transport::{StubTransport, StubTransportData};

        let client = Client::with_transport(Box::new(StubTransport::new(StubTransportData::new())));
        let image = client.transport_reference(&verify_physical());
        let manifest: ocx_oci::ImageManifest = serde_json::from_str(ATT_MANIFEST).expect("the `.att` manifest parses");
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
            let sbom_tag = sibling_tag_reference(&image, ocx_oci::tag::sbom_sidecar_tag(subject));
            inner.manifests.insert(
                sbom_tag.to_string(),
                (
                    SBOM_SIDECAR_MANIFEST.to_vec(),
                    ocx_oci::Algorithm::Sha256.hash(SBOM_SIDECAR_MANIFEST).to_string(),
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
        data: ocx_oci::client::test_transport::StubTransportData,
        subject: &Digest,
    ) -> Result<AttestationScan, VerifyError> {
        use ocx_oci::client::test_transport::StubTransport;

        let client = Client::with_transport(Box::new(StubTransport::new(data)));
        let identifier = verify_id();
        let dial = TestDial::new();
        let trust_root =
            TrustRoot::load_trusted_root_json(GOLDEN_TRUSTED_ROOT.as_bytes()).expect("the committed trust root loads");
        let policies = [key_policy(GOLDEN_PUBLIC_KEY_PEM)];
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = SigningStatePaths::new(temp.path());
        VerifyPipeline::run_attestations(
            &client,
            VerifyContext {
                identifier: &identifier,
                platform: None,
                policies: &policies,
                no_cache: true,
                dial: dial.policy(),
                resolve: resolving_resolver(
                    identifier.clone(),
                    Some((
                        subject.clone(),
                        ocx_oci::Manifest::Image(ocx_oci::ImageManifest::default()),
                    )),
                ),
                trust_root: &trust_root,
                rekor_url: &rekor_url,
                state: state.clone(),
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
        let subject = ocx_oci::Algorithm::Sha256.hash(GOLDEN_SUBJECT_MANIFEST.as_bytes());

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
                ocx_oci::Algorithm::Sha256
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
        use ocx_oci::client::sibling_tag_reference;
        use ocx_oci::client::test_transport::{StubTransport, StubTransportData};

        let subject = ocx_oci::Algorithm::Sha256.hash(GOLDEN_SUBJECT_MANIFEST.as_bytes());
        let data = seed_signed_attestation_beside_an_unsigned_sbom_sidecar(&subject);
        let client = Client::with_transport(Box::new(StubTransport::new(StubTransportData::new())));
        let sbom_tag = sibling_tag_reference(
            &client.transport_reference(&verify_physical()),
            ocx_oci::tag::sbom_sidecar_tag(&subject),
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
        use ocx_oci::client::sibling_tag_reference;
        use ocx_oci::client::test_transport::{StubTransport, StubTransportData};

        let subject = ocx_oci::Algorithm::Sha256.hash(GOLDEN_SUBJECT_MANIFEST.as_bytes());
        let client = Client::with_transport(Box::new(StubTransport::new(StubTransportData::new())));
        let image = client.transport_reference(&verify_physical());
        let data = StubTransportData::new();
        {
            let mut inner = data.write();
            inner.manifests.insert(
                image.clone_with_digest(subject.to_string()).to_string(),
                (GOLDEN_SUBJECT_MANIFEST.as_bytes().to_vec(), subject.to_string()),
            );
            inner.manifest_errors.insert(
                sibling_tag_reference(&image, ocx_oci::tag::sbom_sidecar_tag(&subject)).to_string(),
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
        include_str!("../../../../test/tests/fixtures/golden/attestation_sidecar_key_manifest.json");
    const ATT_ENVELOPE: &[u8] =
        include_bytes!("../../../../test/tests/fixtures/golden/attestation_sidecar_key_envelope.json");

    /// Seed the `sha256-<hex>.att` tag, its envelope blob, and the subject
    /// manifest the keyless arm would need — no referrer listing anywhere, so a
    /// match can only have come through the tag door.
    fn seed_attestation_sidecar_tag(subject: &Digest) -> ocx_oci::client::test_transport::StubTransportData {
        use ocx_oci::client::sibling_tag_reference;
        use ocx_oci::client::test_transport::StubTransportData;

        let image: native::Reference = SCAN_IMAGE.parse().expect("stub reference");
        let manifest: ocx_oci::ImageManifest = serde_json::from_str(ATT_MANIFEST).expect("the `.att` manifest parses");
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
        let subject = ocx_oci::Algorithm::Sha256.hash(GOLDEN_SUBJECT_MANIFEST.as_bytes());
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
            let dial = TestDial::new();
            let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
            let temp = tempfile::TempDir::new().expect("state dir");
            let state = SigningStatePaths::new(temp.path());
            let ctx = VerifyContext {
                identifier: &identifier,
                platform: None,
                policies: &[],
                no_cache: true,
                dial: dial.policy(),
                resolve: indirecting_resolver(identifier.clone()),
                trust_root: &trust_root,
                rekor_url: &rekor_url,
                state: state.clone(),
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

            TrustRootCache::from_cache(&crate::verify::trust_cache::cache_key_for_rekor(&rekor_url), &state)
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
        use ocx_oci::referrer::media_types::{
            ANNOTATION_COSIGN_BUNDLE, ANNOTATION_COSIGN_CERTIFICATE, DSSE_ENVELOPE_MEDIA_TYPE,
        };

        let envelope = golden_dsse_envelope();
        let manifest = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "config": {
                "mediaType": "application/vnd.oci.image.config.v1+json",
                "size": 0,
                "digest": ocx_oci::Algorithm::Sha256.hash(b"").to_string(),
            },
            "layers": [{
                "mediaType": DSSE_ENVELOPE_MEDIA_TYPE,
                "size": envelope.len(),
                "digest": ocx_oci::Algorithm::Sha256.hash(&envelope).to_string(),
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
        data: ocx_oci::client::test_transport::StubTransportData,
        subject_digest: &Digest,
        policies: &[ocx_trust::CompiledPolicy],
        trust_root: &TrustRoot,
        content: VerifyContentMode,
    ) -> (Result<ScanOutcome, VerifyErrorKind>, tempfile::TempDir, Url) {
        use ocx_oci::client::test_transport::StubTransport;

        let client = Client::with_transport(Box::new(StubTransport::new(data)));
        let image: native::Reference = SCAN_IMAGE.parse().expect("stub reference");
        let identifier = verify_id();
        let dial = TestDial::new();
        // Never dialled: the committed trust root pins this stack's Rekor key,
        // so the resolution is answered from trust material and the URL is only
        // the cache key.
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let temp = tempfile::TempDir::new().expect("state dir");
        let outcome = {
            let state = SigningStatePaths::new(temp.path());
            let ctx = VerifyContext {
                identifier: &identifier,
                platform: None,
                policies,
                no_cache: true,
                dial: dial.policy(),
                resolve: indirecting_resolver(identifier.clone()),
                trust_root,
                rekor_url: &rekor_url,
                state: state.clone(),
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
        use ocx_oci::client::sibling_tag_reference;
        use ocx_oci::client::test_transport::StubTransportData;

        let subject = ocx_oci::Algorithm::Sha256.hash(GOLDEN_SUBJECT_MANIFEST.as_bytes());
        let (manifest, envelope) = keyless_att_sidecar();
        let image: native::Reference = SCAN_IMAGE.parse().expect("stub reference");
        let layer = {
            let parsed: ocx_oci::ImageManifest =
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
        let policies = [ocx_trust::CompiledPolicy {
            builder: None,
            backends: vec![ocx_trust::PolicyBackend::Keyless(ocx_trust::CompiledKeyless {
                identity: ocx_trust::IdentityRule::Exact(GOLDEN_IDENTITY.to_string()),
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

        let state = SigningStatePaths::new(temp.path());
        let cached = TrustRootCache::from_cache(&crate::verify::trust_cache::cache_key_for_rekor(&rekor_url), &state)
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
        use ocx_oci::client::test_transport::referrers_key;

        let subject = ocx_oci::Algorithm::Sha256.hash(GOLDEN_SUBJECT_MANIFEST.as_bytes());
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
        use ocx_oci::client::test_transport::referrers_key;

        let subject = ocx_oci::Algorithm::Sha256.hash(GOLDEN_SUBJECT_MANIFEST.as_bytes());
        let policies = [key_policy(GOLDEN_PUBLIC_KEY_PEM)];
        let trust_root =
            TrustRoot::load_trusted_root_json(GOLDEN_TRUSTED_ROOT.as_bytes()).expect("the committed trust root loads");

        let data = seed_attestation_sidecar_tag(&subject);
        let blob = serde_json::to_vec(&into_signature_bundle(message_bundle(true, true)))
            .expect("the signature bundle serializes");
        let blob_digest = ocx_oci::Algorithm::Sha256.hash(&blob);
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
        let subject = ocx_oci::Algorithm::Sha256.hash(GOLDEN_SUBJECT_MANIFEST.as_bytes());
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
    ) -> ocx_oci::client::test_transport::StubTransportData {
        use ocx_oci::client::sibling_tag_reference;
        use ocx_oci::client::test_transport::StubTransportData;

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
        let subject = ocx_oci::Algorithm::Sha256.hash(GOLDEN_SUBJECT_MANIFEST.as_bytes());
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
        let subject = ocx_oci::Algorithm::Sha256.hash(GOLDEN_SUBJECT_MANIFEST.as_bytes());
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
        let subject = ocx_oci::Algorithm::Sha256.hash(GOLDEN_SUBJECT_MANIFEST.as_bytes());
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
        use ocx_oci::client::test_transport::{StubTransportData, referrers_key};

        let subject = ocx_oci::Algorithm::Sha256.hash(GOLDEN_SUBJECT_MANIFEST.as_bytes());
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
        let subject = ocx_oci::Algorithm::Sha256.hash(GOLDEN_SUBJECT_MANIFEST.as_bytes());
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
                    subject_digest: ocx_oci::Algorithm::Sha256.hash(subject),
                    referrer_digest: ocx_oci::Algorithm::Sha256.hash(b"referrer"),
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
        use ocx_oci::client::test_transport::{StubTransportData, referrers_key};

        let subject_bytes = GOLDEN_SUBJECT_MANIFEST.as_bytes();
        let subject = ocx_oci::Algorithm::Sha256.hash(subject_bytes);
        let blob = GOLDEN_KEYLESS_BUNDLE.as_bytes().to_vec();
        let blob_digest = ocx_oci::Algorithm::Sha256.hash(&blob);
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
        use ocx_oci::client::test_transport::{StubTransportData, referrers_key};

        let subject_bytes = GOLDEN_SUBJECT_MANIFEST.as_bytes();
        let subject = ocx_oci::Algorithm::Sha256.hash(subject_bytes);
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
                    let blob_digest = ocx_oci::Algorithm::Sha256.hash(&blob);
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
        use ocx_oci::client::test_transport::{StubTransportData, referrers_key};

        let subject_bytes = GOLDEN_SUBJECT_MANIFEST.as_bytes();
        let subject = ocx_oci::Algorithm::Sha256.hash(subject_bytes);
        let bundle_json = without_tlog_entries(GOLDEN_KEY_BUNDLE);
        let annotated = golden_key_referrer_field("/annotations/dev.sigstore.bundle.predicateType");
        let image: native::Reference = SCAN_IMAGE.parse().expect("stub reference");

        let seed = || {
            let blob = bundle_json.as_bytes().to_vec();
            let blob_digest = ocx_oci::Algorithm::Sha256.hash(&blob);
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

    /// An image index listing exactly `children`, so a test states the
    /// `manifests[]` the membership test reads.
    fn image_index_of(children: &[(&str, &Digest)]) -> ocx_oci::Manifest {
        ocx_oci::Manifest::ImageIndex(ocx_oci::native::ImageIndex {
            schema_version: 2,
            media_type: Some(ocx_oci::OCI_IMAGE_INDEX_MEDIA_TYPE.to_string()),
            manifests: children
                .iter()
                .map(|(platform, digest)| ocx_oci::native::ImageIndexEntry {
                    media_type: ocx_oci::OCI_IMAGE_MEDIA_TYPE.to_string(),
                    digest: digest.to_string(),
                    size: 2,
                    platform: Some(ocx_oci::native::Platform::from(
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
        data: ocx_oci::client::test_transport::StubTransportData,
        resolved: Option<(Digest, ocx_oci::Manifest)>,
        platform: Option<&Platform>,
    ) -> Result<Vec<VerifyResult>, VerifyErrorKind> {
        use ocx_oci::client::test_transport::StubTransport;

        let client = Client::with_transport(Box::new(StubTransport::new(data)));
        let identifier = verify_id();
        let dial = TestDial::new();
        let trust_root =
            TrustRoot::load_trusted_root_json(GOLDEN_TRUSTED_ROOT.as_bytes()).expect("the committed trust root loads");
        let policies = [key_policy(GOLDEN_PUBLIC_KEY_PEM)];
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = SigningStatePaths::new(temp.path());
        VerifyPipeline::run_inner(
            &client,
            VerifyContext {
                identifier: &identifier,
                platform,
                policies: &policies,
                no_cache: true,
                dial: dial.policy(),
                resolve: resolving_resolver(identifier.clone(), resolved),
                trust_root: &trust_root,
                rekor_url: &rekor_url,
                state: state.clone(),
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
    fn seed_sidecar_on(subject: &Digest) -> ocx_oci::client::test_transport::StubTransportData {
        use ocx_oci::client::test_transport::{StubTransportData, referrers_key};

        let client = Client::with_transport(Box::new(ocx_oci::client::test_transport::StubTransport::new(
            StubTransportData::new(),
        )));
        let image = client.transport_reference(&verify_physical());
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
                ocx_oci::Manifest::Image(ocx_oci::ImageManifest::default()),
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
        data: ocx_oci::client::test_transport::StubTransportData,
        target: ScanTarget,
    ) -> Result<ScanOutcome, VerifyErrorKind> {
        use ocx_oci::client::test_transport::StubTransport;

        let client = Client::with_transport(Box::new(StubTransport::new(data)));
        let identifier = verify_id();
        let dial = TestDial::new();
        let trust_root =
            TrustRoot::load_trusted_root_json(GOLDEN_TRUSTED_ROOT.as_bytes()).expect("the committed trust root loads");
        let policies = [key_policy(GOLDEN_PUBLIC_KEY_PEM)];
        let rekor_url = Url::parse("http://127.0.0.1:3000").expect("rekor url");
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = SigningStatePaths::new(temp.path());
        let ctx = VerifyContext {
            identifier: &identifier,
            platform: None,
            policies: &policies,
            no_cache: true,
            dial: dial.policy(),
            resolve: resolving_resolver(identifier.clone(), None),
            trust_root: &trust_root,
            rekor_url: &rekor_url,
            state: state.clone(),
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
        let image = Client::with_transport(Box::new(ocx_oci::client::test_transport::StubTransport::new(
            ocx_oci::client::test_transport::StubTransportData::new(),
        )))
        .transport_reference(&verify_physical());
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
                ocx_oci::Manifest::Image(ocx_oci::ImageManifest::default()),
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
