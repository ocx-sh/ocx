// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Verify pipeline — full keyless Sigstore verification state machine.
//!
//! Per
//! [`adr_oci_referrers_signing_v1.md`](../../../../../.claude/artifacts/adr_oci_referrers_signing_v1.md)
//! S1-H: resolve target → list referrers (capability cache) → try each v0.3
//! bundle referrer (ANY-of) → parse → verify the Fulcio cert chain against the
//! trust root → bind the message signature to the subject digest → verify the
//! ECDSA signature → verify the Rekor SET → bind the transparency-log body to
//! the bundle → match identity + issuer → emit [`VerifyResult`]. The first
//! candidate that fully passes wins; if all fail the aggregate error is
//! returned.
//!
//! The trust root (Fulcio CA) is injected via [`VerifyContext::trust_root`]
//! (C-S1-3); the Rekor public key used for SET verification is fetched from
//! [`VerifyContext::rekor_url`] `/api/v1/log/publicKey`.

use p256::ecdsa::signature::{Verifier, hazmat::PrehashVerifier};
use url::Url;

use super::error::{VerifyError, VerifyErrorKind};
use super::identity::{oidc_issuer, parse_certificate, subject_identity, verify_policies};
use super::trust_cache::TrustRootCache;
use super::trust_root::TrustRoot;
use crate::file_structure::StateStore;
use crate::oci::client::error::ClientError;
use crate::oci::client::{Client, OciTransport};
use crate::oci::index::{Index, IndexOperation, SelectResult};
use crate::oci::referrer::capability::{ReferrersApiCapability, ReferrersSupport};
use crate::oci::referrer::media_types::SIGSTORE_BUNDLE_V03;
use crate::oci::sign::bundle::{MAX_BUNDLE_SIZE_BYTES, parse_bundle};
use crate::oci::sign::rekor::set_signing_payload;
use crate::oci::{Digest, Identifier, Platform, native};
use sigstore_protobuf_specs::dev::sigstore::bundle::v1::{bundle, verification_material};

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
    /// True when the signing cert had expired by verify time but Rekor SET
    /// attests it was integrated pre-expiry.
    pub cert_expired_but_tlog_valid: bool,
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
                return Err(VerifyErrorKind::NoSignaturesFound);
            }
        };
        let subject_digest = resolved.digest().ok_or(VerifyErrorKind::NoSignaturesFound)?;
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
            match Self::verify_one_referrer(transport, &ctx, &descriptor, referrer_bytes, &subject_digest, &image).await
            {
                Ok(result) => return Ok(result),
                Err(kind) => merge_failure(&mut best_error, kind),
            }
        }
        Err(aggregate_failure(total_candidates, examined, best_error))
    }

    /// Verify a single signature-referrer candidate end-to-end from its
    /// already-fetched manifest bytes: parse → bundle blob → cert chain →
    /// subject binding → signature → Rekor SET → transparency-body binding →
    /// identity/policy → cache. Returns [`VerifyResult`] on full success; any
    /// failure is one candidate's verdict, which the ANY-of loop aggregates.
    ///
    /// The referrer manifest is fetched (and its read bounded) by the caller so
    /// the cross-candidate byte budget is charged from bytes actually read.
    async fn verify_one_referrer(
        transport: &dyn OciTransport,
        ctx: &VerifyContext<'_>,
        descriptor: &crate::oci::Descriptor,
        referrer_bytes: Vec<u8>,
        subject_digest: &Digest,
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

        // Verify the Fulcio cert chain against the trust root.
        verify_cert_chain(&parts.leaf_der, ctx.trust_root)?;

        // Bind the message signature to the subject digest (GHSA-whqx class).
        let subject_raw = hex::decode(subject_digest.hex()).map_err(|_| VerifyErrorKind::BundleParseFailed)?;
        if parts.message_digest != subject_raw {
            return Err(VerifyErrorKind::SignatureInvalid);
        }

        // Verify the ECDSA signature over the subject digest with the leaf key.
        verify_signature(&parts.leaf_der, &subject_raw, &parts.signature)?;

        // Verify the Rekor SET against the log's public key — pinned from the
        // trust root when present (offline-capable, closes the TOFU hole),
        // otherwise fetched online. Returns the key PEM used, so a successful
        // online run can cache the trust material for later offline verifies.
        let rekor_key_pem = verify_rekor_set(ctx, &parts).await?;

        // Bind the Rekor transparency-log body to THIS bundle. The SET only
        // attests that the logged body was integrated — not that the body
        // describes this bundle. Without this cross-check a leaked expired
        // ephemeral key can splice a previously-valid SET/body onto a new
        // malicious subject and every independent check still passes
        // (GHSA-whqx-f9j3-ch6m class).
        verify_transparency_body_binding(&parts, subject_digest)?;

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
            // Certificate temporal validity (the leaf's notBefore/notAfter vs the
            // Rekor integrated time) is NOT yet checked, so this stays false.
            // Deferred to production hardening — see signing.md "Current
            // Limitations". A cert that had expired by verify time is not rejected
            // on that basis today.
            cert_expired_but_tlog_valid: false,
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
                let _ = probed.write_cache(ctx.state).await;
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
    signature: Vec<u8>,
    message_digest: Vec<u8>,
    signed_entry_timestamp: Vec<u8>,
    canonicalized_body: Vec<u8>,
    integrated_time: u64,
    log_index: u64,
    log_id_hex: String,
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

        let message_signature = match bundle.content.as_ref() {
            Some(bundle::Content::MessageSignature(sig)) => sig,
            // A DSSE envelope is an attestation, not an artifact signature — v1 verify
            // handles only message signatures (attestation verify is #198).
            _ => return Err(VerifyErrorKind::NoUsableBundle),
        };
        let message_digest = message_signature
            .message_digest
            .as_ref()
            .map(|d| d.digest.clone())
            .ok_or(VerifyErrorKind::BundleParseFailed)?;

        let tlog = material.tlog_entries.first().ok_or(VerifyErrorKind::RekorSetInvalid)?;
        let set = tlog
            .inclusion_promise
            .as_ref()
            .map(|p| p.signed_entry_timestamp.clone())
            .ok_or(VerifyErrorKind::RekorSetAbsentTsaPresent)?;
        let log_id_hex = tlog.log_id.as_ref().map(|l| hex::encode(&l.key_id)).unwrap_or_default();

        Ok(Self {
            leaf_der,
            signature: message_signature.signature.clone(),
            message_digest,
            signed_entry_timestamp: set,
            canonicalized_body: tlog.canonicalized_body.clone(),
            integrated_time: tlog.integrated_time.max(0) as u64,
            log_index: tlog.log_index.max(0) as u64,
            log_id_hex,
        })
    }
}

/// Verify the leaf certificate is signed by one of the trust-root CAs.
fn verify_cert_chain(leaf_der: &[u8], trust_root: &TrustRoot) -> Result<(), VerifyErrorKind> {
    use p256::pkcs8::DecodePublicKey;
    use x509_cert::der::{Decode, Encode};

    let leaf = x509_cert::Certificate::from_der(leaf_der).map_err(|_| VerifyErrorKind::CertChainInvalid)?;
    let tbs_der = leaf
        .tbs_certificate
        .to_der()
        .map_err(|_| VerifyErrorKind::CertChainInvalid)?;
    let signature =
        p256::ecdsa::Signature::from_der(leaf.signature.raw_bytes()).map_err(|_| VerifyErrorKind::CertChainInvalid)?;

    for ca_der in trust_root.der_certs() {
        let Ok(ca) = x509_cert::Certificate::from_der(ca_der) else {
            continue;
        };
        let Ok(spki_der) = ca.tbs_certificate.subject_public_key_info.to_der() else {
            continue;
        };
        let Ok(ca_key) = p256::ecdsa::VerifyingKey::from_public_key_der(&spki_der) else {
            continue;
        };
        if ca_key.verify(&tbs_der, &signature).is_ok() {
            return Ok(());
        }
    }
    Err(VerifyErrorKind::CertChainInvalid)
}

/// Verify the ECDSA-P256 signature over `subject_raw` with the leaf's key.
fn verify_signature(leaf_der: &[u8], subject_raw: &[u8], signature_der: &[u8]) -> Result<(), VerifyErrorKind> {
    use p256::pkcs8::DecodePublicKey;
    use x509_cert::der::{Decode, Encode};

    let leaf = x509_cert::Certificate::from_der(leaf_der).map_err(|_| VerifyErrorKind::CertChainInvalid)?;
    let spki_der = leaf
        .tbs_certificate
        .subject_public_key_info
        .to_der()
        .map_err(|_| VerifyErrorKind::SignatureInvalid)?;
    let key =
        p256::ecdsa::VerifyingKey::from_public_key_der(&spki_der).map_err(|_| VerifyErrorKind::SignatureInvalid)?;
    let signature = p256::ecdsa::Signature::from_der(signature_der).map_err(|_| VerifyErrorKind::SignatureInvalid)?;
    key.verify_prehash(subject_raw, &signature)
        .map_err(|_| VerifyErrorKind::SignatureInvalid)
}

/// Verify the SET over the entry payload, returning the Rekor key PEM used.
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
    use ed25519_dalek::pkcs8::DecodePublicKey;

    let pem = match ctx.trust_root.rekor_public_key_pem() {
        Some(pinned) => pinned.to_string(),
        None if ctx.offline => return Err(VerifyErrorKind::RekorUnavailable),
        None => fetch_rekor_public_key_pem(ctx.rekor_url).await?,
    };
    let rekor_key =
        ed25519_dalek::VerifyingKey::from_public_key_pem(&pem).map_err(|_| VerifyErrorKind::RekorSetInvalid)?;

    let payload = set_signing_payload(
        &parts.canonicalized_body,
        parts.integrated_time,
        parts.log_index,
        &parts.log_id_hex,
    );
    let signature = ed25519_dalek::Signature::from_slice(&parts.signed_entry_timestamp)
        .map_err(|_| VerifyErrorKind::RekorSetInvalid)?;
    rekor_key
        .verify_strict(&payload, &signature)
        .map_err(|_| VerifyErrorKind::RekorSetInvalid)?;
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
    response.text().await.map_err(|_| VerifyErrorKind::RekorUnavailable)
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

    // A fresh, content-equal entry needs no rewrite — leave its TTL alone.
    if let Ok(Some(existing)) = TrustRootCache::from_cache(&cache_key, ctx.state).await
        && existing.fulcio_der_certs == der_certs
        && existing.rekor_public_key_pem.as_deref() == Some(rekor_key_pem.as_str())
    {
        return;
    }

    let entry = TrustRootCache::new(cache_key, der_certs, rekor_key_pem);
    if let Err(e) = entry.write_cache(ctx.state).await {
        tracing::debug!("trust-root cache write skipped: {e}");
    }
}

// ── Transparency-log body binding (GHSA-whqx-f9j3-ch6m class) ─────────────────

/// The hashedrekord entry body fields the verify pipeline binds to the bundle.
///
/// The Rekor `canonicalizedBody` is the exact `hashedrekord` proposal JSON that
/// was uploaded and signed by the SET (see [`super::super::sign::rekor`]). Only
/// the fields cross-checked against the bundle are deserialized; unknown fields
/// are ignored.
#[derive(serde::Deserialize)]
struct HashedRekordBody {
    spec: HashedRekordBodySpec,
}

#[derive(serde::Deserialize)]
struct HashedRekordBodySpec {
    signature: HashedRekordBodySignature,
    data: HashedRekordBodyData,
}

#[derive(serde::Deserialize)]
struct HashedRekordBodySignature {
    /// Base64 of the DER-encoded signature.
    content: String,
    #[serde(rename = "publicKey")]
    public_key: HashedRekordBodyPublicKey,
}

#[derive(serde::Deserialize)]
struct HashedRekordBodyPublicKey {
    /// Base64 of the leaf certificate PEM.
    content: String,
}

#[derive(serde::Deserialize)]
struct HashedRekordBodyData {
    hash: HashedRekordBodyHash,
}

#[derive(serde::Deserialize)]
struct HashedRekordBodyHash {
    /// Hex of the subject digest that was logged.
    value: String,
}

/// Bind the Rekor transparency-log body to the bundle it is attached to.
///
/// [`verify_rekor_set`] proves the SET signs the opaque `canonicalizedBody`, but
/// not that the body describes THIS bundle. Parse the `hashedrekord` body and
/// assert its logged subject digest, signature, and certificate equal the
/// bundle's — closing the expired-key SET/body splice where SET, signature, and
/// cert chain each verify independently against a mismatched subject.
fn verify_transparency_body_binding(parts: &BundleParts, subject_digest: &Digest) -> Result<(), VerifyErrorKind> {
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD;

    let body: HashedRekordBody =
        serde_json::from_slice(&parts.canonicalized_body).map_err(|_| VerifyErrorKind::TransparencyBodyMismatch)?;

    // 1. The logged subject digest must equal the digest the signature verified over.
    if !body.spec.data.hash.value.eq_ignore_ascii_case(subject_digest.hex()) {
        return Err(VerifyErrorKind::TransparencyBodyMismatch);
    }

    // 2. The logged signature must be the bundle's message signature.
    let body_signature = b64
        .decode(body.spec.signature.content.as_bytes())
        .map_err(|_| VerifyErrorKind::TransparencyBodyMismatch)?;
    if body_signature != parts.signature {
        return Err(VerifyErrorKind::TransparencyBodyMismatch);
    }

    // 3. The logged certificate must be the verified leaf certificate.
    let body_cert_pem = b64
        .decode(body.spec.signature.public_key.content.as_bytes())
        .map_err(|_| VerifyErrorKind::TransparencyBodyMismatch)?;
    let body_cert_der = pem::parse(&body_cert_pem)
        .map(pem::Pem::into_contents)
        .map_err(|_| VerifyErrorKind::TransparencyBodyMismatch)?;
    if body_cert_der != parts.leaf_der {
        return Err(VerifyErrorKind::TransparencyBodyMismatch);
    }

    Ok(())
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
    use p256::ecdsa::SigningKey;
    use p256::ecdsa::signature::hazmat::PrehashSigner;
    use p256::elliptic_curve::rand_core::OsRng;
    use sigstore_protobuf_specs::dev::sigstore::bundle::v1::Bundle;
    use sigstore_protobuf_specs::dev::sigstore::common::v1::{
        HashAlgorithm, HashOutput, LogId, MessageSignature, X509Certificate, X509CertificateChain,
    };
    use sigstore_protobuf_specs::dev::sigstore::rekor::v1::{InclusionPromise, TransparencyLogEntry};

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

    fn trust_root_of(certs: &[&[u8]]) -> TrustRoot {
        let pem: String = certs
            .iter()
            .map(|der| pem::encode(&pem::Pem::new("CERTIFICATE", der.to_vec())))
            .collect::<Vec<_>>()
            .join("\n");
        TrustRoot::load_from_pem(pem.as_bytes()).expect("trust root")
    }

    #[test]
    fn cert_chain_accepts_leaf_signed_by_trust_root_ca() {
        let (_key, cert) = self_signed_cert();
        let root = trust_root_of(&[&cert]);
        assert!(verify_cert_chain(&cert, &root).is_ok());
    }

    #[test]
    fn cert_chain_rejects_leaf_not_signed_by_any_trust_root_ca() {
        let (_key, leaf) = self_signed_cert();
        let (_other_key, other) = self_signed_cert();
        let root = trust_root_of(&[&other]);
        assert!(matches!(
            verify_cert_chain(&leaf, &root),
            Err(VerifyErrorKind::CertChainInvalid)
        ));
    }

    #[test]
    fn cert_chain_rejects_garbage_leaf() {
        let (_key, cert) = self_signed_cert();
        let root = trust_root_of(&[&cert]);
        assert!(matches!(
            verify_cert_chain(b"not a certificate", &root),
            Err(VerifyErrorKind::CertChainInvalid)
        ));
    }

    #[test]
    fn signature_over_subject_digest_round_trips_and_rejects_tampering() {
        let (key, cert) = self_signed_cert();
        let subject = [7u8; 32];
        let signature: p256::ecdsa::Signature = key.sign_prehash(&subject).expect("sign");
        let sig_der = signature.to_der();

        assert!(verify_signature(&cert, &subject, sig_der.as_bytes()).is_ok());

        // A different subject digest under the same signature must fail.
        let other_subject = [9u8; 32];
        assert!(matches!(
            verify_signature(&cert, &other_subject, sig_der.as_bytes()),
            Err(VerifyErrorKind::SignatureInvalid)
        ));
        // Garbage signature bytes fail closed.
        assert!(matches!(
            verify_signature(&cert, &subject, b"garbage"),
            Err(VerifyErrorKind::SignatureInvalid)
        ));
    }

    fn message_bundle(with_material: bool, with_tlog: bool) -> Bundle {
        use sigstore_protobuf_specs::dev::sigstore::bundle::v1::{VerificationMaterial, bundle, verification_material};
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
                    inclusion_proof: None,
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
    fn from_bundle_extracts_message_signature_parts() {
        let bundle = message_bundle(true, true);
        let parts = BundleParts::from_bundle(&bundle).expect("valid message bundle");
        assert_eq!(parts.integrated_time, 100);
        assert_eq!(parts.log_index, 5);
        assert_eq!(parts.log_id_hex, "ab");
        assert_eq!(parts.signature, vec![2, 3, 4]);
    }

    /// Build `BundleParts` whose `canonicalized_body` is a real `hashedrekord`
    /// proposal binding `subject`, its `signature`, and `cert_der`.
    fn parts_for(subject: &Digest, signature_der: &[u8], cert_der: &[u8]) -> BundleParts {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD;
        let cert_pem = pem::encode(&pem::Pem::new("CERTIFICATE", cert_der.to_vec()));
        let body = serde_json::json!({
            "kind": "hashedrekord",
            "apiVersion": "0.0.1",
            "spec": {
                "signature": {
                    "content": b64.encode(signature_der),
                    "publicKey": { "content": b64.encode(cert_pem.as_bytes()) }
                },
                "data": { "hash": { "algorithm": "sha256", "value": subject.hex() } }
            }
        });
        BundleParts {
            leaf_der: cert_der.to_vec(),
            signature: signature_der.to_vec(),
            message_digest: hex::decode(subject.hex()).expect("hex"),
            signed_entry_timestamp: Vec::new(),
            canonicalized_body: serde_json::to_vec(&body).expect("body json"),
            integrated_time: 0,
            log_index: 0,
            log_id_hex: String::new(),
        }
    }

    #[test]
    fn transparency_body_binding_accepts_matching_body() {
        let (key, cert_der) = self_signed_cert();
        let subject = crate::oci::Algorithm::Sha256.hash(b"subject manifest");
        let signature: p256::ecdsa::Signature = key
            .sign_prehash(&hex::decode(subject.hex()).expect("hex"))
            .expect("sign");
        let sig_der = signature.to_der().as_bytes().to_vec();
        let parts = parts_for(&subject, &sig_der, &cert_der);
        assert!(verify_transparency_body_binding(&parts, &subject).is_ok());
    }

    #[test]
    fn transparency_body_binding_rejects_spliced_subject() {
        // The GHSA-whqx class: a body/SET that is internally valid but attached
        // to a DIFFERENT subject than the one the signature verified over must be
        // rejected — otherwise an expired-key splice passes every other check.
        let (key, cert_der) = self_signed_cert();
        let subject = crate::oci::Algorithm::Sha256.hash(b"honest manifest");
        let signature: p256::ecdsa::Signature = key
            .sign_prehash(&hex::decode(subject.hex()).expect("hex"))
            .expect("sign");
        let sig_der = signature.to_der().as_bytes().to_vec();
        let parts = parts_for(&subject, &sig_der, &cert_der);

        let malicious_subject = crate::oci::Algorithm::Sha256.hash(b"malicious manifest");
        assert!(matches!(
            verify_transparency_body_binding(&parts, &malicious_subject),
            Err(VerifyErrorKind::TransparencyBodyMismatch)
        ));
    }

    #[test]
    fn transparency_body_binding_rejects_mismatched_signature() {
        // The logged signature differs from the bundle's message signature.
        let (key, cert_der) = self_signed_cert();
        let subject = crate::oci::Algorithm::Sha256.hash(b"subject manifest");
        let signature: p256::ecdsa::Signature = key
            .sign_prehash(&hex::decode(subject.hex()).expect("hex"))
            .expect("sign");
        let sig_der = signature.to_der().as_bytes().to_vec();
        let mut parts = parts_for(&subject, &sig_der, &cert_der);
        // Swap the bundle's signature so it no longer matches the logged body.
        parts.signature = vec![0xde, 0xad, 0xbe, 0xef];
        assert!(matches!(
            verify_transparency_body_binding(&parts, &subject),
            Err(VerifyErrorKind::TransparencyBodyMismatch)
        ));
    }

    #[test]
    fn transparency_body_binding_rejects_garbage_body() {
        let (_key, cert_der) = self_signed_cert();
        let subject = crate::oci::Algorithm::Sha256.hash(b"subject manifest");
        let mut parts = parts_for(&subject, &[1, 2, 3], &cert_der);
        parts.canonicalized_body = b"not json at all".to_vec();
        assert!(matches!(
            verify_transparency_body_binding(&parts, &subject),
            Err(VerifyErrorKind::TransparencyBodyMismatch)
        ));
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
        let image: native::Reference = "ghcr.io/acme/tool:1.0".parse().expect("tagged reference");
        let derived = image.clone_with_digest(format!("sha256:{}", "a".repeat(64)));
        assert_eq!(derived.tag(), None, "digest-addressed ref must carry no tag");
        assert_eq!(derived.registry(), "ghcr.io", "host must survive");
        assert_eq!(derived.repository(), "acme/tool", "repository must survive");
    }

    // ── Index indirection: transport traffic follows the PHYSICAL registry ──

    /// SHA-256 the indirecting test index reports as the subject digest.
    fn indirection_subject_digest() -> Digest {
        crate::oci::Algorithm::Sha256.hash(b"indirected subject manifest")
    }

    /// A test index whose logical name resolves to a DIFFERENT physical
    /// registry — the `index.ocx.sh` shape (`ocx.sh/<ns>/<pkg>` pointing at
    /// `oci://ghcr.io/<org>/<repo>`) reduced to what the pipeline consumes.
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
            Ok((b"{}".to_vec(), indirection_subject_digest().to_string()))
        }

        async fn pull_blob(&self, _: &native::Reference, _: &Digest) -> std::result::Result<Vec<u8>, ClientError> {
            unimplemented!("verify streams blobs")
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
            Ok(Vec::new())
        }

        fn box_clone(&self) -> Box<dyn OciTransport> {
            Box::new(self.clone())
        }
    }

    /// Drive a verify run against the recording transport for the logical name
    /// `ocx.sh/acme/tool:1.0`, indirected to the physical `ghcr.io/acme/tool:1.0`,
    /// and return the `"<method>:<registry>"` log. The transport lists no
    /// referrers, so the run must end in `NoSignaturesFound`.
    async fn run_recorded_verify(mirrors: crate::oci::client::MirrorMap) -> Vec<String> {
        let logical = Identifier::parse("ocx.sh/acme/tool:1.0").expect("logical identifier");
        let physical = Identifier::parse("ghcr.io/acme/tool:1.0").expect("physical identifier");

        let transport = RecordingTransport::default();
        let mut client = Client::with_transport(Box::new(transport.clone()));
        client.mirrors = mirrors;
        let index = Index::from_impl(IndirectingIndex { physical });
        let temp = tempfile::TempDir::new().expect("state dir");
        let state = StateStore::new(temp.path());
        let (_key, cert) = self_signed_cert();
        let trust_root = trust_root_of(&[&cert]);
        let rekor_url = Url::parse("https://rekor.example").expect("rekor url");
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
        let Err(error) = outcome else {
            panic!("the recording transport lists no referrers, so verify must fail");
        };
        assert!(
            matches!(error.kind, VerifyErrorKind::NoSignaturesFound),
            "expected the empty-referrer outcome, got: {error}",
        );
        transport.calls()
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
        let calls = run_recorded_verify(crate::oci::client::MirrorMap::default()).await;
        assert!(
            calls.iter().any(|call| call == "list_referrers:ghcr.io"),
            "the referrer listing must target the physical registry, got: {calls:?}",
        );
        assert!(
            calls.iter().all(|call| call.ends_with(":ghcr.io")),
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
            "ghcr.io".to_string(),
            crate::config::mirror::ParsedMirror {
                protocol: "https".to_string(),
                host: "mirror.example".to_string(),
                path_prefix: "proxy".to_string(),
            },
        )]);
        let calls = run_recorded_verify(mirrors).await;

        assert!(
            calls.iter().any(|call| call == "list_referrers:mirror.example"),
            "referrer reads must follow the mirror, got: {calls:?}",
        );
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
    //   deliberately leaves `list_referrers` `unimplemented!()`, and reproducing
    //   the crypto material in Rust would duplicate the whole `fake_sigstore.py`
    //   stack. Those cases are covered end-to-end in the acceptance suite against
    //   the fake stack (`test/tests/test_verify.py`, `test_auto_verify.py`); the
    //   pure body/SET-binding splice is unit-covered by the
    //   `transparency_body_binding_*` tests above.
}
