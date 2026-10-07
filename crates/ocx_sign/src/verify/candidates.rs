// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Non-verifying signature discovery: "is there already a signature here", for `ocx-mirror`'s backfill.

use sigstore_protobuf_specs::dev::sigstore::bundle::v1::{bundle, verification_material};

use super::DiscoveryMethod;
use super::dsse::is_cosign_image_signature;
use super::identity::{oidc_issuer, parse_certificate, subject_identity};
use super::pipeline::{MAX_REFERRER_MANIFEST_BYTES, MAX_SIGNATURE_CANDIDATES};
use super::simplesigning_read::{SidecarKind, sidecar_tag};
use crate::sign::bundle::{MAX_BUNDLE_SIZE_BYTES, parse_bundle};
use ocx_oci::client::error::ClientError;
use ocx_oci::client::{OciTransport, sibling_tag_reference};
use ocx_oci::media_type::SIGNABLE_MANIFEST_TYPES;
use ocx_oci::referrer::media_types::{COSIGN_SIG_ARTIFACT_TYPE, SIGSTORE_BUNDLE_V03};

/// A signature candidate attached to a subject — **no verification performed on any field**.
///
/// Deciding policy from these values trusts whoever could write to the registry; only
/// [`crate::verify::VerifyPipeline`] answers "is this signature good".
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SignerCandidate {
    pub discovery: DiscoveryMethod,
    /// The referrer or sidecar manifest digest.
    pub digest: ocx_oci::Digest,
    pub artifact_type: Option<String>,
    /// Certificate SAN, for a keyless signature. **Unvalidated.**
    pub certificate_identity: Option<String>,
    /// Certificate OIDC issuer, for a keyless signature. **Unvalidated.**
    pub certificate_issuer: Option<String>,
    /// `verificationMaterial.publicKey.hint`, for a key-pair signature; a forged hint only fails verification.
    pub public_key_hint: Option<String>,
}

impl SignerCandidate {
    /// Constructs a candidate from the two fields every candidate carries; identity attaches via `with_*`.
    ///
    /// # Examples
    ///
    /// ```
    /// use ocx_oci::Algorithm;
    /// use ocx_sign::verify::{DiscoveryMethod, SignerCandidate};
    ///
    /// let candidate = SignerCandidate::new(DiscoveryMethod::ReferrersApi, Algorithm::Sha256.hash(b"bundle"))
    ///     .with_certificate_identity("ocx-test@example.com")
    ///     .with_certificate_issuer("https://accounts.google.com");
    ///
    /// assert_eq!(candidate.certificate_identity.as_deref(), Some("ocx-test@example.com"));
    /// assert_eq!(candidate.public_key_hint, None);
    /// ```
    pub fn new(discovery: DiscoveryMethod, digest: ocx_oci::Digest) -> Self {
        Self {
            discovery,
            digest,
            artifact_type: None,
            certificate_identity: None,
            certificate_issuer: None,
            public_key_hint: None,
        }
    }

    pub fn with_artifact_type(mut self, artifact_type: impl Into<String>) -> Self {
        self.artifact_type = Some(artifact_type.into());
        self
    }

    /// Attaches the certificate SAN. **Unvalidated**, like the field.
    pub fn with_certificate_identity(mut self, identity: impl Into<String>) -> Self {
        self.certificate_identity = Some(identity.into());
        self
    }

    /// Attaches the certificate OIDC issuer. **Unvalidated**, like the field.
    pub fn with_certificate_issuer(mut self, issuer: impl Into<String>) -> Self {
        self.certificate_issuer = Some(issuer.into());
        self
    }

    pub fn with_public_key_hint(mut self, hint: impl Into<String>) -> Self {
        self.public_key_hint = Some(hint.into());
        self
    }
}

/// Lists the signature candidates attached to `subject` (referrers plus the `.sig` sidecar), verifying none.
///
/// A `.sig` sidecar, or a referrer whose bytes do not parse, yields a candidate with every identity field `None`.
///
/// Not `.att` or `.sbom`: both attach to unsigned subjects, so counting them makes a backfill skip what it must sign.
///
/// # Errors
///
/// Whatever the transport raises; an absent signature is an empty vector, never an error.
pub async fn list_signature_candidates(
    transport: &dyn OciTransport,
    image: &ocx_oci::native::Reference,
    subject: &ocx_oci::Digest,
) -> Result<Vec<SignerCandidate>, ClientError> {
    let listing = transport.list_referrers_with_fallback(image, subject, None).await?;
    let signature_referrers: Vec<ocx_oci::Descriptor> = listing
        .descriptors
        .into_iter()
        .filter(|descriptor| is_signature_artifact_type(descriptor.artifact_type.as_deref()))
        .collect();
    if signature_referrers.len() > MAX_SIGNATURE_CANDIDATES {
        tracing::debug!(
            "subject carries {} signature referrers; listing the first {MAX_SIGNATURE_CANDIDATES}",
            signature_referrers.len()
        );
    }

    let mut found = Vec::new();
    for descriptor in signature_referrers.into_iter().take(MAX_SIGNATURE_CANDIDATES) {
        // Skip, never abort: one malformed sibling must not fail the whole subject.
        let Ok(digest) = ocx_oci::Digest::try_from(descriptor.digest.as_str()) else {
            continue;
        };
        // `None` is an attestation wearing the signature artifact type.
        let Some(identity) = read_referrer_identity(transport, image, &descriptor).await? else {
            continue;
        };
        found.push(SignerCandidate {
            discovery: listing.via,
            digest,
            artifact_type: descriptor.artifact_type,
            certificate_identity: identity.certificate_identity,
            certificate_issuer: identity.certificate_issuer,
            public_key_hint: identity.public_key_hint,
        });
    }

    if let Some(sidecar) = read_signature_sidecar(transport, image, subject).await? {
        found.push(sidecar);
    }
    Ok(found)
}

/// Whether a referrer's `artifactType` names a signature.
///
/// An untyped referrer is not a candidate: `VerifyPipeline` accepts one only because it then verifies it.
fn is_signature_artifact_type(artifact_type: Option<&str>) -> bool {
    artifact_type.is_some_and(|declared| declared == SIGSTORE_BUNDLE_V03 || declared == COSIGN_SIG_ARTIFACT_TYPE)
}

/// The `.sig` sidecar, if the tag exists; any failure but a missing tag propagates, never reading as unsigned.
async fn read_signature_sidecar(
    transport: &dyn OciTransport,
    image: &ocx_oci::native::Reference,
    subject: &ocx_oci::Digest,
) -> Result<Option<SignerCandidate>, ClientError> {
    let target = sibling_tag_reference(image, sidecar_tag(subject, SidecarKind::Signature));
    let digest = match transport.pull_manifest_raw(&target, SIGNABLE_MANIFEST_TYPES).await {
        Ok((_bytes, digest)) => digest,
        Err(ClientError::ManifestNotFound(_)) => return Ok(None),
        Err(other) => return Err(other),
    };
    Ok(Some(SignerCandidate {
        discovery: DiscoveryMethod::SidecarTag,
        // Transport-computed, so a parse failure is our bug and propagates.
        digest: ocx_oci::Digest::try_from(digest.as_str())
            .map_err(|error| ClientError::InvalidManifest(error.to_string()))?,
        artifact_type: None,
        // Per-layer certificates, many layers: no single identity to report.
        certificate_identity: None,
        certificate_issuer: None,
        public_key_hint: None,
    }))
}

/// What a bundle claims about its signer, with none of it checked.
#[derive(Default)]
struct BundleIdentity {
    certificate_identity: Option<String>,
    certificate_issuer: Option<String>,
    public_key_hint: Option<String>,
}

/// Reads `descriptor`'s referrer manifest and bundle blob, returning the identity the bundle claims.
///
/// Unreadable content yields an empty identity, never an error; `Ok(None)` means an attestation to drop.
async fn read_referrer_identity(
    transport: &dyn OciTransport,
    image: &ocx_oci::native::Reference,
    descriptor: &ocx_oci::Descriptor,
) -> Result<Option<BundleIdentity>, ClientError> {
    let referrer_ref = image.clone_with_digest(descriptor.digest.clone());
    let (manifest_bytes, _) = transport
        .pull_manifest_raw(&referrer_ref, SIGNABLE_MANIFEST_TYPES)
        .await?;
    if manifest_bytes.len() as u64 > MAX_REFERRER_MANIFEST_BYTES {
        return Ok(Some(BundleIdentity::default()));
    }
    let Ok(manifest) = serde_json::from_slice::<ocx_oci::referrer::ReferrerManifest>(&manifest_bytes) else {
        return Ok(Some(BundleIdentity::default()));
    };
    let Some(layer) = manifest.layers.first() else {
        return Ok(Some(BundleIdentity::default()));
    };
    // The declared size is untrusted: a cheap pre-reject only; `pull_bundle_capped` bounds the read (CWE-400).
    if !usize::try_from(layer.size).is_ok_and(|declared| declared <= MAX_BUNDLE_SIZE_BYTES) {
        return Ok(Some(BundleIdentity::default()));
    }
    let Ok(blob_digest) = ocx_oci::Digest::try_from(layer.digest.as_str()) else {
        return Ok(Some(BundleIdentity::default()));
    };
    let Some(bundle_bytes) = pull_bundle_capped(transport, image, &blob_digest).await? else {
        return Ok(Some(BundleIdentity::default()));
    };
    Ok(read_bundle_identity(&bundle_bytes))
}

/// Reads at most `cap + 1` bytes, so a registry lying about the size cannot make us buffer it all.
async fn pull_bundle_capped(
    transport: &dyn OciTransport,
    image: &ocx_oci::native::Reference,
    blob_digest: &ocx_oci::Digest,
) -> Result<Option<Vec<u8>>, ClientError> {
    use tokio::io::AsyncReadExt as _;

    let reader = transport.pull_blob_streaming(image, blob_digest).await?;
    let mut bytes = Vec::new();
    reader
        .take(MAX_BUNDLE_SIZE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| ClientError::Registry(Box::new(error)))?;
    Ok((bytes.len() <= MAX_BUNDLE_SIZE_BYTES).then_some(bytes))
}

/// Splits a bundle's verification material into identity fields, validating none.
///
/// `None` is an attestation: `artifactType` is producer-set, so the DSSE `predicateType` discriminates.
fn read_bundle_identity(bundle_bytes: &[u8]) -> Option<BundleIdentity> {
    let Some(bundle) = parse_bundle(bundle_bytes, MAX_BUNDLE_SIZE_BYTES) else {
        return Some(BundleIdentity::default());
    };
    if let Some(bundle::Content::DsseEnvelope(envelope)) = bundle.content.as_ref()
        && !is_cosign_image_signature(envelope)
    {
        return None;
    }
    let Some(content) = bundle.verification_material.and_then(|material| material.content) else {
        return Some(BundleIdentity::default());
    };
    let leaf_der = match content {
        verification_material::Content::Certificate(certificate) => certificate.raw_bytes,
        verification_material::Content::X509CertificateChain(chain) => match chain.certificates.into_iter().next() {
            Some(certificate) => certificate.raw_bytes,
            None => return Some(BundleIdentity::default()),
        },
        verification_material::Content::PublicKey(key) => {
            return Some(BundleIdentity {
                public_key_hint: Some(key.hint),
                ..BundleIdentity::default()
            });
        }
    };
    let Ok(certificate) = parse_certificate(&leaf_der) else {
        return Some(BundleIdentity::default());
    };
    Some(BundleIdentity {
        certificate_identity: subject_identity(&certificate),
        certificate_issuer: oidc_issuer(&certificate),
        public_key_hint: None,
    })
}

#[cfg(test)]
mod tests;
