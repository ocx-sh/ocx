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

use super::error::{TrustRootLoadReason, VerifyError, VerifyErrorKind};
use super::identity::{oidc_issuer, parse_certificate, subject_identity, verify_policies};
use super::tlog;
use super::trust_cache::TrustRootCache;
use super::trust_root::TrustRoot;
use crate::file_structure::StateStore;
use crate::oci::client::error::ClientError;
use crate::oci::client::{Client, OciTransport};
use crate::oci::index::{Index, IndexOperation, SelectResult};
use crate::oci::referrer::capability::{ReferrersApiCapability, ReferrersSupport};
use crate::oci::referrer::media_types::SIGSTORE_BUNDLE_V03;
use crate::oci::sign::bundle::{MAX_BUNDLE_SIZE_BYTES, parse_bundle};
use crate::oci::{Digest, Identifier, Platform, native};
use sigstore_protobuf_specs::dev::sigstore::bundle::v1::{bundle, verification_material};
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

    async fn run_inner(client: &Client, ctx: VerifyContext<'_>) -> Result<VerifyResult, VerifyErrorKind> {
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

        let transport = client.transport();
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
        let referrers = Self::list_signature_referrers(transport, &ctx, &image, &subject_digest).await?;
        let mut candidates: Vec<crate::oci::Descriptor> = referrers
            .into_iter()
            .filter(|descriptor| match descriptor.artifact_type.as_deref() {
                Some(artifact_type) => artifact_type == SIGSTORE_BUNDLE_V03,
                None => true,
            })
            .collect();
        if candidates.is_empty() {
            return Err(VerifyErrorKind::NoSignaturesFound);
        }
        // Deterministic order so the passing candidate and the aggregate error
        // are reproducible regardless of registry listing order.
        candidates.sort_by(|a, b| a.digest.cmp(&b.digest));

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
        let subject_bytes = pull_subject_manifest_verified(transport, &image, &subject_digest).await?;

        // 3. ANY-of: verify each candidate independently, returning the first that
        //    fully passes crypto + identity/policy. This fixes key rotation (a
        //    valid later signature is no longer masked by an earlier one) and the
        //    malformed-first-referrer DoS. Bounded by candidate count and a
        //    total-bytes budget. After all fail, return the most actionable error
        //    deterministically — a fail-closed availability outcome, not forgery.
        let total_candidates = candidates.len();
        let mut examined = 0usize;
        let mut spent_bytes: u64 = 0;
        let mut best_error: Option<VerifyErrorKind> = None;
        for descriptor in candidates.into_iter().take(MAX_SIGNATURE_CANDIDATES) {
            // Cheap reject of a self-declared over-cap descriptor before any
            // fetch. The actual body length is re-checked after the read, since
            // the declared size is untrusted (a registry can lie about it).
            if descriptor.size < 0 || descriptor.size as u64 > MAX_REFERRER_MANIFEST_BYTES {
                examined += 1;
                merge_failure(&mut best_error, VerifyErrorKind::BundleParseFailed);
                continue;
            }
            // Stop once the cross-candidate byte budget is spent. Charged from
            // bytes actually read below, never the untrusted declared size, so a
            // registry cannot bypass the budget by advertising size 0.
            if spent_bytes >= MAX_TOTAL_REFERRER_BYTES {
                break;
            }
            examined += 1;
            // `clone_with_digest` drops the tag, so this stays digest-only — a
            // `repo:tag@digest` reference keys a different registry path and 404s.
            let referrer_ref = image.clone_with_digest(descriptor.digest.clone());
            let referrer_bytes = match pull_referrer_manifest_capped(transport, &referrer_ref).await {
                Ok(bytes) => bytes,
                Err(kind) => {
                    // An over-cap read still cost up to the per-manifest cap; charge it.
                    spent_bytes = spent_bytes.saturating_add(MAX_REFERRER_MANIFEST_BYTES);
                    merge_failure(&mut best_error, kind);
                    continue;
                }
            };
            spent_bytes = spent_bytes.saturating_add(referrer_bytes.len() as u64);
            match Self::verify_one_referrer(
                transport,
                &ctx,
                &verifier,
                &descriptor,
                referrer_bytes,
                &subject_digest,
                &subject_bytes,
                &image,
            )
            .await
            {
                Ok(result) => return Ok(result),
                Err(kind) => merge_failure(&mut best_error, kind),
            }
        }
        Err(aggregate_failure(total_candidates, examined, best_error))
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
    ) -> Result<VerifyResult, VerifyErrorKind> {
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
        if bundle_layer.size < 0 || bundle_layer.size as u64 > MAX_BUNDLE_SIZE_BYTES as u64 {
            return Err(VerifyErrorKind::BundleParseFailed);
        }
        let bundle_bytes = pull_bundle_blob_capped(transport, image, &bundle_blob_digest).await?;

        // Parse the bundle.
        let bundle = parse_bundle(&bundle_bytes).ok_or(VerifyErrorKind::BundleParseFailed)?;
        let parts = BundleParts::from_bundle(&bundle)?;

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

        // Identity + issuer match against the resolved trust policies (ANY-of).
        verify_policies(&parts.leaf_der, ctx.policies)?;

        // On a successful online run, cache the trust material for later offline
        // verifies against the same Rekor instance. Best-effort + content-equal
        // skip so a batch does not stampede the file or slide the 24h TTL on use.
        if !ctx.offline {
            cache_trust_material(ctx, rekor_key_pem).await;
        }

        // Emit the result (identity/issuer read back from the cert).
        let cert = parse_certificate(&parts.leaf_der)?;
        Ok(VerifyResult {
            subject_digest: subject_digest.clone(),
            referrer_digest,
            certificate_identity: subject_identity(&cert).unwrap_or_default(),
            certificate_oidc_issuer: oidc_issuer(&cert).unwrap_or_default(),
            signed_at: parts.integrated_time,
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

        // Fetch the signature referrers (server-side artifactType filter).
        transport
            .list_referrers(image, subject_digest, Some(SIGSTORE_BUNDLE_V03))
            .await
            .map_err(map_client_error)
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
    ) -> Result<Self, VerifyErrorKind> {
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

        // A DSSE envelope is an attestation, not an artifact signature — v1 verify
        // handles only message signatures (attestation verify is #198). The
        // signature bytes themselves are `sigstore`'s to read.
        if !matches!(bundle.content.as_ref(), Some(bundle::Content::MessageSignature(_))) {
            return Err(VerifyErrorKind::NoUsableBundle);
        }
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
        None if ctx.offline => return Err(VerifyErrorKind::RekorUnavailable),
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
        .map_err(|_| VerifyErrorKind::RekorUnavailable)?;
    if !response.status().is_success() {
        return Err(VerifyErrorKind::RekorUnavailable);
    }
    // Capped: a PEM public key is under a kilobyte.
    let raw = crate::oci::endpoint::read_body_capped(response)
        .await
        .ok_or(VerifyErrorKind::RekorUnavailable)?;
    String::from_utf8(raw).map_err(|_| VerifyErrorKind::RekorUnavailable)
}

/// Pull the bundle blob with a hard in-memory read cap (CWE-400 defense).
///
/// Reads at most `MAX_BUNDLE_SIZE_BYTES + 1` bytes so an over-cap body is
/// detected and rejected without buffering the whole thing — the pre-download
/// descriptor check bounds the honest case, this bounds a registry that lies
/// about the size. For an honest under-cap blob the native transport's
/// `VerifyingStream` still checks the blob digest at stream end.
async fn pull_bundle_blob_capped(
    transport: &dyn OciTransport,
    image: &native::Reference,
    bundle_blob_digest: &Digest,
) -> Result<Vec<u8>, VerifyErrorKind> {
    use tokio::io::AsyncReadExt as _;
    let reader = transport
        .pull_blob_streaming(image, bundle_blob_digest)
        .await
        .map_err(map_client_error)?;
    let mut bytes = Vec::new();
    reader
        .take(MAX_BUNDLE_SIZE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| VerifyErrorKind::BundleParseFailed)?;
    if bytes.len() > MAX_BUNDLE_SIZE_BYTES {
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

/// Keep the most actionable failure across candidate referrers: replace the
/// running best when `kind` outranks it (see [`failure_rank`]).
fn merge_failure(best: &mut Option<VerifyErrorKind>, kind: VerifyErrorKind) {
    if best
        .as_ref()
        .is_none_or(|prev| failure_rank(&kind) > failure_rank(prev))
    {
        *best = Some(kind);
    }
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
        VerifyErrorKind::RekorUnavailable | VerifyErrorKind::RekorSetAbsentTsaPresent => 3,
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
/// enforces identity itself in [`verify_policies`].
///
/// Not a hole: `verify_policies` runs unconditionally on the same leaf a few
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
    use crate::cli::{ExitCode, classify_error};
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
            BundleParts::from_bundle(&bundle),
            Err(VerifyErrorKind::BundleParseFailed)
        ));
    }

    #[test]
    fn from_bundle_requires_a_tlog_entry() {
        let bundle = message_bundle(true, false);
        assert!(matches!(
            BundleParts::from_bundle(&bundle),
            Err(VerifyErrorKind::RekorSetInvalid)
        ));
    }

    #[test]
    fn from_bundle_rejects_dsse_envelope() {
        use sigstore_protobuf_specs::dev::sigstore::bundle::v1::bundle;
        use sigstore_protobuf_specs::io::intoto::Envelope;
        let mut bundle = message_bundle(true, true);
        // A DSSE attestation is not an artifact signature (v1 verify handles
        // only message signatures; attestation verify is #198).
        bundle.content = Some(bundle::Content::DsseEnvelope(Envelope {
            payload: Vec::new(),
            payload_type: String::new(),
            signatures: Vec::new(),
        }));
        assert!(matches!(
            BundleParts::from_bundle(&bundle),
            Err(VerifyErrorKind::NoUsableBundle)
        ));
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
                BundleParts::from_bundle(&bundle),
                Err(VerifyErrorKind::RekorInclusionProofAbsent)
            ),
            "a bundle with an inclusion promise but no proof must be refused"
        );
    }

    #[test]
    fn from_bundle_extracts_message_signature_parts() {
        let bundle = message_bundle(true, true);
        let parts = BundleParts::from_bundle(&bundle).expect("valid message bundle");
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
        let rekor_avail = failure_rank(&VerifyErrorKind::RekorUnavailable);
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

        let streamed = pull_bundle_blob_capped(&transport, &image, &honest_digest)
            .await
            .expect("honest under-cap blob streams back");
        assert_eq!(streamed, honest, "streamed bytes must equal the stored blob");

        assert!(
            matches!(
                pull_bundle_blob_capped(&transport, &image, &oversize_digest).await,
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
        crate::oci::referrer::ReferrerManifest::build(subject, SIGSTORE_BUNDLE_V03, payload)
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
}
