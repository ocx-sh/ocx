// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Attaching a referrer, on a registry that indexes referrers and on one that does not.

use sigstore_protobuf_specs::dev::sigstore::bundle::v1::{bundle, verification_material};
use url::Url;

use crate::attest::MAX_ATTESTATION_ENVELOPE_BYTES;
use crate::sign::bundle::parse_bundle;
use crate::sign::error::SignErrorKind;
use crate::sign::oidc::OidcToken;
use crate::sign::pipeline::LegDigests;
use crate::sign::signer::{Signer, SignerIdentity};
use crate::sign::state::SigningStatePaths;
use crate::verify::pipeline::{MAX_REFERRER_MANIFEST_BYTES, MAX_SIGNATURE_CANDIDATES};
use ocx_oci::client::OciTransport;
use ocx_oci::client::error::ClientError;
use ocx_oci::media_type::SIGNABLE_MANIFEST_TYPES;
use ocx_oci::referrer::ReferrerManifest;
use ocx_oci::referrer::capability::{ReferrersApiCapability, ReferrersSupport};
use ocx_oci::referrer::media_types::{ANNOTATION_BUNDLE_PREDICATE_TYPE, SIGSTORE_BUNDLE_V03};
use ocx_oci::{Algorithm, Descriptor, Digest, OCI_IMAGE_MEDIA_TYPE, native};

/// The Referrers-API verdict for the host a referrer is about to be pushed to, via the capability cache.
///
/// `image` must be the write reference, or a mirror's verdict is cached against the upstream.
pub(crate) async fn referrers_capability(
    transport: &dyn OciTransport,
    image: &native::Reference,
    subject_digest: &Digest,
    state: &SigningStatePaths,
    no_cache: bool,
) -> Result<ReferrersSupport, SignErrorKind> {
    let cached = if no_cache {
        None
    } else {
        ReferrersApiCapability::from_cache(
            image.resolve_registry(),
            &state.referrers_capability_file(image.resolve_registry()),
        )
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
            // Best-effort cache write; a failure here must not fail the sign.
            let _ = probed
                .write_cache(&state.referrers_capability_file(&probed.registry))
                .await;
            probed
        }
    };
    Ok(capability.supported)
}

/// Push a referrer manifest and, on a registry without the Referrers API, name it in
/// the OCI tag-schema fallback index too (`adr_oci_referrers_signing_v1.md § Amendment 10`).
///
/// `write_image` must come from `Client::transport_write_reference`, or the fallback index lands on a mirror.
///
/// # Errors
///
/// The manifest PUT's error, or on an `Unsupported` registry the append's.
pub(crate) async fn attach_referrer(
    transport: &dyn OciTransport,
    write_image: &native::Reference,
    subject_digest: &Digest,
    manifest_bytes: &[u8],
    support: ReferrersSupport,
) -> Result<(Digest, Descriptor), SignErrorKind> {
    let referrer_descriptor = transport
        .push_referrer_manifest(write_image, subject_digest, manifest_bytes, OCI_IMAGE_MEDIA_TYPE)
        .await
        .map_err(map_client_error)?;

    // Skipped when supported: the mutable fallback tag would add an attacker-writable second source of truth.
    if support == ReferrersSupport::Unsupported {
        transport
            .append_referrer_fallback_index(write_image, subject_digest, &referrer_descriptor)
            .await
            .map_err(map_client_error)?;
    }

    let referrer_digest =
        Digest::try_from(referrer_descriptor.digest.as_str()).map_err(|e| SignErrorKind::Internal(Box::new(e)))?;
    Ok((referrer_digest, referrer_descriptor))
}

/// Asks the signer who it will sign as at most once per run, and only when something could match.
pub(crate) struct IdentityProbe<'a> {
    signer: &'a dyn Signer,
    token: Option<&'a OidcToken>,
    fulcio_url: &'a Url,
    known: Option<Option<SignerIdentity>>,
}

impl<'a> IdentityProbe<'a> {
    pub(crate) fn new(signer: &'a dyn Signer, token: Option<&'a OidcToken>, fulcio_url: &'a Url) -> Self {
        Self {
            signer,
            token,
            fulcio_url,
            known: None,
        }
    }

    /// # Errors
    ///
    /// The signer's, e.g. a Fulcio refusal; the sign that follows would hit the same one.
    pub(crate) async fn get(&mut self) -> Result<Option<SignerIdentity>, SignErrorKind> {
        if let Some(known) = &self.known {
            return Ok(known.clone());
        }
        let identity = self.signer.identity(self.token, self.fulcio_url).await?;
        self.known = Some(identity.clone());
        Ok(identity)
    }
}

/// A signature already published for the subject under the signer's identity.
pub(crate) struct ExistingSignature {
    pub(crate) digests: LegDigests,
    pub(crate) identity: SignerIdentity,
    pub(crate) transparency_log_index: Option<u64>,
}

/// The bundle referrer of `predicate_type` that `probe`'s identity already attached to `subject`, if any.
///
/// With `statement`, the bundle's DSSE payload must be those exact bytes: a different document is a new attestation.
///
/// Fail-open: only the first `MAX_SIGNATURE_CANDIDATES` bundle referrers are read, and an unreadable
/// listing or referrer counts as absent, so the worst case is one more referrer, as before.
/// The claim is unverified: a forged one can only suppress our write, which push access allows anyway.
///
/// # Errors
///
/// Only the probe's.
pub(crate) async fn find_bundle_referrer(
    transport: &dyn OciTransport,
    image: &native::Reference,
    subject: &Digest,
    predicate_type: &str,
    statement: Option<&[u8]>,
    probe: &mut IdentityProbe<'_>,
) -> Result<Option<ExistingSignature>, SignErrorKind> {
    let listing = match transport.list_referrers_with_fallback(image, subject, None).await {
        Ok(listing) => listing,
        Err(error) => {
            // Fail-open: refusing the sign over a lookup would be worse than today's duplicate.
            tracing::debug!("cannot list referrers to look for an existing signature: {error}");
            return Ok(None);
        }
    };
    let mut candidates = Vec::new();
    for descriptor in listing
        .descriptors
        .into_iter()
        .filter(|descriptor| descriptor.artifact_type.as_deref() == Some(SIGSTORE_BUNDLE_V03))
        .take(MAX_SIGNATURE_CANDIDATES)
    {
        if let Some(candidate) = bundle_candidate(transport, image, subject, predicate_type, &descriptor).await {
            candidates.push(candidate);
        }
    }
    if candidates.is_empty() {
        return Ok(None);
    }
    // After the listing: a keyless probe costs a Fulcio certificate, spent only when something could match.
    let Some(identity) = probe.get().await? else {
        return Ok(None);
    };
    for digests in candidates {
        let Some(bundle) = pull_capped(transport, image, &digests.payload_digest).await else {
            continue;
        };
        let Some(bundle) = parse_bundle(&bundle, MAX_ATTESTATION_ENVELOPE_BYTES) else {
            continue;
        };
        if let Some(wanted) = statement
            && !matches!(&bundle.content, Some(bundle::Content::DsseEnvelope(envelope)) if envelope.payload == wanted)
        {
            continue;
        }
        let Some(material) = bundle.verification_material else {
            continue;
        };
        let transparency_log_index = material
            .tlog_entries
            .first()
            .and_then(|entry| u64::try_from(entry.log_index).ok());
        let (claimed_identity, claimed_issuer, claimed_hint) = match material.content {
            Some(verification_material::Content::Certificate(certificate)) => certificate_claim(&certificate.raw_bytes),
            Some(verification_material::Content::X509CertificateChain(chain)) => {
                chain.certificates.first().map_or((None, None, None), |certificate| {
                    certificate_claim(&certificate.raw_bytes)
                })
            }
            Some(verification_material::Content::PublicKey(key)) => (None, None, Some(key.hint)),
            None => continue,
        };
        if identity.matches(
            claimed_identity.as_deref(),
            claimed_issuer.as_deref(),
            claimed_hint.as_deref(),
        ) {
            return Ok(Some(ExistingSignature {
                digests,
                identity,
                transparency_log_index,
            }));
        }
    }
    Ok(None)
}

/// The referrer's addresses when it is a bundle for `subject` declaring `predicate_type`.
async fn bundle_candidate(
    transport: &dyn OciTransport,
    image: &native::Reference,
    subject: &Digest,
    predicate_type: &str,
    descriptor: &Descriptor,
) -> Option<LegDigests> {
    let manifest_digest = Digest::try_from(descriptor.digest.as_str()).ok()?;
    let (bytes, _) = transport
        .pull_manifest_raw(
            &image.clone_with_digest(descriptor.digest.clone()),
            SIGNABLE_MANIFEST_TYPES,
        )
        .await
        .ok()?;
    if bytes.len() as u64 > MAX_REFERRER_MANIFEST_BYTES || Algorithm::Sha256.hash(&bytes) != manifest_digest {
        return None;
    }
    let manifest: ReferrerManifest = serde_json::from_slice(&bytes).ok()?;
    let declared = manifest.annotations.as_ref()?.get(ANNOTATION_BUNDLE_PREDICATE_TYPE)?;
    if declared != predicate_type || manifest.subject.digest != subject.to_string() {
        return None;
    }
    let layer = manifest.layers.first()?;
    Some(LegDigests {
        payload_digest: Digest::try_from(layer.digest.as_str()).ok()?,
        manifest_digest,
    })
}

/// SAN and issuer of a DER leaf, unvalidated.
fn certificate_claim(der: &[u8]) -> (Option<String>, Option<String>, Option<String>) {
    match crate::verify::identity::parse_certificate(der) {
        Ok(leaf) => (
            crate::verify::identity::subject_identity(&leaf),
            crate::verify::identity::oidc_issuer(&leaf),
            None,
        ),
        Err(_) => (None, None, None),
    }
}

/// A blob whose bytes hash to `digest`, read at most one byte past the cap so a lying registry cannot make us buffer it all.
pub(crate) async fn pull_capped(
    transport: &dyn OciTransport,
    image: &native::Reference,
    digest: &Digest,
) -> Option<Vec<u8>> {
    use tokio::io::AsyncReadExt as _;
    let reader = transport.pull_blob_streaming(image, digest).await.ok()?;
    let mut bytes = Vec::new();
    reader
        .take(MAX_ATTESTATION_ENVELOPE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .ok()?;
    (bytes.len() <= MAX_ATTESTATION_ENVELOPE_BYTES && &Algorithm::Sha256.hash(&bytes) == digest).then_some(bytes)
}

/// Map an OCI client error into the sign taxonomy.
///
/// Everything but `ReferrersUnsupported` stays intact under `Internal`, so the cause supplies its own exit code.
pub fn map_client_error(error: ClientError) -> SignErrorKind {
    match error {
        ClientError::ReferrersUnsupported { .. } => SignErrorKind::ReferrersUnsupported,
        other => SignErrorKind::Internal(Box::new(other)),
    }
}
