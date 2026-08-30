// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Reading cosign's *simplesigning* sidecar — the pre-OCI-1.1 shape a
//! `sha256-<hex>.sig` / `.att` / `.sbom` tag holds.
//!
//! A sidecar is an ordinary OCI image manifest whose **layers** are
//! simplesigning payloads ([`crate::oci::simplesigning`]), one per signature,
//! each carrying its verification material in *annotations* rather than in a
//! bundle blob:
//!
//! | Annotation | Carries |
//! |---|---|
//! | [`ANNOTATION_COSIGN_SIGNATURE`] | base64 signature over the payload bytes |
//! | [`ANNOTATION_COSIGN_CERTIFICATE`] | PEM leaf certificate (keyless only) |
//! | [`ANNOTATION_COSIGN_CHAIN`] | PEM intermediates (keyless only) |
//! | [`ANNOTATION_COSIGN_BUNDLE`] | offline Rekor bundle (see the gap below) |
//!
//! # A parsing difference, never a trust difference
//!
//! Three shapes are legal and none is malformed input:
//!
//! * **Key mode** — signature only. Verified against the applicable
//!   [`PolicyBackend::Key`](crate::trust::PolicyBackend::Key), exactly as a
//!   key-mode bundle is.
//! * **Keyless** — certificate present. The **identical** keyless gate runs:
//!   chain to the trust root, embedded SCT against the CT log keys, the
//!   signature, then SAN + OIDC issuer against the trust policies.
//! * **Keyless with no transparency material** — the shape cosign v3.1.1
//!   actually writes, because `attach signature --rekor-response` validates its
//!   argument and never emits the bundle annotation. It is **refused**
//!   ([`VerifyErrorKind::SignatureInvalid`]) unless the caller passes
//!   `--allow-unlogged-signature`; see [`verify_keyless`] for the ordering and
//!   the reasoning. cosign refuses it too, and needs `--insecure-ignore-tlog`
//!   to accept it.
//!
//! # Where the cryptography lives
//!
//! Nowhere here. The keyless gate is
//! [`sigstore::bundle::verify::Verifier`] — the same verifier
//! [`super::pipeline`] hands a bundle to — and the key gate is
//! [`super::identity::matching_key_policies`]. This module owns no X.509, no
//! ASN.1 and no signature code; what it owns is the sidecar's *shape*.
//!
//! # Trust boundary: the signature covers the bytes as served
//!
//! Every signature check runs over the **raw layer bytes the registry
//! returned**. A parsed [`SimpleSigningClaim`] is read for its two `critical`
//! fields and is never re-serialized to reconstruct the signed payload — a
//! round trip is not guaranteed byte-identical, and a reconstruction that
//! differed would be a silent verification bypass (the type's own note).
//!
//! # Transparency-log evidence is the keyless gate
//!
//! A keyless signature's certificate lives about ten minutes. *When* it was
//! used is therefore the only thing separating a live signature from a stale
//! certificate replayed for ever, and the transparency log is the only place
//! that answer comes from. So on the keyless arm the
//! [`ANNOTATION_COSIGN_BUNDLE`] annotation is **required and checked**
//! ([`logged_entry`]): its Signed Entry Timestamp is verified against the log's
//! own public key, its logged body is bound to this signature over this
//! payload, and only then does its `integratedTime` become the instant both
//! this module and `sigstore` judge the certificate's window against.
//!
//! This reverses the G1 contract, by owner decision. That contract declared the
//! no-annotation shape legal and anchored its window on the certificate's own
//! `notBefore` — circular, and vacuous three times over (see
//! [`super::signing_instant`]). What replaced it is a refusal by default plus
//! one explicit opt-out for air-gapped CI.
//!
//! # Reported gaps
//!
//! * **The key arm does not read [`ANNOTATION_COSIGN_BUNDLE`].** `ocx package
//!   sign --key` uploads to Rekor only when asked (D10), so a key-mode sidecar
//!   usually carries no annotation, and a key signature's trust story is a
//!   committed public key rather than a signing instant. Crediting it there
//!   would add a `signed_at` nothing needs; the keyless arm is where the entry
//!   is *required*, which is where reading it has to be right.
//! * **No online Rekor lookup.** Evidence comes from the offline annotation or
//!   not at all: nothing here searches Rekor for an entry a sidecar failed to
//!   carry. cosign's own `attach signature` never writes one, so a lookup would
//!   only ever rescue artifacts cosign itself refuses.
//! * **There is no `.att` artifact type to discover by, and none was
//!   missing from G1.** Measured against cosign v3.1.1: `cosign attest` writes
//!   a [`SIGSTORE_BUNDLE_V03`] referrer — the *same* type a signature referrer
//!   carries — and the `.att` manifest it can still be made to write carries
//!   neither `artifactType` nor `subject`, so no listing reaches it. `.att` is
//!   a tag-only shape and [`super::attestation_sidecar`] is its reader.
//! * **The `.sbom` sidecar *tag* is read elsewhere, and [`SidecarKind`] still
//!   does not name it.** Only `.sig` and `.att` have variants —
//!   [`SidecarKind::Signature`] is handed to [`read_sidecar_tag`] here,
//!   [`SidecarKind::Attestation`] to its sibling — and `.sbom`'s absence is
//!   structural rather than an omission:
//!   [`read_sidecar_manifest`] examines layers whose media type is
//!   [`SIMPLESIGNING_MEDIA_TYPE`] and skips every other, while a cosign `.sbom`
//!   layer keeps the SBOM document's own type
//!   (`cosign attach sbom` — see [`COSIGN_SBOM_ARTIFACT_TYPE`]). Aimed at it
//!   this reader would return an empty scan for *every* sidecar that exists, so
//!   a variant here would be read as coverage it cannot deliver. The reader that
//!   *does* exist is a **document** reader on the permissive listing path
//!   ([`super::pipeline`]'s `scan_unverified` and its `read_sbom_sidecar_tag`),
//!   where an unsigned SBOM belongs — not a variant plus a second call site
//!   here. `golden/sbom_sidecar_manifest.json` is the committed capture it is
//!   built against.
//!
//! [`ANNOTATION_COSIGN_SIGNATURE`]: crate::oci::referrer::media_types::ANNOTATION_COSIGN_SIGNATURE
//! [`ANNOTATION_COSIGN_CERTIFICATE`]: crate::oci::referrer::media_types::ANNOTATION_COSIGN_CERTIFICATE
//! [`ANNOTATION_COSIGN_CHAIN`]: crate::oci::referrer::media_types::ANNOTATION_COSIGN_CHAIN
//! [`ANNOTATION_COSIGN_BUNDLE`]: crate::oci::referrer::media_types::ANNOTATION_COSIGN_BUNDLE
//! [`COSIGN_SIG_ARTIFACT_TYPE`]: crate::oci::referrer::media_types::COSIGN_SIG_ARTIFACT_TYPE
//! [`COSIGN_SBOM_ARTIFACT_TYPE`]: crate::oci::referrer::media_types::COSIGN_SBOM_ARTIFACT_TYPE
//! [`SIGSTORE_BUNDLE_V03`]: crate::oci::referrer::media_types::SIGSTORE_BUNDLE_V03

use base64::Engine as _;
use sigstore::bundle::verify::Verifier;
use sigstore::rekor::models::hashedrekord;
use sigstore_protobuf_specs::dev::sigstore::bundle::v1::{Bundle, VerificationMaterial, bundle, verification_material};
use sigstore_protobuf_specs::dev::sigstore::common::v1::{MessageSignature, X509Certificate, X509CertificateChain};
use sigstore_protobuf_specs::dev::sigstore::rekor::v1::{InclusionPromise, TransparencyLogEntry};
use url::Url;
use x509_cert::der::EncodePem as _;

use super::discovery::DiscoveryMethod;
use super::error::VerifyErrorKind;
use super::identity::{self, matching_policies, oidc_issuer, parse_certificate, subject_identity};
use super::pipeline::{
    ACCEPTED_MANIFEST_TYPES, MAX_REFERRER_MANIFEST_BYTES, MAX_SIGNATURE_CANDIDATES, PolicyDeferredToOcx,
    RefusedCandidate, RekorKeyMemo, VerifiedSignature, VerifyResult, map_client_error, map_verification_error,
    pull_blob_capped,
};
use super::signing_instant::SigningInstant;
use super::tlog;
use super::trust_root::TrustRoot;
use crate::oci::client::error::ClientError;
use crate::oci::client::{OciTransport, sibling_tag_reference};
use crate::oci::referrer::media_types::{
    ANNOTATION_COSIGN_BUNDLE, ANNOTATION_COSIGN_CERTIFICATE, ANNOTATION_COSIGN_CHAIN, ANNOTATION_COSIGN_SIGNATURE,
    SIMPLESIGNING_MEDIA_TYPE,
};
use crate::oci::sign::{KeyBackendKind, SignatureFormat};
use crate::oci::simplesigning::{SIMPLESIGNING_CLAIM_TYPE, SimpleSigningClaim};
use crate::oci::{Descriptor, Digest, ImageManifest, native};
use crate::trust::CompiledPolicy;

/// The Sigstore bundle 0.1 profile media type.
///
/// Declared here because `sigstore` 0.14 keeps its `bundle::models::Version`
/// enum private, so there is no symbol to name. Not an unchecked literal: it is
/// the one profile whose structural check a log entry with no Merkle proof
/// satisfies, so the whole keyless sidecar path routes through it — a drifted
/// spelling reads as `BundleProfileErrorKind::Unknown` and reds every keyless
/// test in this module with `bundle_parse_failed`.
pub(super) const SIGSTORE_BUNDLE_V01_MEDIA_TYPE: &str = "application/vnd.dev.sigstore.bundle+json;version=0.1";

/// Maximum accepted size of one simplesigning payload layer, in bytes.
///
/// cosign's own payloads are ~256 bytes (the committed golden fixtures are 255
/// and 259). The layer digest comes from an **untrusted** sidecar manifest, so
/// digest verification does not bound size: an over-cap descriptor is rejected
/// before a connection is opened, and the read itself is bounded again by
/// [`pull_blob_capped`] so a registry lying about the size still cannot force an
/// unbounded allocation (CWE-400). 64 KiB is generous headroom for a publisher
/// that fills `optional`.
const MAX_SIMPLESIGNING_PAYLOAD_BYTES: usize = 64 * 1024;

/// Which cosign sidecar a tag names.
///
/// Both are *tag suffixes* on the truncated-digest tag, needing no
/// artifact-type constant — which is the whole reason `.att` is readable at
/// all: cosign publishes no attestation artifact type, so the tag is the only
/// way in (see [`super::attestation_sidecar`]).
///
/// cosign's third suffix, `.sbom`, has **no variant here**: an enum that named
/// it would be read as a reader that reaches it, and none exists (see the
/// module doc's "Reported gaps").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidecarKind {
    /// `sha256-<hex>.sig` — image signatures.
    Signature,
    /// `sha256-<hex>.att` — attestations.
    Attestation,
}

impl SidecarKind {
    /// The tag suffix, including the leading dot.
    ///
    /// Taken from [`crate::package::tag`], which is where the classifier that
    /// refuses to read these back as package versions spells them: one literal
    /// per suffix, so this reader cannot ask for a name the classifier stopped
    /// reserving.
    pub const fn suffix(self) -> &'static str {
        match self {
            Self::Signature => crate::package::tag::SIG_SIDECAR_SUFFIX,
            Self::Attestation => crate::package::tag::ATT_SIDECAR_SUFFIX,
        }
    }
}

/// The cosign sidecar tag naming `subject`'s signatures of `kind`.
///
/// Derived from [`referrer_fallback_tag`](crate::package::tag::referrer_fallback_tag)
/// rather than formatted here, so this and the fallback-index writer cannot
/// disagree about the truncated-digest half; only the suffix is added.
pub fn sidecar_tag(subject: &Digest, kind: SidecarKind) -> String {
    let mut tag = crate::package::tag::referrer_fallback_tag(subject);
    tag.push_str(kind.suffix());
    tag
}

/// Everything a sidecar layer is judged against: the crypto, the policies, and
/// how transparency-log evidence is obtained and required.
///
/// One struct rather than six threaded parameters, because all three readers in
/// this module carry the identical set from door to layer and none of them acts
/// on it — three functions threading one tuple is a missing type. Built once per
/// scan in `pipeline::scan_simplesigning`.
///
/// Assembled there rather than borrowed whole from
/// [`VerifyContext`](super::pipeline::VerifyContext): this module needs six of
/// its fields, and a unit test verifying one layer should not have to fabricate
/// an `Index` and a `StateStore` to say "the local Rekor key is pinned here".
pub struct SidecarVerification<'a> {
    /// The `sigstore` verifier: chain to the trust root, embedded SCT, and the
    /// signature itself. The same one the bundle path is handed.
    pub verifier: &'a Verifier,
    /// The resolved ANY-of trust policies — a certificate's identity and issuer
    /// on the keyless arm, the pinned public keys on the key arm.
    pub policies: &'a [CompiledPolicy],
    /// Supplies the pinned Rekor public key when it has one — the only source
    /// an offline run can use.
    pub trust_root: &'a TrustRoot,
    /// Where an unpinned Rekor public key is fetched from, online.
    pub rekor_url: &'a Url,
    /// No Sigstore trust-services network: an unpinned key is a refusal.
    pub offline: bool,
    /// `--allow-unlogged-signature`. See
    /// [`VerifyContext::allow_unlogged_signature`](super::pipeline::VerifyContext).
    pub allow_unlogged: bool,
    /// The run's resolved Rekor log keys, shared with every other door this
    /// scan opens. Owned rather than borrowed because it is a handle: a clone
    /// is the same memo.
    pub rekor_keys: RekorKeyMemo,
}

/// What one sidecar manifest yielded: the layers that verified, and the ones
/// that were examined and refused.
///
/// Both travel out for the same reason [`super::pipeline::AttestationScan`]
/// carries both — one malformed layer must not be able to hide every valid
/// signature beside it, and a caller reporting "1 signature" when a second was
/// refused is reporting the less actionable half.
#[derive(Debug, Default)]
pub struct SidecarScan {
    /// Every simplesigning layer that verified, in manifest order.
    pub verified: Vec<VerifiedSignature>,
    /// Every simplesigning layer that was examined and refused, in manifest
    /// order. `referrer_digest` is the **layer** digest: one layer is one
    /// signature, and the manifest digest would name all of them at once.
    pub refused: Vec<RefusedCandidate>,
}

/// Fetch a cosign sidecar tag and verify every simplesigning layer it carries.
///
/// `Ok(None)` means the tag does not exist — "no sidecar", never an error: a
/// subject with no legacy signatures is the overwhelmingly common case, and a
/// registry 404 here says exactly that.
///
/// # Errors
///
/// [`VerifyErrorKind`] when the registry fails for any reason other than a
/// missing manifest, or when the sidecar manifest itself is over-cap or does
/// not parse. A *layer* failure is never an error here — it lands in
/// [`SidecarScan::refused`].
pub async fn read_sidecar_tag(
    transport: &dyn OciTransport,
    image: &native::Reference,
    subject_digest: &Digest,
    kind: SidecarKind,
    verify: &SidecarVerification<'_>,
    via: DiscoveryMethod,
) -> Result<Option<SidecarScan>, VerifyErrorKind> {
    let target = sibling_tag_reference(image, sidecar_tag(subject_digest, kind));
    let bytes = match transport.pull_manifest_raw(&target, ACCEPTED_MANIFEST_TYPES).await {
        Ok((bytes, _digest)) => bytes,
        Err(ClientError::ManifestNotFound(_)) => return Ok(None),
        Err(other) => return Err(map_client_error(other)),
    };
    if bytes.len() as u64 > MAX_REFERRER_MANIFEST_BYTES {
        return Err(VerifyErrorKind::BundleParseFailed);
    }
    read_sidecar_manifest(transport, image, &bytes, subject_digest, verify, via)
        .await
        .map(Some)
}

/// Verify every simplesigning layer of an already-fetched sidecar manifest.
///
/// The shared core behind both discovery doors: the `sha256-<hex>.sig` sidecar
/// tag ([`read_sidecar_tag`]) and an OCI 1.1 referrer whose `artifactType` is
/// [`COSIGN_SIG_ARTIFACT_TYPE`](crate::oci::referrer::media_types::COSIGN_SIG_ARTIFACT_TYPE).
/// Which door a manifest came through changes only what a caller reports as its
/// [`DiscoveryMethod`](super::DiscoveryMethod) — never how it is verified.
///
/// A layer whose media type is not [`SIMPLESIGNING_MEDIA_TYPE`] is **skipped**,
/// not refused: a sidecar legitimately carries other layers, and a manifest with
/// zero simplesigning layers simply contributes no candidates.
///
/// # Errors
///
/// [`VerifyErrorKind::BundleParseFailed`] when the manifest does not parse as an
/// OCI image manifest.
pub async fn read_sidecar_manifest(
    transport: &dyn OciTransport,
    image: &native::Reference,
    manifest_bytes: &[u8],
    subject_digest: &Digest,
    verify: &SidecarVerification<'_>,
    via: DiscoveryMethod,
) -> Result<SidecarScan, VerifyErrorKind> {
    let manifest: ImageManifest =
        serde_json::from_slice(manifest_bytes).map_err(|_| VerifyErrorKind::BundleParseFailed)?;

    let mut scan = SidecarScan::default();
    // Bounds the work a hostile registry can force by listing many layers, the
    // same ceiling the bundle scan applies to referrer candidates.
    let layers: Vec<&Descriptor> = manifest
        .layers
        .iter()
        .filter(|layer| layer.media_type == SIMPLESIGNING_MEDIA_TYPE)
        .collect();
    if layers.len() > MAX_SIGNATURE_CANDIDATES {
        // Never silent: a truncated scan has looked at fewer signatures than
        // the sidecar carries, and a caller reporting the survivors as the whole
        // set would understate it. The verdict is unaffected — one verified
        // signature is the ANY-of answer — but D-5's report must be able to say
        // so.
        tracing::debug!(
            "sidecar carries {} simplesigning layers; examining the first {MAX_SIGNATURE_CANDIDATES}",
            layers.len()
        );
    }
    for layer in layers.into_iter().take(MAX_SIGNATURE_CANDIDATES) {
        let payload = match pull_payload(transport, image, layer).await {
            Ok(payload) => payload,
            Err(kind) => {
                scan.refused.push(RefusedCandidate {
                    referrer_digest: layer.digest.clone(),
                    reason: kind,
                });
                continue;
            }
        };
        match verify_layer(layer, &payload, subject_digest, verify, via).await {
            Ok(result) => scan.verified.push(result),
            Err(reason) => scan.refused.push(RefusedCandidate {
                referrer_digest: layer.digest.clone(),
                reason,
            }),
        }
    }
    Ok(scan)
}

/// Pull one simplesigning payload under [`MAX_SIMPLESIGNING_PAYLOAD_BYTES`].
///
/// The declared descriptor size is untrusted, so it is only a cheap pre-fetch
/// reject; the read itself is bounded independently.
async fn pull_payload(
    transport: &dyn OciTransport,
    image: &native::Reference,
    layer: &Descriptor,
) -> Result<Vec<u8>, VerifyErrorKind> {
    if layer.size < 0 || layer.size as usize > MAX_SIMPLESIGNING_PAYLOAD_BYTES {
        return Err(VerifyErrorKind::BundleParseFailed);
    }
    let digest = Digest::try_from(layer.digest.as_str()).map_err(|_| VerifyErrorKind::BundleParseFailed)?;
    pull_blob_capped(transport, image, &digest, MAX_SIMPLESIGNING_PAYLOAD_BYTES).await
}

/// Verify one simplesigning layer against `subject_digest`.
///
/// `payload` is the layer body **exactly as the registry served it** — the
/// bytes every signature check below covers.
///
/// # Errors
///
/// One layer's verdict, never the sidecar's: the caller records it and keeps
/// going.
pub(super) async fn verify_layer(
    layer: &Descriptor,
    payload: &[u8],
    subject_digest: &Digest,
    verify: &SidecarVerification<'_>,
    via: DiscoveryMethod,
) -> Result<VerifiedSignature, VerifyErrorKind> {
    let layer_digest = Digest::try_from(layer.digest.as_str()).map_err(|_| VerifyErrorKind::BundleParseFailed)?;
    let signature = layer_signature(layer)?;

    // OCX's own structural checks run BEFORE any delegated call, so their
    // precise kinds are the ones a user sees — the ordering the bundle path
    // uses for the same reason.
    check_claim(payload, subject_digest)?;

    let signer = match layer_certificate(layer)? {
        Some(leaf_der) => {
            let logged = logged_entry(layer, verify).await?;
            verify_keyless(&leaf_der, layer_chain(layer)?, payload, &signature, logged, verify).await?
        }
        // Key mode: no certificate to chain, no SAN to read, no window to
        // bound. Not checks skipped — checks over material this shape does not
        // have, which is why both identity fields come out absent rather than
        // empty.
        //
        // The `dev.sigstore.cosign/bundle` annotation is not read here either,
        // and its absence from this arm is a declared gap rather than an
        // oversight: `ocx package sign --key` uploads to Rekor only when asked
        // (D10), so there is usually no annotation, and crediting one would add
        // a `signed_at` to a shape whose entire trust story is a committed
        // public key. The keyless arm is where the entry is *required*, which is
        // where reading it has to be right.
        None => {
            // The PAYLOAD bytes, never a re-serialized claim: cosign signs
            // `sha256(payload)` and the raw bytes are what the layer digest
            // addresses.
            identity::matching_key_policies(payload, &signature, verify.policies)?;
            VerifiedSigner {
                key_backend: KeyBackendKind::File,
                certificate_identity: None,
                certificate_oidc_issuer: None,
                logged: None,
            }
        }
    };

    Ok(VerifiedSignature {
        result: VerifyResult {
            subject_digest: subject_digest.clone(),
            referrer_digest: layer_digest,
            key_backend: signer.key_backend,
            certificate_identity: signer.certificate_identity,
            certificate_oidc_issuer: signer.certificate_oidc_issuer,
            // Only ever from an entry whose SET verified against the log's own
            // key and whose logged body was bound to this signature. `None` is
            // the key-mode arm and the `--allow-unlogged-signature` arm, where
            // nothing proved a signing time and reporting one would invent it.
            signed_at: signer
                .logged
                .as_ref()
                .and_then(|entry| u64::try_from(entry.integrated_time).ok()),
            signature_format: SignatureFormat::Simplesigning,
            discovery_method: via,
            // Same provenance rule, and it stayed load-bearing through the
            // change: the entry `sidecar_bundle` synthesises for `sigstore`'s
            // `CheckedBundle` carries `log_index: 0` and is never checked, so
            // its index must never reach here — it would publish a log position
            // nothing proved and key D6's dedup on a constant, collapsing every
            // unlogged sidecar signature into one row. What reaches here is the
            // *annotation's* index, and only after `logged_entry` verified it.
            rekor_log_index: signer.logged.as_ref().map(|entry| entry.log_index),
        },
        signature,
    })
}

/// The facts the two key models establish differently — the sidecar twin of the
/// bundle path's own split, kept so a swap of two adjacent `Option<String>`s
/// cannot type-check silently.
struct VerifiedSigner {
    key_backend: KeyBackendKind,
    certificate_identity: Option<String>,
    certificate_oidc_issuer: Option<String>,
    /// The verified transparency-log entry, when the layer carried one.
    logged: Option<LoggedEntry>,
}

/// A transparency-log entry a sidecar layer carried, **after** its Signed Entry
/// Timestamp verified against the log's own key.
///
/// Constructed only by [`logged_entry`]. Nothing else may build one — the fields
/// stay private for exactly that reason, and the accessors below are what
/// [`super::attestation_sidecar`] reads it through: the type is what separates
/// "the annotation said so" from "the log's own key said so", and a `pub(super)`
/// field would reduce that to a convention.
///
/// It is not yet *bound* to anything — [`bind_logged_body`] does that for a
/// `hashedrekord` body and [`super::dsse::verify_tlog_binding`] for a `dsse`
/// one, both deliberately later. `body` travels along for exactly that call.
#[derive(Debug, Clone)]
pub(super) struct LoggedEntry {
    integrated_time: i64,
    log_index: u64,
    /// The entry's `canonicalizedBody`, as the SET covered it.
    body: Vec<u8>,
}

impl LoggedEntry {
    /// The entry's `integratedTime`, in seconds since the Unix epoch.
    pub(super) const fn integrated_time(&self) -> i64 {
        self.integrated_time
    }

    /// The entry's position in the log.
    pub(super) const fn log_index(&self) -> u64 {
        self.log_index
    }

    /// The entry's `canonicalizedBody`, as the SET covered it.
    pub(super) fn body(&self) -> &[u8] {
        &self.body
    }
}

/// The full keyless gate over an annotation certificate.
///
/// Chain, SCT and signature are `sigstore`'s, reached through the same
/// [`Verifier`] the bundle path uses; identity and issuer are
/// [`matching_policies`]; the certificate-validity window is
/// [`tlog::verify_integrated_time_within_certificate`], anchored on the log
/// entry's `integratedTime`. Nothing is relaxed because the material arrived in
/// annotations rather than in a bundle blob.
///
/// # Transparency-log evidence is required, and the ordering says why
///
/// `logged` is `None` when the layer carried no `dev.sigstore.cosign/bundle`
/// annotation — the shape `cosign attach signature` writes, since its
/// `--rekor-response` is inert on v3.1.1. A keyless signature with nothing in
/// the transparency log has no provable signing instant: its Fulcio leaf lived
/// about ten minutes and expired long before anyone verifies, so *when* it was
/// used is the only question that separates a live signature from a stale
/// certificate replayed for ever. Without an entry the refusal is
/// [`VerifyErrorKind::SignatureInvalid`] — the signature could not be
/// established, which is what 65 says — unless `allow_unlogged` was passed.
///
/// That refusal comes **last**, after chain, SCT, signature and identity, and
/// deliberately: a sidecar signed by the wrong identity must still report
/// `identity_mismatch`, which is the more actionable verdict and the one an
/// operator can fix. Only an artifact that is right in every other respect gets
/// told the thing that is missing is the log entry.
///
/// The ordering claim is about the **missing-entry** refusal and only it. A
/// layer that carries an annotation which is malformed, whose base64 or SET
/// does not hold, or whose log key cannot be resolved is judged by
/// [`logged_entry`] *before* this function is entered at all, so it reports
/// `bundle_parse_failed` / `rekor_set_invalid` / `transparency_log_unavailable`
/// even when the identity is also wrong. That direction is harmless — every
/// such kind is a refusal, and each names material the operator controls — and
/// it is the price of anchoring the certificate window on the entry's own
/// `integratedTime`, which has to be in hand before the window is checked.
async fn verify_keyless(
    leaf_der: &[u8],
    chain_ders: Vec<Vec<u8>>,
    payload: &[u8],
    signature: &[u8],
    logged: Option<LoggedEntry>,
    verify: &SidecarVerification<'_>,
) -> Result<VerifiedSigner, VerifyErrorKind> {
    let cert = parse_certificate(leaf_der)?;
    // The instant `sigstore` anchors BOTH of its time checks on — it builds the
    // chain at this value and compares the certificate's window against it —
    // so it is the entry's `integratedTime` whenever one exists. Under the
    // opt-out there is no such value and `sigstore` still demands an entry to
    // hold the bundle together, so the certificate's own `notBefore` stands in
    // and the library's window check is vacuous. That vacuity is precisely what
    // `--allow-unlogged-signature` buys, is reachable no other way, and is why
    // this is a bare `i64` rather than a `SigningInstant`: the type exists to
    // stop a value nothing proved from being *called* a signing instant.
    let anchor = logged.as_ref().map_or_else(
        || i64::try_from(cert.tbs_certificate.validity.not_before.to_unix_duration().as_secs()).unwrap_or(i64::MAX),
        |entry| entry.integrated_time,
    );
    let bundle = sidecar_bundle(&cert, leaf_der, chain_ders, payload, signature, anchor)?;

    // Chain to the trust root, embedded SCT against the CT log keys, and the
    // ECDSA signature over `sha256(payload)` — all three inside this one call,
    // all three `sigstore`'s. `offline: true` because the entry, when there is
    // one, arrived in the annotation and was checked by `logged_entry` before
    // this call; see `sidecar_bundle` for what the synthesised entry does and
    // does not decide.
    // `verify` rather than `verify_digest`: the digest variant takes
    // `sigstore`'s own `sha2::Sha256` value, and that crate resolves a different
    // `sha2` semver line than ocx_lib does, so the type cannot be constructed
    // here. Handing it the payload slice as a reader has it compute the same
    // SHA-256 internally — one hash either way, and no version coupling.
    if let Err(error) = verify
        .verifier
        .verify(payload, bundle, &PolicyDeferredToOcx, true)
        .await
    {
        return Err(map_verification_error(error));
    }

    // Identity + issuer against the resolved trust policies (ANY-of), read back
    // off the leaf that just passed the chain, the SCT and the signature.
    matching_policies(leaf_der, verify.policies)?;

    match logged.as_ref() {
        Some(entry) => {
            // The binding, and it runs HERE rather than beside the SET check
            // that produced this entry. Both orders refuse a tampered
            // signature — every check has to pass — but only this one refuses
            // it as `signature_invalid`. Checked beside the SET, a flipped
            // signature annotation would report `transparency_body_mismatch`
            // ("the logged body does not bind to the bundle"), which points at
            // the log for a fault that is entirely in the bytes beside it, and
            // which the C-006 refusal contract does not name for a corrupted
            // signature. What reaches this line is a signature that already
            // verified under a chained, SCT-checked, identity-matched
            // certificate, so a mismatch here really is a spliced entry.
            bind_logged_body(&entry.body, payload, signature)?;
            // Row 13 (CVE-2024-55655), re-asserted here as it is on the bundle
            // path, and now against real evidence: the entry's
            // `integratedTime`, SET-checked and, one line above, bound to this
            // signature.
            tlog::verify_integrated_time_within_certificate(
                SigningInstant::TransparencyLog(entry.integrated_time),
                &cert,
            )?;
        }
        // The opt-out. The window check is SKIPPED rather than fed the
        // certificate's own `notBefore`: a check that asks the certificate when
        // it was valid and then judges the certificate against that answer can
        // never fail, and a call that can never fail reads as a gate while
        // being none. The caller said they accept a signature nothing
        // timestamps; this is that, stated once, instead of dressed up.
        None if verify.allow_unlogged => {
            tracing::debug!(
                "accepting a keyless sidecar with no transparency-log evidence (--allow-unlogged-signature)"
            );
        }
        None => return Err(VerifyErrorKind::SignatureInvalid),
    }

    Ok(VerifiedSigner {
        key_backend: KeyBackendKind::Keyless,
        certificate_identity: subject_identity(&cert),
        certificate_oidc_issuer: oidc_issuer(&cert),
        logged,
    })
}

/// cosign's `dev.sigstore.cosign/bundle` annotation: the offline Rekor bundle,
/// SET-verified, or `None` when the layer carries none.
///
/// The field names are Go struct tags on cosign's side and are wire, not style —
/// the same shape `oci::sign::simplesigning_write::offline_bundle` emits.
///
/// **Payload-agnostic, and shared.** Nothing here reads the logged *body*, so
/// [`super::attestation_sidecar`] calls this function rather than copying it:
/// the annotation, the base64, the log-key ladder and the SET are properties of
/// the annotation, not of what was logged. Which binder the body then faces is
/// the caller's — [`bind_logged_body`] for a `hashedrekord`,
/// [`super::dsse::verify_tlog_binding`] for a `dsse:0.0.1`.
///
/// # What is checked, and what a cosign v1 offline bundle cannot offer
///
/// 1. The **Signed Entry Timestamp** over the entry's canonical
///    `{body, integratedTime, logIndex, logID}`, against the log's own public
///    key ([`tlog::verify_set`] — the identical construction the bundle path
///    runs). The key comes from the same three-rung ladder the bundle path
///    uses ([`RekorKeyMemo::resolve`]): this run's already-resolved keys, then
///    pinned trust material, then an online fetch, and nothing at all offline.
/// 2. The **binding**: the logged `hashedrekord` body must name `sha256(payload)`
///    and carry this signature. Without it a real SET over a real entry for a
///    *different* artifact would pass step 1 and prove nothing about the bytes
///    in hand — an entry someone attached, not an entry about this signature.
///    That check is [`bind_logged_body`] and is run by [`verify_keyless`] after
///    the signature itself verified; see the call site for why the order
///    decides which refusal an operator is shown.
///
/// There is **no Merkle inclusion proof**, because cosign's v1 offline bundle
/// carries none: `SignedEntryTimestamp` plus the payload is the whole format.
/// That is the same evidence `cosign verify` checks against such a bundle, and
/// it is strictly more than this path had before, which credited the annotation
/// with nothing at all. The bundle path still demands both (a Sigstore bundle
/// v0.3 carries the proof, so an absent one there is a defect, not a format).
///
/// The logged `publicKey` is deliberately not compared against the annotation
/// certificate. It would mean re-deriving byte-for-byte the PEM cosign uploaded,
/// which no round trip guarantees, and it buys nothing: the certificate is
/// independently chained, SCT-checked and identity-matched, the signature
/// verifies under it, and the payload is bound to the subject by `check_claim`.
///
/// # Errors
///
/// [`VerifyErrorKind::BundleParseFailed`] when the annotation is not the
/// documented JSON; [`VerifyErrorKind::RekorSetInvalid`] when its base64 or its
/// SET does not hold; [`VerifyErrorKind::TransparencyBodyMismatch`] when the
/// logged body is about something other than this signature over this payload;
/// [`VerifyErrorKind::TransparencyLogUnavailable`] when the log's key can be
/// neither read from trust material nor fetched.
pub(super) async fn logged_entry(
    layer: &Descriptor,
    verify: &SidecarVerification<'_>,
) -> Result<Option<LoggedEntry>, VerifyErrorKind> {
    let Some(raw) = annotation(layer, ANNOTATION_COSIGN_BUNDLE) else {
        return Ok(None);
    };
    let offline: OfflineBundleAnnotation = serde_json::from_str(raw).map_err(|_| VerifyErrorKind::BundleParseFailed)?;
    let base64 = base64::engine::general_purpose::STANDARD;
    let signed_entry_timestamp = base64
        .decode(&offline.signed_entry_timestamp)
        .map_err(|_| VerifyErrorKind::RekorSetInvalid)?;
    let body = base64
        .decode(&offline.payload.body)
        .map_err(|_| VerifyErrorKind::RekorSetInvalid)?;
    let integrated_time = offline.payload.integrated_time;
    let log_index = u64::try_from(offline.payload.log_index).map_err(|_| VerifyErrorKind::RekorSetInvalid)?;

    let pem = verify
        .rekor_keys
        .resolve(
            verify.trust_root,
            verify.rekor_url,
            verify.offline,
            &offline.payload.log_id,
        )
        .await?;
    tlog::verify_set(
        &tlog::rekor_key(&pem)?,
        &tlog::TlogEntry {
            canonicalized_body: &body,
            integrated_time: u64::try_from(integrated_time).map_err(|_| VerifyErrorKind::RekorSetInvalid)?,
            log_index,
            log_id_hex: &offline.payload.log_id,
            signed_entry_timestamp: &signed_entry_timestamp,
        },
    )?;

    Ok(Some(LoggedEntry {
        integrated_time,
        log_index,
        body,
    }))
}

/// The logged `hashedrekord` body must be about **this** signature over **this**
/// payload. See [`logged_entry`] for why step 1 alone is not enough.
fn bind_logged_body(body: &[u8], payload: &[u8], signature: &[u8]) -> Result<(), VerifyErrorKind> {
    let logged: sigstore::rekor::models::Hashedrekord =
        serde_json::from_slice(body).map_err(|_| VerifyErrorKind::TransparencyBodyMismatch)?;
    if !matches!(logged.spec.data.hash.algorithm, hashedrekord::AlgorithmKind::sha256) {
        return Err(VerifyErrorKind::TransparencyBodyMismatch);
    }
    let payload_digest = crate::oci::Algorithm::Sha256.hash(payload);
    if logged.spec.data.hash.value != payload_digest.hex() {
        return Err(VerifyErrorKind::TransparencyBodyMismatch);
    }
    let encoded_signature = base64::engine::general_purpose::STANDARD.encode(signature);
    if logged.spec.signature.content != encoded_signature {
        return Err(VerifyErrorKind::TransparencyBodyMismatch);
    }
    Ok(())
}

/// cosign's offline Rekor bundle, as the annotation carries it.
///
/// The read twin of `oci::sign::simplesigning_write::offline_bundle`'s writer.
/// Field names and capitalisation are Go struct tags on cosign's side: wire, not
/// style. Signed integers because Rekor's own schema is `int64` and a negative
/// value must be *rejected* by a later conversion rather than wrap into a large
/// unsigned one here.
#[derive(serde::Deserialize)]
struct OfflineBundleAnnotation {
    #[serde(rename = "SignedEntryTimestamp")]
    signed_entry_timestamp: String,
    #[serde(rename = "Payload")]
    payload: OfflineBundleAnnotationPayload,
}

#[derive(serde::Deserialize)]
struct OfflineBundleAnnotationPayload {
    body: String,
    #[serde(rename = "integratedTime")]
    integrated_time: i64,
    #[serde(rename = "logIndex")]
    log_index: i64,
    #[serde(rename = "logID")]
    log_id: String,
}

/// The Sigstore bundle a sidecar layer's material describes.
///
/// Built so the keyless gate can be **the same code** the bundle path runs
/// rather than a second implementation of chain building and SCT verification —
/// `sigstore` 0.14 exposes its certificate pool and its CT keyring only through
/// [`Verifier`], and hand-rolling either on a trust path is the class of
/// mistake that fails silently past local fixtures.
///
/// # The transparency-log entry, and why it decides nothing
///
/// `sigstore`'s `CheckedBundle` requires exactly one entry, so one is supplied:
/// the `hashedrekord` body this signature *would* be logged under, derived —
/// with `sigstore`'s own types — from the certificate, the signature and
/// `sha256(payload)`. It is not evidence and is never treated as any: its
/// inclusion promise is empty and `sigstore` 0.14 verifies neither the SET nor
/// the Merkle proof (both are `TODO`s upstream). Its only consumers are
/// `sigstore`'s consistency comparison against a body it re-derives
/// identically, and its two time checks — the chain build and the
/// certificate-expiry comparison — which both anchor on `integrated_time`.
///
/// That is why `integrated_time` is a **caller** argument. It is the entry's
/// own `integratedTime`, checked by [`logged_entry`] before this is built, so
/// the library's expiry check runs against evidence rather than against the
/// certificate's own claim about itself. Under
/// `--allow-unlogged-signature` there is no entry and the caller passes the
/// leaf's `notBefore`, which makes that library check vacuous — the whole
/// content of the opt-out, and stated at the call site rather than hidden here.
fn sidecar_bundle(
    cert: &x509_cert::Certificate,
    leaf_der: &[u8],
    chain_ders: Vec<Vec<u8>>,
    payload: &[u8],
    signature: &[u8],
    integrated_time: i64,
) -> Result<Bundle, VerifyErrorKind> {
    let base64 = base64::engine::general_purpose::STANDARD;
    let leaf_pem = cert
        .to_pem(x509_cert::der::pem::LineEnding::LF)
        .map_err(|_| VerifyErrorKind::CertChainInvalid)?;
    let payload_digest = crate::oci::Algorithm::Sha256.hash(payload);

    let body = hashedrekord::Spec {
        signature: hashedrekord::Signature {
            content: base64.encode(signature),
            public_key: hashedrekord::PublicKey::new(base64.encode(leaf_pem)),
        },
        data: hashedrekord::Data {
            hash: hashedrekord::Hash {
                algorithm: hashedrekord::AlgorithmKind::sha256,
                value: payload_digest.hex().to_owned(),
            },
        },
    };
    let body = sigstore::rekor::models::Hashedrekord {
        kind: "hashedrekord".to_owned(),
        api_version: "0.0.1".to_owned(),
        spec: body,
    };
    let canonicalized_body =
        serde_json_canonicalizer::to_vec(&body).map_err(|e| VerifyErrorKind::Internal(Box::new(e)))?;

    let mut certificates = vec![X509Certificate {
        raw_bytes: leaf_der.to_vec(),
    }];
    certificates.extend(chain_ders.into_iter().map(|raw_bytes| X509Certificate { raw_bytes }));

    Ok(Bundle {
        // The 0.1 profile: it is the one whose structural check an entry with
        // no Merkle proof satisfies, which is the shape a sidecar has.
        media_type: SIGSTORE_BUNDLE_V01_MEDIA_TYPE.to_owned(),
        verification_material: Some(VerificationMaterial {
            timestamp_verification_data: None,
            tlog_entries: vec![TransparencyLogEntry {
                log_index: 0,
                log_id: None,
                kind_version: None,
                integrated_time,
                inclusion_promise: Some(InclusionPromise {
                    signed_entry_timestamp: Vec::new(),
                }),
                inclusion_proof: None,
                canonicalized_body,
            }],
            content: Some(verification_material::Content::X509CertificateChain(
                X509CertificateChain { certificates },
            )),
        }),
        content: Some(bundle::Content::MessageSignature(MessageSignature {
            // Left absent deliberately: `sigstore` verifies the signature
            // against the digest *it* computes from the input, and never reads
            // this field. Populating it would look like a second, unchecked
            // statement of what was signed.
            message_digest: None,
            signature: signature.to_vec(),
        })),
    })
}

/// The two `critical` fields a verifier is required to understand.
///
/// Parses the claim to *read* them and for nothing else: the signature is taken
/// over `payload` itself, so this parse can never stand in for the signed bytes.
fn check_claim(payload: &[u8], subject_digest: &Digest) -> Result<(), VerifyErrorKind> {
    let claim: SimpleSigningClaim = serde_json::from_slice(payload).map_err(|_| VerifyErrorKind::BundleParseFailed)?;

    if claim.critical.claim_type != SIMPLESIGNING_CLAIM_TYPE {
        return Err(VerifyErrorKind::SimpleSigningClaimUnsupported {
            claim_type: claim.critical.claim_type,
        });
    }
    // The cross-subject splice guard: a genuine signature over *another*
    // manifest, re-attached to this one, is valid in every other respect.
    if claim.critical.image.docker_manifest_digest != subject_digest.to_string() {
        return Err(VerifyErrorKind::SubjectDigestMismatch);
    }
    Ok(())
}

/// The base64 signature annotation, decoded.
fn layer_signature(layer: &Descriptor) -> Result<Vec<u8>, VerifyErrorKind> {
    let encoded = annotation(layer, ANNOTATION_COSIGN_SIGNATURE).ok_or(VerifyErrorKind::BundleParseFailed)?;
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| VerifyErrorKind::BundleParseFailed)
}

/// The leaf certificate annotation as DER, or `None` under a key.
pub(super) fn layer_certificate(layer: &Descriptor) -> Result<Option<Vec<u8>>, VerifyErrorKind> {
    let Some(pem) = annotation(layer, ANNOTATION_COSIGN_CERTIFICATE) else {
        // A chain with no leaf is a malformed shape, not the key-mode shape:
        // there is no certificate to verify the intermediates lead to.
        return match annotation(layer, ANNOTATION_COSIGN_CHAIN) {
            Some(_) => Err(VerifyErrorKind::CertChainInvalid),
            None => Ok(None),
        };
    };
    let block = pem::parse(pem).map_err(|_| VerifyErrorKind::CertChainInvalid)?;
    Ok(Some(block.contents().to_vec()))
}

/// The intermediate chain annotation as DER, empty when absent.
pub(super) fn layer_chain(layer: &Descriptor) -> Result<Vec<Vec<u8>>, VerifyErrorKind> {
    let Some(pem_text) = annotation(layer, ANNOTATION_COSIGN_CHAIN) else {
        return Ok(Vec::new());
    };
    let blocks = pem::parse_many(pem_text).map_err(|_| VerifyErrorKind::CertChainInvalid)?;
    if blocks.is_empty() {
        return Err(VerifyErrorKind::CertChainInvalid);
    }
    Ok(blocks.into_iter().map(|block| block.into_contents()).collect())
}

fn annotation<'a>(layer: &'a Descriptor, key: &str) -> Option<&'a str> {
    layer
        .annotations
        .as_ref()
        .and_then(|annotations| annotations.get(key))
        .map(String::as_str)
}

#[cfg(test)]
mod tests {
    //! Verified against **committed cosign v3.1.1 output**, loaded with
    //! `include_bytes!`/`include_str!` so a moved fixture is a compile error and
    //! no reader can normalise the bytes on the way in — the precedent
    //! `crate::oci::simplesigning`'s own tests set.
    //!
    //! The trust root is the committed local Fulcio/CT/Rekor material under
    //! `test/sigstore`, so every keyless assertion here runs fully offline with
    //! no container.
    use super::*;
    use crate::oci::client::test_transport::{StubTransport, StubTransportData};
    use crate::oci::verify::trust_root::TrustRoot;
    use crate::trust::{CompiledKeyless, IdentityRule, PolicyBackend};
    use sigstore::rekor::apis::configuration::Configuration as RekorConfiguration;

    const KEY_MANIFEST: &str =
        include_str!("../../../../../test/tests/fixtures/golden/simplesigning_key_manifest.json");
    const KEY_PAYLOAD: &[u8] =
        include_bytes!("../../../../../test/tests/fixtures/golden/simplesigning_key_payload.json");
    const KEYLESS_MANIFEST: &str =
        include_str!("../../../../../test/tests/fixtures/golden/simplesigning_keyless_manifest.json");
    const KEYLESS_PAYLOAD: &[u8] =
        include_bytes!("../../../../../test/tests/fixtures/golden/simplesigning_keyless_payload.json");
    const COSIGN_PUBLIC_KEY_PEM: &str = include_str!("../../../../../test/tests/fixtures/golden/keys/cosign.pub");
    const TRUSTED_ROOT_JSON: &[u8] = include_bytes!("../../../../../test/sigstore/trusted_root.json");
    /// G0's keyless golden bundle, borrowed for its **transparency-log entry**
    /// alone: a real `integratedTime`, `logIndex`, `logID` and
    /// `canonicalizedBody` under a Signed Entry Timestamp the committed trust
    /// root's Rekor key actually verifies. No sidecar fixture carries a
    /// `dev.sigstore.cosign/bundle` annotation (cosign v3.1.1 writes none), so
    /// this is the only committed material a positive SET assertion can be
    /// built from — and a positive one is what makes the tampered half mean
    /// anything.
    const GOLDEN_KEYLESS_BUNDLE: &str = include_str!("../../../../../test/tests/fixtures/golden/keyless_bundle.json");

    /// D-authored negative fixtures (spec D7 — simplesigning read fixtures are
    /// committed bytes).
    const FOREIGN_SUBJECT_PAYLOAD: &[u8] =
        include_bytes!("../../../../../test/tests/fixtures/simplesigning/foreign_subject_payload.json");
    const FOREIGN_CLAIM_TYPE_PAYLOAD: &[u8] =
        include_bytes!("../../../../../test/tests/fixtures/simplesigning/foreign_claim_type_payload.json");
    const UNTRUSTED_CA_MANIFEST: &str =
        include_str!("../../../../../test/tests/fixtures/simplesigning/untrusted_ca_manifest.json");
    const TAMPERED_SIGNATURE_MANIFEST: &str =
        include_str!("../../../../../test/tests/fixtures/simplesigning/tampered_signature_manifest.json");
    const PUBLISHER_FORMATTED_PAYLOAD: &[u8] =
        include_bytes!("../../../../../test/tests/fixtures/simplesigning/publisher_formatted_payload.json");
    const PUBLISHER_FORMATTED_MANIFEST: &str =
        include_str!("../../../../../test/tests/fixtures/simplesigning/publisher_formatted_manifest.json");

    /// The subject both golden fixtures name, read out of the payload rather
    /// than transcribed — a transcribed digest is a second source of truth.
    fn golden_subject(payload: &[u8]) -> Digest {
        let parsed: serde_json::Value = serde_json::from_slice(payload).expect("payload is JSON");
        Digest::try_from(
            parsed
                .pointer("/critical/image/docker-manifest-digest")
                .and_then(serde_json::Value::as_str)
                .expect("payload names a subject"),
        )
        .expect("the subject is an OCI digest")
    }

    fn layer_of(manifest: &str) -> Descriptor {
        let parsed: ImageManifest = serde_json::from_str(manifest).expect("manifest parses");
        parsed.layers.into_iter().next().expect("the sidecar carries one layer")
    }

    fn verifier() -> Verifier {
        Verifier::new(
            RekorConfiguration::default(),
            TrustRoot::load_trusted_root_json(TRUSTED_ROOT_JSON).expect("the committed trusted root loads"),
        )
        .expect("the trusted root builds a verifier")
    }

    /// The committed local trust root, which pins the local Rekor public key —
    /// so a `dev.sigstore.cosign/bundle` annotation's SET verifies here with no
    /// container and no network.
    fn trust_root() -> TrustRoot {
        TrustRoot::load_trusted_root_json(TRUSTED_ROOT_JSON).expect("the committed trusted root loads")
    }

    /// The default gate: evidence is required, and an unpinned Rekor key would
    /// have to be fetched (which `offline` forbids, so no test can silently
    /// reach the network). The committed root pins the local key, so the real
    /// path is exercised rather than skipped.
    fn gate<'a>(
        verifier: &'a Verifier,
        policies: &'a [CompiledPolicy],
        root: &'a TrustRoot,
        rekor_url: &'a Url,
    ) -> SidecarVerification<'a> {
        SidecarVerification {
            verifier,
            policies,
            trust_root: root,
            rekor_url,
            offline: true,
            allow_unlogged: false,
            rekor_keys: RekorKeyMemo::default(),
        }
    }

    fn rekor_url() -> Url {
        Url::parse("http://127.0.0.1:3000").expect("rekor url")
    }

    fn key_policy(pem: &str) -> CompiledPolicy {
        CompiledPolicy {
            builder: None,
            backends: vec![PolicyBackend::Key(
                sigstore::crypto::CosignVerificationKey::try_from_pem(pem.as_bytes()).expect("an SPKI PEM"),
            )],
        }
    }

    fn keyless_policy(identity: &str, issuer: &str) -> CompiledPolicy {
        CompiledPolicy {
            builder: None,
            backends: vec![PolicyBackend::Keyless(CompiledKeyless {
                identity: IdentityRule::Exact(identity.to_owned()),
                issuer: issuer.to_owned(),
            })],
        }
    }

    /// The identity the committed keyless fixture's certificate actually
    /// carries — the same pair `oci/verify/identity.rs` pins against the G0
    /// keyless bundle.
    const GOLDEN_IDENTITY: &str = "ocx-test@example.com";
    const GOLDEN_ISSUER: &str = "http://dex:5556/dex";

    // ── S-004: the key-mode shape ────────────────────────────────────────────

    /// **S-004.** cosign's key-mode `.sig` — signature annotation alone, no
    /// certificate, no chain, no bundle — verifies against a
    /// `PolicyBackend::Key`, and reports the key backend with no certificate
    /// identity.
    ///
    /// The assertion is on the returned [`VerifyResult`], not on "no error": it
    /// pins the subject the signature bound, the layer that carried it, and
    /// that both identity fields are absent because there is no certificate to
    /// read them from.
    #[tokio::test]
    async fn a_cosign_key_mode_sidecar_layer_verifies_against_a_pinned_key() {
        let layer = layer_of(KEY_MANIFEST);
        let subject = golden_subject(KEY_PAYLOAD);
        let policies = [key_policy(COSIGN_PUBLIC_KEY_PEM)];

        let result = verify_layer(
            &layer,
            KEY_PAYLOAD,
            &subject,
            &gate(&verifier(), &policies, &trust_root(), &rekor_url()),
            DiscoveryMethod::SidecarTag,
        )
        .await
        .expect("cosign's key-mode simplesigning layer verifies")
        .result;

        assert_eq!(result.subject_digest, subject);
        assert_eq!(result.referrer_digest.to_string(), layer.digest);
        assert_eq!(result.key_backend, KeyBackendKind::File);
        assert_eq!(result.certificate_identity, None);
        assert_eq!(result.certificate_oidc_issuer, None);
        assert_eq!(result.signed_at, None);
    }

    /// The key-mode signature is genuinely checked: the same layer under a
    /// policy naming a *different* key is refused.
    ///
    /// Paired with the test above on purpose — an always-Ok signature check
    /// passes the first, an always-Err one passes this, and only the pair shows
    /// the verdict tracks the key.
    #[tokio::test]
    async fn a_key_mode_layer_is_refused_by_a_policy_naming_another_key() {
        use p256::ecdsa::SigningKey;
        use p256::elliptic_curve::rand_core::OsRng;
        use p256::pkcs8::EncodePublicKey as _;

        let other = SigningKey::random(&mut OsRng);
        let other_pem = other
            .verifying_key()
            .to_public_key_pem(p256::pkcs8::LineEnding::LF)
            .expect("a P-256 public key encodes as SPKI PEM");

        let layer = layer_of(KEY_MANIFEST);
        let subject = golden_subject(KEY_PAYLOAD);
        let verdict = verify_layer(
            &layer,
            KEY_PAYLOAD,
            &subject,
            &gate(&verifier(), &[key_policy(&other_pem)], &trust_root(), &rekor_url()),
            DiscoveryMethod::SidecarTag,
        )
        .await;

        assert!(
            matches!(verdict, Err(VerifyErrorKind::SignatureInvalid)),
            "another key must not verify cosign's signature: {verdict:?}"
        );
    }

    // ── S-005: the keyless, no-transparency-log shape ────────────────────────

    /// **S-005, reversed.** cosign's keyless `.sig` — a certificate annotation
    /// and **no** `dev.sigstore.cosign/bundle` — is **refused**.
    ///
    /// G1 froze the opposite: the shape was declared legal and its
    /// certificate-validity window anchored on the certificate's own
    /// `notBefore`, which asks the certificate when it was valid and then
    /// judges it against its own answer. This fixture is why that mattered —
    /// its leaf is valid `2026-08-29T02:07:58Z .. 02:17:58Z`, ten minutes, and
    /// under the old contract it verified for ever. cosign refuses the same
    /// artifact by default (rc 12, "signature not found in transparency log")
    /// and needs `--insecure-ignore-tlog` to accept it.
    ///
    /// Asserted as `SignatureInvalid` and not merely "an error": a refusal for
    /// a parse or chain reason would pass a bare `is_err()` while proving the
    /// gate never ran. Its sibling below is the other half — the opt-out has to
    /// bring this exact layer back, or the flag is one nobody can use.
    #[tokio::test]
    async fn a_cosign_keyless_sidecar_layer_with_no_tlog_material_is_refused() {
        let layer = layer_of(KEYLESS_MANIFEST);
        let subject = golden_subject(KEYLESS_PAYLOAD);
        let policies = [keyless_policy(GOLDEN_IDENTITY, GOLDEN_ISSUER)];

        // The fixture really is the no-transparency-log shape: assert it before
        // asserting anything about how it is judged.
        assert!(
            annotation(&layer, ANNOTATION_COSIGN_BUNDLE).is_none(),
            "the committed keyless fixture must carry no offline Rekor bundle"
        );

        let verdict = verify_layer(
            &layer,
            KEYLESS_PAYLOAD,
            &subject,
            &gate(&verifier(), &policies, &trust_root(), &rekor_url()),
            DiscoveryMethod::SidecarTag,
        )
        .await;

        assert!(
            matches!(verdict, Err(VerifyErrorKind::SignatureInvalid)),
            "a keyless sidecar with no transparency-log evidence must be refused: {verdict:?}"
        );
    }

    /// The opt-out, and the proof it is reachable: the **same layer** the test
    /// above refuses verifies under `allow_unlogged`, through the full keyless
    /// gate — chain, SCT, signature, identity — with only the evidence
    /// requirement lifted.
    ///
    /// The pair is what makes either half mean anything. Alone, the refusal
    /// above is satisfied by a gate that refuses every keyless sidecar; alone,
    /// this one is satisfied by a gate that refuses none.
    ///
    /// Both absences are asserted too. Nothing timestamped this signature, so
    /// `signed_at` and `rekor_log_index` must stay empty — a flag that bought
    /// acceptance *and* invented a signing instant would be worse than the
    /// contract it replaced.
    #[tokio::test]
    async fn the_opt_out_brings_back_the_sidecar_the_evidence_gate_refuses() {
        let layer = layer_of(KEYLESS_MANIFEST);
        let subject = golden_subject(KEYLESS_PAYLOAD);
        let policies = [keyless_policy(GOLDEN_IDENTITY, GOLDEN_ISSUER)];
        let root = trust_root();
        let url = rekor_url();

        let result = verify_layer(
            &layer,
            KEYLESS_PAYLOAD,
            &subject,
            &SidecarVerification {
                verifier: &verifier(),
                policies: &policies,
                trust_root: &root,
                rekor_url: &url,
                offline: true,
                allow_unlogged: true,
                rekor_keys: RekorKeyMemo::default(),
            },
            DiscoveryMethod::SidecarTag,
        )
        .await
        .expect("--allow-unlogged-signature accepts a keyless sidecar with no log entry")
        .result;

        // Read back off the certificate that just passed the chain, the SCT and
        // the signature — so this proves the gate ran on real material rather
        // than that a call returned `Ok`.
        assert_eq!(result.key_backend, KeyBackendKind::Keyless);
        assert_eq!(result.certificate_identity.as_deref(), Some(GOLDEN_IDENTITY));
        assert_eq!(result.certificate_oidc_issuer.as_deref(), Some(GOLDEN_ISSUER));
        assert_eq!(result.subject_digest, subject);
        assert_eq!(
            result.signed_at, None,
            "the opt-out accepts a signature nothing timestamps; it must not report an instant"
        );
        assert_eq!(
            result.rekor_log_index, None,
            "the opt-out accepts a signature no log holds; it must not report a log position"
        );
    }

    /// The keyless identity gate is not weakened by the material arriving in
    /// annotations: the same layer under a policy naming another identity is
    /// refused with 77, and under the right identity but the wrong issuer with
    /// the issuer's own kind.
    #[tokio::test]
    async fn the_keyless_identity_gate_runs_on_the_annotation_certificate() {
        let layer = layer_of(KEYLESS_MANIFEST);
        let subject = golden_subject(KEYLESS_PAYLOAD);

        let wrong_identity = verify_layer(
            &layer,
            KEYLESS_PAYLOAD,
            &subject,
            &gate(
                &verifier(),
                &[keyless_policy("nobody@example.com", GOLDEN_ISSUER)],
                &trust_root(),
                &rekor_url(),
            ),
            DiscoveryMethod::SidecarTag,
        )
        .await;
        assert!(
            matches!(wrong_identity, Err(VerifyErrorKind::IdentityMismatch)),
            "{wrong_identity:?}"
        );

        let wrong_issuer = verify_layer(
            &layer,
            KEYLESS_PAYLOAD,
            &subject,
            &gate(
                &verifier(),
                &[keyless_policy(GOLDEN_IDENTITY, "https://elsewhere.example")],
                &trust_root(),
                &rekor_url(),
            ),
            DiscoveryMethod::SidecarTag,
        )
        .await;
        assert!(
            matches!(wrong_issuer, Err(VerifyErrorKind::IssuerMismatch)),
            "{wrong_issuer:?}"
        );
    }

    // ── S-014: the chain gate ────────────────────────────────────────────────

    /// **S-014.** A `.sig` whose annotation certificate does not chain to the
    /// Fulcio root is refused with `cert_chain_invalid` (65).
    ///
    /// The fixture's certificate carries the *same* SAN and OIDC issuer as the
    /// genuine one, so nothing downstream of the chain check can be what
    /// refuses it — the identity gate would accept this certificate.
    #[tokio::test]
    async fn a_sidecar_certificate_outside_the_fulcio_root_is_refused() {
        let layer = layer_of(UNTRUSTED_CA_MANIFEST);
        let subject = golden_subject(KEYLESS_PAYLOAD);
        let policies = [keyless_policy(GOLDEN_IDENTITY, GOLDEN_ISSUER)];

        let leaf = layer_certificate(&layer)
            .expect("the fixture carries a certificate")
            .expect("the fixture carries a certificate");
        let cert = parse_certificate(&leaf).expect("the rogue certificate parses");
        assert_eq!(
            subject_identity(&cert).as_deref(),
            Some(GOLDEN_IDENTITY),
            "the rogue certificate must be identity-acceptable, or the chain is not what refuses it"
        );
        assert_eq!(oidc_issuer(&cert).as_deref(), Some(GOLDEN_ISSUER));

        let verdict = verify_layer(
            &layer,
            KEYLESS_PAYLOAD,
            &subject,
            &gate(&verifier(), &policies, &trust_root(), &rekor_url()),
            DiscoveryMethod::SidecarTag,
        )
        .await;
        assert!(
            matches!(verdict, Err(VerifyErrorKind::CertChainInvalid)),
            "a certificate outside the trust root must be refused: {verdict:?}"
        );

        // The opt-out's blast radius, in the one direction the run above cannot
        // show. Under `allow_unlogged: false` this layer reds at the chain check
        // *before* the missing-entry arm is reached, so that verdict cannot tell
        // "the chain is enforced" apart from "refused for the absent entry".
        // Re-run with the flag on: the only thing it may buy is the evidence
        // requirement, so a rogue CA must still be refused, and with the same
        // kind.
        let root = trust_root();
        let url = rekor_url();
        let under_opt_out = verify_layer(
            &layer,
            KEYLESS_PAYLOAD,
            &subject,
            &SidecarVerification {
                verifier: &verifier(),
                policies: &policies,
                trust_root: &root,
                rekor_url: &url,
                offline: true,
                allow_unlogged: true,
                rekor_keys: RekorKeyMemo::default(),
            },
            DiscoveryMethod::SidecarTag,
        )
        .await;
        assert!(
            matches!(under_opt_out, Err(VerifyErrorKind::CertChainInvalid)),
            "--allow-unlogged-signature lifts the evidence requirement and nothing else: {under_opt_out:?}"
        );
    }

    /// The payload signature is genuinely checked on the keyless path too: the
    /// genuine certificate with one flipped signature byte is refused.
    ///
    /// Separate from the chain fixture on purpose — chain, SCT and signature are
    /// one delegated `sigstore` call, so only two fixtures that break different
    /// halves of it can tell the halves apart.
    #[tokio::test]
    async fn a_tampered_sidecar_signature_is_refused() {
        let layer = layer_of(TAMPERED_SIGNATURE_MANIFEST);
        let genuine = layer_of(KEYLESS_MANIFEST);
        assert_eq!(
            annotation(&layer, ANNOTATION_COSIGN_CERTIFICATE),
            annotation(&genuine, ANNOTATION_COSIGN_CERTIFICATE),
            "the tampered fixture must differ from the genuine one in the signature alone"
        );
        assert_ne!(
            annotation(&layer, ANNOTATION_COSIGN_SIGNATURE),
            annotation(&genuine, ANNOTATION_COSIGN_SIGNATURE)
        );

        let verdict = verify_layer(
            &layer,
            KEYLESS_PAYLOAD,
            &golden_subject(KEYLESS_PAYLOAD),
            &gate(
                &verifier(),
                &[keyless_policy(GOLDEN_IDENTITY, GOLDEN_ISSUER)],
                &trust_root(),
                &rekor_url(),
            ),
            DiscoveryMethod::SidecarTag,
        )
        .await;
        assert!(
            matches!(verdict, Err(VerifyErrorKind::SignatureInvalid)),
            "a tampered signature must be refused: {verdict:?}"
        );
    }

    // ── S-006: the cross-subject splice ──────────────────────────────────────

    /// **S-006.** A genuine, fully valid signature over *another* manifest,
    /// re-attached to this one, is refused with `subject_digest_mismatch` (65).
    ///
    /// The layer is the untouched golden keyless one — its certificate chains,
    /// its SCT verifies, its signature is correct and its identity matches — so
    /// the subject binding is the only thing that can refuse it. That is what
    /// makes inverting the comparison a red rather than a no-op.
    #[tokio::test]
    async fn a_genuine_signature_for_another_subject_is_refused() {
        let layer = layer_of(KEYLESS_MANIFEST);
        let genuine_subject = golden_subject(KEYLESS_PAYLOAD);
        let other_subject = Digest::try_from("sha256:0000000000000000000000000000000000000000000000000000000000000001")
            .expect("a well-formed digest");
        assert_ne!(genuine_subject, other_subject);

        let verdict = verify_layer(
            &layer,
            KEYLESS_PAYLOAD,
            &other_subject,
            &gate(
                &verifier(),
                &[keyless_policy(GOLDEN_IDENTITY, GOLDEN_ISSUER)],
                &trust_root(),
                &rekor_url(),
            ),
            DiscoveryMethod::SidecarTag,
        )
        .await;
        assert!(
            matches!(verdict, Err(VerifyErrorKind::SubjectDigestMismatch)),
            "a signature bound to another subject must be refused: {verdict:?}"
        );
    }

    /// The claim reader reads the binding out of committed bytes, not out of a
    /// value a test constructed: a payload naming another manifest is refused,
    /// and the same reader accepts the golden payload for its own subject.
    #[test]
    fn the_claim_reader_refuses_a_payload_naming_another_manifest() {
        let subject = golden_subject(KEYLESS_PAYLOAD);
        assert!(check_claim(KEYLESS_PAYLOAD, &subject).is_ok());

        let foreign = check_claim(FOREIGN_SUBJECT_PAYLOAD, &subject);
        assert!(
            matches!(foreign, Err(VerifyErrorKind::SubjectDigestMismatch)),
            "{foreign:?}"
        );
        // …and the fixture really does name a different manifest.
        assert_ne!(golden_subject(FOREIGN_SUBJECT_PAYLOAD), subject);
    }

    /// `critical.type` is a gate, not decoration: a payload of another claim
    /// type must not be read as an image signature.
    #[test]
    fn the_claim_reader_refuses_another_claim_type() {
        let verdict = check_claim(FOREIGN_CLAIM_TYPE_PAYLOAD, &golden_subject(FOREIGN_CLAIM_TYPE_PAYLOAD));
        let Err(VerifyErrorKind::SimpleSigningClaimUnsupported { claim_type }) = verdict else {
            panic!("expected a claim-type refusal, got {verdict:?}");
        };
        assert_ne!(claim_type, SIMPLESIGNING_CLAIM_TYPE);
    }

    // ── The raw-bytes rule ───────────────────────────────────────────────────

    /// The signature is checked over the bytes **as served**, never over a
    /// re-serialization of the parsed claim.
    ///
    /// Neither committed cosign payload can prove this: both were emitted by
    /// `serde_json` in the first place, so they round-trip byte-identically and
    /// a reader that re-serialized would pass against them unnoticed — exactly
    /// the silent bypass [`SimpleSigningClaim`]'s trust note warns about. This
    /// fixture is the same claim in a publisher's own formatting (indented, with
    /// a trailing newline), signed with the committed golden key over those
    /// bytes. A re-serialization produces the compact form instead, whose
    /// signature is not this one, so only reading the served bytes verifies.
    #[tokio::test]
    async fn the_signature_covers_the_served_bytes_not_a_re_serialized_claim() {
        // Assert the premise first: this payload genuinely does not round-trip,
        // or the test proves nothing.
        let parsed: SimpleSigningClaim =
            serde_json::from_slice(PUBLISHER_FORMATTED_PAYLOAD).expect("the fixture is a claim");
        let round_tripped = parsed.to_signing_bytes().expect("the claim re-serializes");
        assert_ne!(
            round_tripped.as_slice(),
            PUBLISHER_FORMATTED_PAYLOAD,
            "the fixture must NOT round-trip, or a re-serializing reader would pass here"
        );

        let layer = layer_of(PUBLISHER_FORMATTED_MANIFEST);
        // The descriptor addresses the served bytes, so the digest is a second,
        // independent statement that those are the bytes under test.
        assert_eq!(
            crate::oci::Algorithm::Sha256
                .hash(PUBLISHER_FORMATTED_PAYLOAD)
                .to_string(),
            layer.digest
        );

        let result = verify_layer(
            &layer,
            PUBLISHER_FORMATTED_PAYLOAD,
            &golden_subject(PUBLISHER_FORMATTED_PAYLOAD),
            &gate(
                &verifier(),
                &[key_policy(COSIGN_PUBLIC_KEY_PEM)],
                &trust_root(),
                &rekor_url(),
            ),
            DiscoveryMethod::SidecarTag,
        )
        .await
        .expect("a signature over the served bytes verifies")
        .result;
        assert_eq!(result.key_backend, KeyBackendKind::File);

        // The other direction: handing the reader the re-serialized form —
        // which is what a claim-reconstructing implementation would check —
        // is refused. Both halves together are what show the verdict tracks the
        // bytes rather than the claim.
        let reconstructed = verify_layer(
            &layer,
            &round_tripped,
            &golden_subject(PUBLISHER_FORMATTED_PAYLOAD),
            &gate(
                &verifier(),
                &[key_policy(COSIGN_PUBLIC_KEY_PEM)],
                &trust_root(),
                &rekor_url(),
            ),
            DiscoveryMethod::SidecarTag,
        )
        .await;
        assert!(
            matches!(reconstructed, Err(VerifyErrorKind::SignatureInvalid)),
            "a reconstructed payload must not verify: {reconstructed:?}"
        );
    }

    // ── S-013: the sidecar tag ──────────────────────────────────────────────

    /// **S-013.** A subject with no sidecar tag reads as `Ok(None)` — "no
    /// sidecar", never an error, because a subject cosign never touched is the
    /// overwhelmingly common case and a 404 says exactly that.
    ///
    /// The two sweeps that used to live here — one over every `SidecarKind`
    /// asserting the suffix and its tag reservation, one seeding the same
    /// manifest under each suffix — were deleted with `SidecarKind::Sbom`: both
    /// iterated a `.sbom` variant nothing reads, which is how a documented gap
    /// came to look covered. Tag reservation for all three suffixes is pinned
    /// where it belongs, in `package::tag`'s own tests and `tests/tag_verdicts.rs`.
    #[tokio::test]
    async fn a_missing_sidecar_tag_reads_as_no_sidecar() {
        let image: native::Reference = "localhost:5000/golden/simplesigning-key:1.0"
            .parse()
            .expect("test reference");
        let unsigned = Digest::try_from("sha256:0000000000000000000000000000000000000000000000000000000000000002")
            .expect("a well-formed digest");
        let policies = [key_policy(COSIGN_PUBLIC_KEY_PEM)];

        let absent = read_sidecar_tag(
            &StubTransport::new(StubTransportData::new()),
            &image,
            &unsigned,
            SidecarKind::Signature,
            &gate(&verifier(), &policies, &trust_root(), &rekor_url()),
            DiscoveryMethod::SidecarTag,
        )
        .await
        .expect("a missing sidecar tag is not an error");
        assert!(absent.is_none());
    }

    /// **S-013, the positive half.** A `.sig` sidecar seeded at exactly the tag
    /// [`sidecar_tag`] derives is found, and its layer verifies through the same
    /// claim logic the referrer door uses.
    ///
    /// Restored from the `SidecarKind::ALL` sweep deleted with the `.sbom`
    /// variant: that sweep's one irreplaceable assertion was that
    /// [`read_sidecar_tag`] reaches a sidecar which *exists*. Its sibling above
    /// asserts only `Ok(None)`, which a reader that addressed the wrong tag —
    /// or the wrong suffix — would satisfy just as well.
    #[tokio::test]
    async fn a_seeded_signature_sidecar_tag_is_found_and_read() {
        let subject = golden_subject(KEY_PAYLOAD);
        let layer = layer_of(KEY_MANIFEST);
        let image: native::Reference = "localhost:5000/golden/simplesigning-key:1.0"
            .parse()
            .expect("test reference");

        let data = StubTransportData::new();
        {
            let mut inner = data.write();
            let target = sibling_tag_reference(&image, sidecar_tag(&subject, SidecarKind::Signature));
            inner.manifests.insert(
                target.to_string(),
                (
                    KEY_MANIFEST.as_bytes().to_vec(),
                    crate::oci::OCI_IMAGE_MEDIA_TYPE.to_owned(),
                ),
            );
            inner.blobs.insert(layer.digest.clone(), KEY_PAYLOAD.to_vec());
        }
        let policies = [key_policy(COSIGN_PUBLIC_KEY_PEM)];

        let scan = read_sidecar_tag(
            &StubTransport::new(data),
            &image,
            &subject,
            SidecarKind::Signature,
            &gate(&verifier(), &policies, &trust_root(), &rekor_url()),
            DiscoveryMethod::SidecarTag,
        )
        .await
        .expect("the seeded sidecar reads")
        .expect("a seeded sidecar tag is found, not reported absent");

        assert!(scan.refused.is_empty(), "{:?}", scan.refused);
        assert_eq!(scan.verified.len(), 1);
        assert_eq!(scan.verified[0].result.referrer_digest.to_string(), layer.digest);
        assert_eq!(scan.verified[0].result.key_backend, KeyBackendKind::File);
        assert_eq!(scan.verified[0].result.discovery_method, DiscoveryMethod::SidecarTag);
        assert_eq!(scan.verified[0].result.signature_format, SignatureFormat::Simplesigning);
    }

    /// A layer of another media type is skipped, and a sidecar with no
    /// simplesigning layer contributes no candidates without failing.
    #[tokio::test]
    async fn non_simplesigning_layers_are_skipped_rather_than_refused() {
        let subject = golden_subject(KEY_PAYLOAD);
        let image: native::Reference = "localhost:5000/golden/simplesigning-key:1.0"
            .parse()
            .expect("test reference");
        let mut manifest: ImageManifest = serde_json::from_str(KEY_MANIFEST).expect("manifest parses");
        for layer in &mut manifest.layers {
            layer.media_type = "application/vnd.oci.image.layer.v1.tar+gzip".to_owned();
        }
        let bytes = serde_json::to_vec(&manifest).expect("manifest re-serializes");

        let scan = read_sidecar_manifest(
            &StubTransport::new(StubTransportData::new()),
            &image,
            &bytes,
            &subject,
            &gate(
                &verifier(),
                &[key_policy(COSIGN_PUBLIC_KEY_PEM)],
                &trust_root(),
                &rekor_url(),
            ),
            DiscoveryMethod::ReferrersApi,
        )
        .await
        .expect("a sidecar carrying no simplesigning layer is not an error");
        assert!(scan.verified.is_empty());
        assert!(scan.refused.is_empty());
    }

    // ── The transparency-log evidence, checked directly ──────────────────────
    //
    // Neither `bind_logged_body` nor `logged_entry`'s SET branch is reachable
    // from a sidecar fixture: no committed sidecar carries a
    // `dev.sigstore.cosign/bundle` annotation, and the tests that mutate a
    // keyless sidecar flip the *signature* annotation, which `verifier.verify`
    // refuses first. Driven through `verify_layer` both functions would be
    // green in every state, so they are driven directly here instead.

    /// A `hashedrekord` body in Rekor's own wire spelling.
    ///
    /// A JSON literal rather than the `hashedrekord::Spec` value `sidecar_bundle`
    /// builds: a body constructed from the producer's types agrees with the
    /// reader by construction, and would keep agreeing if both sides drifted off
    /// the schema together.
    fn logged_body(payload_hex: &str, signature_base64: &str, algorithm: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "kind": "hashedrekord",
            "apiVersion": "0.0.1",
            "spec": {
                "signature": {
                    "content": signature_base64,
                    // Deliberately not the annotation certificate: the binding
                    // does not compare it, and `logged_entry`'s doc says why.
                    "publicKey": { "content": "" }
                },
                "data": { "hash": { "algorithm": algorithm, "value": payload_hex } }
            }
        }))
        .expect("the body serializes")
    }

    /// The splice guard. A **real** Rekor entry — valid SET, `integratedTime`
    /// inside the leaf's window — for artifact *A*, spliced onto artifact *B*'s
    /// otherwise-valid keyless sidecar, passes every check the SET can make:
    /// membership proves nothing about *B* without this binding.
    ///
    /// Four bodies over the same committed material, differing in one field
    /// each. The accepting one is what stops the three refusals being satisfied
    /// by a function that refuses everything; the three refusals are what stop
    /// the acceptance being satisfied by `Ok(())`.
    #[test]
    fn the_logged_body_must_be_about_this_signature_over_this_payload() {
        let base64 = base64::engine::general_purpose::STANDARD;
        let signature = layer_signature(&layer_of(KEYLESS_MANIFEST)).expect("the golden layer carries a signature");
        let signature_base64 = base64.encode(&signature);
        let payload_hex = crate::oci::Algorithm::Sha256.hash(KEYLESS_PAYLOAD).hex().to_owned();

        let bound = logged_body(&payload_hex, &signature_base64, "sha256");
        assert!(
            bind_logged_body(&bound, KEYLESS_PAYLOAD, &signature).is_ok(),
            "an entry naming this payload's digest and this signature must bind"
        );

        // The splice: a genuine entry about another artifact. Its digest comes
        // from a committed payload rather than a literal, so it is a real
        // artifact's hash and not a value chosen to differ.
        let foreign_hex = crate::oci::Algorithm::Sha256
            .hash(FOREIGN_SUBJECT_PAYLOAD)
            .hex()
            .to_owned();
        assert_ne!(foreign_hex, payload_hex);
        let other_artifact = logged_body(&foreign_hex, &signature_base64, "sha256");
        assert!(
            matches!(
                bind_logged_body(&other_artifact, KEYLESS_PAYLOAD, &signature),
                Err(VerifyErrorKind::TransparencyBodyMismatch)
            ),
            "an entry about another artifact must not bind to this payload"
        );

        // The same entry re-pointed at another signature over the same bytes —
        // the committed one-byte-flipped fixture, so this is real cosign
        // material too.
        let other_signature =
            layer_signature(&layer_of(TAMPERED_SIGNATURE_MANIFEST)).expect("the tampered layer carries a signature");
        assert_ne!(other_signature, signature);
        let other_signature_body = logged_body(&payload_hex, &base64.encode(&other_signature), "sha256");
        assert!(
            matches!(
                bind_logged_body(&other_signature_body, KEYLESS_PAYLOAD, &signature),
                Err(VerifyErrorKind::TransparencyBodyMismatch)
            ),
            "an entry logging another signature must not bind to this one"
        );

        // A non-SHA-256 algorithm, with the SHA-256 value left in place: the
        // hash comparison still matches, so only the algorithm branch can
        // refuse this one.
        let wrong_algorithm = logged_body(&payload_hex, &signature_base64, "sha1");
        assert!(
            matches!(
                bind_logged_body(&wrong_algorithm, KEYLESS_PAYLOAD, &signature),
                Err(VerifyErrorKind::TransparencyBodyMismatch)
            ),
            "a body whose hash is not SHA-256 states nothing about `sha256(payload)`"
        );
    }

    /// The `dev.sigstore.cosign/bundle` annotation cosign *would* write, built
    /// from a committed Rekor entry: the golden keyless bundle's own
    /// `tlogEntries[0]`, re-spelled in the Go struct tags the annotation uses.
    ///
    /// Every field is read out of the fixture — a transcribed `logID` or
    /// `integratedTime` is a second source of truth, and the SET is a signature
    /// over all four.
    fn offline_bundle_annotation(signed_entry_timestamp: &[u8]) -> String {
        let base64 = base64::engine::general_purpose::STANDARD;
        let bundle: serde_json::Value =
            serde_json::from_str(GOLDEN_KEYLESS_BUNDLE).expect("the golden keyless bundle is JSON");
        let entry = &bundle["verificationMaterial"]["tlogEntries"][0];
        let number = |pointer: &str| -> i64 {
            entry[pointer]
                .as_str()
                .expect("the entry field is a JSON string")
                .parse()
                .expect("the entry field is an integer")
        };
        let log_id = base64
            .decode(entry["logId"]["keyId"].as_str().expect("the entry names a log"))
            .expect("the log id is base64");
        serde_json::json!({
            "SignedEntryTimestamp": base64.encode(signed_entry_timestamp),
            "Payload": {
                "body": entry["canonicalizedBody"],
                "integratedTime": number("integratedTime"),
                "logIndex": number("logIndex"),
                "logID": hex::encode(log_id),
            }
        })
        .to_string()
    }

    /// The golden entry's own SET, as the annotation carries it.
    fn golden_signed_entry_timestamp() -> Vec<u8> {
        let bundle: serde_json::Value =
            serde_json::from_str(GOLDEN_KEYLESS_BUNDLE).expect("the golden keyless bundle is JSON");
        base64::engine::general_purpose::STANDARD
            .decode(
                bundle["verificationMaterial"]["tlogEntries"][0]["inclusionPromise"]["signedEntryTimestamp"]
                    .as_str()
                    .expect("the golden entry carries a SET"),
            )
            .expect("the SET is base64")
    }

    /// The annotation's Signed Entry Timestamp is verified against the log's own
    /// key, not merely parsed: the golden entry's real SET yields the entry, and
    /// the same entry with one flipped signature byte is refused.
    ///
    /// The pair is the whole point. The accepting half alone is satisfied by a
    /// reader that never calls `verify_set`; the refusing half alone is
    /// satisfied by one that refuses every annotation.
    #[tokio::test]
    async fn the_annotation_set_is_verified_against_the_logs_own_key() {
        let mut layer = layer_of(KEYLESS_MANIFEST);
        let genuine = golden_signed_entry_timestamp();
        let annotations = layer.annotations.get_or_insert_default();
        annotations.insert(ANNOTATION_COSIGN_BUNDLE.to_owned(), offline_bundle_annotation(&genuine));

        let root = trust_root();
        let url = rekor_url();
        let policies: [CompiledPolicy; 0] = [];
        let entry = logged_entry(&layer, &gate(&verifier(), &policies, &root, &url))
            .await
            .expect("the golden entry's SET verifies against the pinned Rekor key")
            .expect("an annotation is present, so an entry comes back");
        // Read back off the fixture so a regenerated capture moves both sides.
        let bundle: serde_json::Value =
            serde_json::from_str(GOLDEN_KEYLESS_BUNDLE).expect("the golden keyless bundle is JSON");
        let golden = &bundle["verificationMaterial"]["tlogEntries"][0];
        assert_eq!(entry.integrated_time.to_string(), golden["integratedTime"]);
        assert_eq!(entry.log_index.to_string(), golden["logIndex"]);
        assert_eq!(
            base64::engine::general_purpose::STANDARD.encode(&entry.body),
            golden["canonicalizedBody"].as_str().expect("a canonical body"),
        );

        // One flipped byte in the SET's `s` value — still DER-shaped, no longer
        // the log's signature over these four fields.
        let mut tampered = genuine.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 0x01;
        layer.annotations.as_mut().expect("annotations were inserted").insert(
            ANNOTATION_COSIGN_BUNDLE.to_owned(),
            offline_bundle_annotation(&tampered),
        );
        let verdict = logged_entry(&layer, &gate(&verifier(), &policies, &root, &url)).await;
        assert!(
            matches!(verdict, Err(VerifyErrorKind::RekorSetInvalid)),
            "a SET that is not the log's signature over this entry must be refused: {verdict:?}"
        );
    }
}
