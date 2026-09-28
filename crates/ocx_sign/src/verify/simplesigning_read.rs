// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Reading cosign's *simplesigning* sidecar (`sha256-<hex>.sig` / `.att`): an image manifest whose layers are
//! [`crate::simplesigning`] payloads, one per signature, with their material in annotations.
//!
//! Owns no cryptography: layers run the bundle path's gates ([`super::identity::matching_key_policies`],
//! [`sigstore::bundle::verify::Verifier`]).

use base64::Engine as _;
use sigstore::bundle::verify::Verifier;
use sigstore::rekor::models::hashedrekord;
use sigstore_protobuf_specs::dev::sigstore::bundle::v1::{Bundle, VerificationMaterial, bundle, verification_material};
use sigstore_protobuf_specs::dev::sigstore::common::v1::{MessageSignature, X509Certificate, X509CertificateChain};
use sigstore_protobuf_specs::dev::sigstore::rekor::v1::{InclusionPromise, TransparencyLogEntry};
use url::Url;
use x509_cert::der::EncodePem as _;

use super::DiscoveryMethod;
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
use crate::sign::SignatureFormat;
use crate::simplesigning::{SIMPLESIGNING_CLAIM_TYPE, SimpleSigningClaim};
use ocx_oci::client::error::ClientError;
use ocx_oci::client::{OciTransport, sibling_tag_reference};
use ocx_oci::referrer::media_types::{
    ANNOTATION_COSIGN_BUNDLE, ANNOTATION_COSIGN_CERTIFICATE, ANNOTATION_COSIGN_CHAIN, ANNOTATION_COSIGN_SIGNATURE,
    SIMPLESIGNING_MEDIA_TYPE,
};
use ocx_oci::{Descriptor, Digest, ImageManifest, native};
use ocx_trust::CompiledPolicy;
use ocx_trust::key_ref::KeyBackendKind;

/// The Sigstore bundle 0.1 profile media type (`sigstore` keeps its enum private).
// The only profile a log entry without a Merkle proof satisfies; a misspelling fails every keyless sidecar.
pub(super) const SIGSTORE_BUNDLE_V01_MEDIA_TYPE: &str = "application/vnd.dev.sigstore.bundle+json;version=0.1";

/// Maximum accepted size of one simplesigning payload layer, in bytes (cosign's own are ~256).
const MAX_SIMPLESIGNING_PAYLOAD_BYTES: usize = 64 * 1024;

/// Which cosign sidecar a tag names.
// No `.sbom` variant: its layer keeps the SBOM's own type, which [`read_sidecar_manifest`] skips; see
// `read_sbom_sidecar_tag` in `super::pipeline`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidecarKind {
    /// `sha256-<hex>.sig` — image signatures.
    Signature,
    /// `sha256-<hex>.att` — attestations.
    Attestation,
}

impl SidecarKind {
    /// The tag suffix, including the leading dot.
    // From `ocx_oci::tag`, so this reader never asks for a suffix the version classifier stopped reserving.
    pub const fn suffix(self) -> &'static str {
        match self {
            Self::Signature => ocx_oci::tag::SIG_SIDECAR_SUFFIX,
            Self::Attestation => ocx_oci::tag::ATT_SIDECAR_SUFFIX,
        }
    }
}

/// The cosign sidecar tag naming `subject`'s signatures of `kind`.
pub fn sidecar_tag(subject: &Digest, kind: SidecarKind) -> String {
    ocx_oci::tag::sidecar_tag(subject, kind.suffix())
}

/// Everything a sidecar layer is judged against: the crypto, the policies, and the transparency-log requirements.
pub struct SidecarVerification<'a> {
    /// The `sigstore` verifier the bundle path is handed.
    pub verifier: &'a Verifier,
    /// The resolved ANY-of trust policies.
    pub policies: &'a [CompiledPolicy],
    /// Supplies the pinned Rekor public key, the only source an offline run can use.
    pub trust_root: &'a TrustRoot,
    /// Where an unpinned Rekor public key is fetched from, online.
    pub rekor_url: &'a Url,
    /// No Sigstore trust-services network: an unpinned key is a refusal.
    pub offline: bool,
    /// `--allow-unlogged-signature`.
    pub allow_unlogged: bool,
    /// The run's resolved Rekor log keys; a clone shares the memo.
    pub rekor_keys: RekorKeyMemo,
}

/// What one sidecar manifest yielded: the layers that verified, and the ones examined and refused.
#[derive(Debug, Default)]
pub struct SidecarScan {
    /// Every simplesigning layer that verified, in manifest order.
    pub verified: Vec<VerifiedSignature>,
    /// Every simplesigning layer examined and refused, in manifest order, keyed by **layer** digest.
    pub refused: Vec<RefusedCandidate>,
}

/// Fetch a cosign sidecar tag and verify every simplesigning layer it carries; `Ok(None)` when the tag is absent.
///
/// # Errors
///
/// A registry failure other than a missing manifest, or an over-cap or unparseable sidecar manifest.
/// A layer failure lands in [`SidecarScan::refused`] instead.
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

/// Verify every simplesigning layer of an already-fetched sidecar manifest; other layers are skipped, not refused.
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
    let layers: Vec<&Descriptor> = manifest
        .layers
        .iter()
        .filter(|layer| layer.media_type == SIMPLESIGNING_MEDIA_TYPE)
        .collect();
    if layers.len() > MAX_SIGNATURE_CANDIDATES {
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
async fn pull_payload(
    transport: &dyn OciTransport,
    image: &native::Reference,
    layer: &Descriptor,
) -> Result<Vec<u8>, VerifyErrorKind> {
    // The declared size is untrusted: only a pre-fetch reject; `pull_blob_capped` bounds the read itself (CWE-400).
    if layer.size < 0 || layer.size as usize > MAX_SIMPLESIGNING_PAYLOAD_BYTES {
        return Err(VerifyErrorKind::BundleParseFailed);
    }
    let digest = Digest::try_from(layer.digest.as_str()).map_err(|_| VerifyErrorKind::BundleParseFailed)?;
    pull_blob_capped(transport, image, &digest, MAX_SIMPLESIGNING_PAYLOAD_BYTES).await
}

/// Verify one simplesigning layer against `subject_digest`.
///
/// `payload` is the layer body exactly as served; every signature check covers those bytes, never a re-serialized
/// claim, or a non-identical round trip becomes a silent bypass.
///
/// # Errors
///
/// One layer's verdict, never the sidecar's.
pub(super) async fn verify_layer(
    layer: &Descriptor,
    payload: &[u8],
    subject_digest: &Digest,
    verify: &SidecarVerification<'_>,
    via: DiscoveryMethod,
) -> Result<VerifiedSignature, VerifyErrorKind> {
    let layer_digest = Digest::try_from(layer.digest.as_str()).map_err(|_| VerifyErrorKind::BundleParseFailed)?;
    let signature = layer_signature(layer)?;

    // Before any delegated call, so the user sees OCX's precise refusal kinds.
    check_claim(payload, subject_digest)?;

    let signer = match layer_certificate(layer)? {
        Some(leaf_der) => {
            // Before chain and identity: the certificate window anchors on this entry's `integratedTime`.
            let logged = logged_entry(layer, verify).await?;
            verify_keyless(&leaf_der, layer_chain(layer)?, payload, &signature, logged, verify).await?
        }
        // Key mode leaves the bundle annotation unread: crediting it would add a `signed_at` the key never proved.
        None => {
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
            // Only from a SET-verified entry bound to this signature; otherwise nothing proved a signing time.
            signed_at: signer
                .logged
                .as_ref()
                .and_then(|entry| u64::try_from(entry.integrated_time).ok()),
            signature_format: SignatureFormat::Simplesigning,
            discovery_method: via,
            // Never `sidecar_bundle`'s synthesized `log_index: 0`, or every unlogged signature collapses into one dedup
            // row.
            rekor_log_index: signer.logged.as_ref().map(|entry| entry.log_index),
        },
        signature,
    })
}

/// The facts the two key models establish differently.
struct VerifiedSigner {
    key_backend: KeyBackendKind,
    certificate_identity: Option<String>,
    certificate_oidc_issuer: Option<String>,
    /// The verified transparency-log entry, when the layer carried one.
    logged: Option<LoggedEntry>,
}

/// A sidecar layer's transparency-log entry whose SET verified against the log's own key; not yet bound to anything.
// Fields stay private so only `logged_entry` can mint one; a `pub(super)` field lets an unverified annotation pass.
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

/// The full keyless gate over an annotation certificate, as strict as the bundle path's.
///
/// A `None` `logged` is refused as [`VerifyErrorKind::SignatureInvalid`] unless `allow_unlogged` is set.
async fn verify_keyless(
    leaf_der: &[u8],
    chain_ders: Vec<Vec<u8>>,
    payload: &[u8],
    signature: &[u8],
    logged: Option<LoggedEntry>,
    verify: &SidecarVerification<'_>,
) -> Result<VerifiedSigner, VerifyErrorKind> {
    let cert = parse_certificate(leaf_der)?;
    // Under the opt-out the leaf's own `notBefore` stands in; a bare `i64`, so it is never passed as a
    // `SigningInstant`.
    let anchor = logged.as_ref().map_or_else(
        || i64::try_from(cert.tbs_certificate.validity.not_before.to_unix_duration().as_secs()).unwrap_or(i64::MAX),
        |entry| entry.integrated_time,
    );
    let bundle = sidecar_bundle(&cert, leaf_der, chain_ders, payload, signature, anchor)?;

    // `offline: true`: `logged_entry` already checked the entry, and the synthesized one carries no evidence.
    if let Err(error) = verify
        .verifier
        .verify(payload, bundle, &PolicyDeferredToOcx, true)
        .await
    {
        return Err(map_verification_error(error));
    }

    matching_policies(leaf_der, verify.policies)?;

    match logged.as_ref() {
        Some(entry) => {
            // After the signature check, or a flipped signature annotation reports `transparency_body_mismatch`.
            bind_logged_body(&entry.body, payload, signature)?;
            tlog::verify_integrated_time_within_certificate(
                SigningInstant::TransparencyLog(entry.integrated_time),
                &cert,
            )?;
        }
        // Skip the window check rather than feed it `notBefore`, against which it can never fail.
        None if verify.allow_unlogged => {
            tracing::debug!(
                "accepting a keyless sidecar with no transparency-log evidence (--allow-unlogged-signature)"
            );
        }
        // Refused last, so a wrong identity still reports `identity_mismatch`.
        None => return Err(VerifyErrorKind::SignatureInvalid),
    }

    Ok(VerifiedSigner {
        key_backend: KeyBackendKind::Keyless,
        certificate_identity: subject_identity(&cert),
        certificate_oidc_issuer: oidc_issuer(&cert),
        logged,
    })
}

/// cosign's `dev.sigstore.cosign/bundle` annotation, SET-verified, or `None` when the layer carries none.
///
/// Binding the logged body is the caller's job: [`bind_logged_body`] or [`super::dsse::verify_tlog_binding`].
///
/// # Errors
///
/// [`VerifyErrorKind::BundleParseFailed`] for malformed JSON; [`VerifyErrorKind::RekorSetInvalid`] when its base64
/// or SET does not hold; [`VerifyErrorKind::TransparencyLogUnavailable`] when the log's key can be neither read nor
/// fetched.
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
    // Membership only: a real SET over another artifact's entry passes until the caller binds the body.
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

/// Refuse a logged `hashedrekord` body that is not about **this** signature over **this** payload.
// `publicKey` is not compared: cosign's uploaded PEM is not re-derivable byte for byte, so it would refuse genuine
// entries.
fn bind_logged_body(body: &[u8], payload: &[u8], signature: &[u8]) -> Result<(), VerifyErrorKind> {
    let logged: sigstore::rekor::models::Hashedrekord =
        serde_json::from_slice(body).map_err(|_| VerifyErrorKind::TransparencyBodyMismatch)?;
    if !matches!(logged.spec.data.hash.algorithm, hashedrekord::AlgorithmKind::sha256) {
        return Err(VerifyErrorKind::TransparencyBodyMismatch);
    }
    let payload_digest = ocx_oci::Algorithm::Sha256.hash(payload);
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
// Field names are cosign's Go struct tags (wire); integers stay signed so a negative one is rejected, never wrapped.
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

/// The Sigstore bundle a sidecar layer's material describes, so [`Verifier`] runs the bundle path's keyless gate.
///
/// Its one log entry is synthesized to satisfy `CheckedBundle` and is not evidence; `integrated_time` anchors the
/// library's time checks.
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
    let payload_digest = ocx_oci::Algorithm::Sha256.hash(payload);

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
            // Absent: `sigstore` never reads it, so a value would be an unchecked claim about what was signed.
            message_digest: None,
            signature: signature.to_vec(),
        })),
    })
}

/// Check the two `critical` fields a verifier is required to understand.
fn check_claim(payload: &[u8], subject_digest: &Digest) -> Result<(), VerifyErrorKind> {
    let claim: SimpleSigningClaim = serde_json::from_slice(payload).map_err(|_| VerifyErrorKind::BundleParseFailed)?;

    if claim.critical.claim_type != SIMPLESIGNING_CLAIM_TYPE {
        return Err(VerifyErrorKind::SimpleSigningClaimUnsupported {
            claim_type: claim.critical.claim_type,
        });
    }
    // The cross-subject splice guard: a genuine signature over another manifest is valid in every other respect.
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
        // A chain with no leaf is malformed, not key mode.
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
    //! `crate::simplesigning`'s own tests set.
    //!
    //! The trust root is the committed local Fulcio/CT/Rekor material under
    //! `test/sigstore`, so every keyless assertion here runs fully offline with
    //! no container.
    use super::*;
    use crate::verify::trust_root::TrustRoot;
    use ocx_oci::client::test_transport::{StubTransport, StubTransportData};
    use ocx_trust::{CompiledKeyless, IdentityRule, PolicyBackend};
    use sigstore::rekor::apis::configuration::Configuration as RekorConfiguration;

    const KEY_MANIFEST: &str = include_str!("../../../../test/tests/fixtures/golden/simplesigning_key_manifest.json");
    const KEY_PAYLOAD: &[u8] = include_bytes!("../../../../test/tests/fixtures/golden/simplesigning_key_payload.json");
    const KEYLESS_MANIFEST: &str =
        include_str!("../../../../test/tests/fixtures/golden/simplesigning_keyless_manifest.json");
    const KEYLESS_PAYLOAD: &[u8] =
        include_bytes!("../../../../test/tests/fixtures/golden/simplesigning_keyless_payload.json");
    const COSIGN_PUBLIC_KEY_PEM: &str = include_str!("../../../../test/tests/fixtures/golden/keys/cosign.pub");
    const TRUSTED_ROOT_JSON: &[u8] = include_bytes!("../../../../test/sigstore/trusted_root.json");
    /// G0's keyless golden bundle, borrowed for its **transparency-log entry**
    /// alone: a real `integratedTime`, `logIndex`, `logID` and
    /// `canonicalizedBody` under a Signed Entry Timestamp the committed trust
    /// root's Rekor key actually verifies. No sidecar fixture carries a
    /// `dev.sigstore.cosign/bundle` annotation (cosign v3.1.1 writes none), so
    /// this is the only committed material a positive SET assertion can be
    /// built from — and a positive one is what makes the tampered half mean
    /// anything.
    const GOLDEN_KEYLESS_BUNDLE: &str = include_str!("../../../../test/tests/fixtures/golden/keyless_bundle.json");

    /// D-authored negative fixtures (spec D7 — simplesigning read fixtures are
    /// committed bytes).
    const FOREIGN_SUBJECT_PAYLOAD: &[u8] =
        include_bytes!("../../../../test/tests/fixtures/simplesigning/foreign_subject_payload.json");
    const FOREIGN_CLAIM_TYPE_PAYLOAD: &[u8] =
        include_bytes!("../../../../test/tests/fixtures/simplesigning/foreign_claim_type_payload.json");
    const UNTRUSTED_CA_MANIFEST: &str =
        include_str!("../../../../test/tests/fixtures/simplesigning/untrusted_ca_manifest.json");
    const TAMPERED_SIGNATURE_MANIFEST: &str =
        include_str!("../../../../test/tests/fixtures/simplesigning/tampered_signature_manifest.json");
    const PUBLISHER_FORMATTED_PAYLOAD: &[u8] =
        include_bytes!("../../../../test/tests/fixtures/simplesigning/publisher_formatted_payload.json");
    const PUBLISHER_FORMATTED_MANIFEST: &str =
        include_str!("../../../../test/tests/fixtures/simplesigning/publisher_formatted_manifest.json");

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
            ocx_oci::Algorithm::Sha256.hash(PUBLISHER_FORMATTED_PAYLOAD).to_string(),
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
                    ocx_oci::OCI_IMAGE_MEDIA_TYPE.to_owned(),
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
        let payload_hex = ocx_oci::Algorithm::Sha256.hash(KEYLESS_PAYLOAD).hex().to_owned();

        let bound = logged_body(&payload_hex, &signature_base64, "sha256");
        assert!(
            bind_logged_body(&bound, KEYLESS_PAYLOAD, &signature).is_ok(),
            "an entry naming this payload's digest and this signature must bind"
        );

        // The splice: a genuine entry about another artifact. Its digest comes
        // from a committed payload rather than a literal, so it is a real
        // artifact's hash and not a value chosen to differ.
        let foreign_hex = ocx_oci::Algorithm::Sha256
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
