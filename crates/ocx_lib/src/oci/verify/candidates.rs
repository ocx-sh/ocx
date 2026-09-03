// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Non-verifying signature discovery — OCX-C-2.
//!
//! `ocx-mirror`'s signature backfill needs the cheap question "is there
//! already a signature here", not the expensive one `ocx package verify`
//! answers. This module lists what a subject carries without checking any of
//! it, so a caller can pre-filter before spending a full verify.

use sigstore_protobuf_specs::dev::sigstore::bundle::v1::{bundle, verification_material};

use super::discovery::DiscoveryMethod;
use super::dsse::is_cosign_image_signature;
use super::identity::{oidc_issuer, parse_certificate, subject_identity};
use super::pipeline::{ACCEPTED_MANIFEST_TYPES, MAX_REFERRER_MANIFEST_BYTES, MAX_SIGNATURE_CANDIDATES};
use super::simplesigning_read::{SidecarKind, sidecar_tag};
use crate::oci;
use crate::oci::client::error::ClientError;
use crate::oci::client::{OciTransport, sibling_tag_reference};
use crate::oci::referrer::media_types::{COSIGN_SIG_ARTIFACT_TYPE, SIGSTORE_BUNDLE_V03};
use crate::oci::sign::bundle::{MAX_BUNDLE_SIZE_BYTES, parse_bundle};

/// A signature candidate attached to a subject, with the identity fields a
/// caller can match on — and **no verification performed on any of them**.
///
/// Every identity field below was read out of a certificate whose chain was
/// not checked, or out of a bundle whose signature was not verified. A caller
/// deciding policy from these values is trusting whoever could write to the
/// registry. [`crate::oci::verify::VerifyPipeline`] remains the only answer
/// to "is this signature good"; this type exists so a sweep can ask the
/// cheaper question "is there already one here".
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SignerCandidate {
    /// How this candidate was found.
    pub discovery: DiscoveryMethod,
    /// The referrer or sidecar manifest digest.
    pub digest: oci::Digest,
    /// The referrer's `artifactType`, when it carried one.
    pub artifact_type: Option<String>,
    /// Certificate SAN, for a keyless signature. **Unvalidated.**
    pub certificate_identity: Option<String>,
    /// Certificate OIDC issuer, for a keyless signature. **Unvalidated.**
    pub certificate_issuer: Option<String>,
    /// `verificationMaterial.publicKey.hint` — the SHA-256 of the DER public
    /// key — for a key-pair signature. Self-authenticating against a
    /// configured key: a forged hint only yields a signature that then fails
    /// verification.
    pub public_key_hint: Option<String>,
}

impl SignerCandidate {
    /// Constructs a candidate from the two fields every candidate carries.
    ///
    /// The only constructor available outside this crate, because the struct
    /// is `#[non_exhaustive]` — that is deliberate (a later identity field
    /// must not break `ocx-mirror`, which takes `ocx_lib` as a path
    /// dependency), and it makes a downstream struct literal a compile error.
    /// The identity fields are attached with the `with_*` methods below rather
    /// than passed here, so adding one stays non-breaking on this signature
    /// too — and so three consecutive `Option<String>` arguments cannot be
    /// transposed silently.
    ///
    /// Chiefly a test seam: `ocx-mirror`'s `--identity` / `--issuer` filter is
    /// specified against candidate values, and obtaining one from a live
    /// registry and a Sigstore stack per filter case is not a test design.
    /// Nothing constructed here has been verified — see the type's own docs.
    ///
    /// # Examples
    ///
    /// ```
    /// use ocx_lib::oci::Algorithm;
    /// use ocx_lib::oci::verify::{DiscoveryMethod, SignerCandidate};
    ///
    /// let candidate = SignerCandidate::new(DiscoveryMethod::ReferrersApi, Algorithm::Sha256.hash(b"bundle"))
    ///     .with_certificate_identity("ocx-test@example.com")
    ///     .with_certificate_issuer("https://accounts.google.com");
    ///
    /// assert_eq!(candidate.certificate_identity.as_deref(), Some("ocx-test@example.com"));
    /// assert_eq!(candidate.public_key_hint, None);
    /// ```
    pub fn new(discovery: DiscoveryMethod, digest: oci::Digest) -> Self {
        Self {
            discovery,
            digest,
            artifact_type: None,
            certificate_identity: None,
            certificate_issuer: None,
            public_key_hint: None,
        }
    }

    /// Attaches the referrer's `artifactType`.
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

    /// Attaches the `verificationMaterial.publicKey.hint`.
    pub fn with_public_key_hint(mut self, hint: impl Into<String>) -> Self {
        self.public_key_hint = Some(hint.into());
        self
    }
}

/// Lists the signature candidates attached to `subject`, verifying none.
///
/// Discovery only: the Referrers API with the OCI tag-schema fallback
/// ([`OciTransport::list_referrers_with_fallback`]), filtered to the two
/// artifact types that name a *signature* ([`SIGSTORE_BUNDLE_V03`] and
/// [`COSIGN_SIG_ARTIFACT_TYPE`]), plus the cosign `sha256-<hex>.sig` sidecar
/// tag. No certificate chain is validated, no Rekor entry is fetched, and no
/// trust policy is consulted.
///
/// `.att` and `.sbom` are **not** candidates. An attestation and an SBOM are
/// routinely attached to subjects carrying no signature at all, so counting
/// one would report "already attested to" for an unsigned subject and let a
/// backfill skip exactly the artifact it exists to sign.
///
/// A DSSE **attestation referrer** is excluded for the same reason, and no
/// annotation can do it: `ocx package attest` writes [`SIGSTORE_BUNDLE_V03`]
/// on an attestation referrer too, and `dev.sigstore.bundle.content` reads
/// `dsse-envelope` on a signature as well — both fields are the producer's,
/// and both agree on the two kinds. The bundle's own in-toto Statement is the
/// only authoritative discriminator, so the bundle this function already
/// fetches for the identity fields is routed through
/// `dsse::is_cosign_image_signature`, the same router `VerifyPipeline` uses.
/// A referrer that is not an image signature is dropped **silently**: it is a
/// legitimate attestation, not a fault.
///
/// A referrer that declares no `artifactType` is likewise not a candidate:
/// `VerifyPipeline` reads an untyped bundle referrer permissively because it
/// then *verifies* it, and a sweep that cannot verify must not inherit that
/// permission — the safe direction for a backfill is a redundant re-sign, not
/// a skipped subject.
///
/// Identity fields are read out of the referrer's bundle blob with the same
/// non-verifying `parse_bundle` reader the verify pipeline parses with.
/// A `.sig` sidecar candidate leaves all three `None`: the certificate lives
/// in a per-*layer* annotation and one sidecar manifest carries many layers,
/// so there is no single identity to report without picking one arbitrarily.
///
/// At most `MAX_SIGNATURE_CANDIDATES` referrer candidates are listed, the
/// ceiling the verify pipeline already applies — a registry chooses how long
/// its own referrers listing is, and every entry past the ceiling costs a
/// manifest and a blob fetch. Truncation can only understate what is attached,
/// which for a backfill means a redundant re-sign rather than a skipped
/// subject; it is logged at debug rather than being silent.
///
/// # Errors
///
/// Whatever the transport raises. An absent signature is an **empty
/// vector**, never an error — "nothing found" and "could not look" stay
/// distinct, the same split [`OciTransport::list_referrers_with_fallback`]
/// already makes. Registry-supplied bytes that do not parse — a referrer
/// manifest, or a bundle that is not one — leave the identity fields `None`
/// and are not an error either: the candidate was still found. A descriptor
/// whose *digest* does not parse is dropped, for the same reason in the other
/// direction: there is nothing to address, and the registry still answered.
pub async fn list_signature_candidates(
    transport: &dyn OciTransport,
    image: &oci::native::Reference,
    subject: &oci::Digest,
) -> Result<Vec<SignerCandidate>, ClientError> {
    let listing = transport.list_referrers_with_fallback(image, subject, None).await?;
    let signature_referrers: Vec<oci::Descriptor> = listing
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
        // A descriptor whose digest is not a digest names nothing a caller
        // could fetch, so it is not a candidate — but the registry *did*
        // answer, so it is not an `Err` either. Aborting the whole subject
        // over one malformed sibling is the one outcome neither a backfill
        // nor OCX-C-2 wants.
        let Ok(digest) = oci::Digest::try_from(descriptor.digest.as_str()) else {
            continue;
        };
        // `None` is an attestation wearing the signature artifact type. Not an
        // error and not a candidate — skipped, and nothing is reported.
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
/// The two spellings the verify pipeline already routes as signatures: OCX's
/// and cosign's own bundle referrer, and cosign's OCI 1.1 simplesigning
/// referrer.
fn is_signature_artifact_type(artifact_type: Option<&str>) -> bool {
    artifact_type.is_some_and(|declared| declared == SIGSTORE_BUNDLE_V03 || declared == COSIGN_SIG_ARTIFACT_TYPE)
}

/// The `sha256-<hex>.sig` sidecar attached to `subject`, if the tag exists.
///
/// An absent tag is `Ok(None)` — "no sidecar", which is the overwhelmingly
/// common answer. Every other transport failure propagates: a registry that
/// could not answer must not read as "not signed".
async fn read_signature_sidecar(
    transport: &dyn OciTransport,
    image: &oci::native::Reference,
    subject: &oci::Digest,
) -> Result<Option<SignerCandidate>, ClientError> {
    let target = sibling_tag_reference(image, sidecar_tag(subject, SidecarKind::Signature));
    let digest = match transport.pull_manifest_raw(&target, ACCEPTED_MANIFEST_TYPES).await {
        Ok((_bytes, digest)) => digest,
        Err(ClientError::ManifestNotFound(_)) => return Ok(None),
        Err(other) => return Err(other),
    };
    Ok(Some(SignerCandidate {
        discovery: DiscoveryMethod::SidecarTag,
        // Not the registry's string: the transport computed this digest over
        // the bytes it just read, so a parse failure here is our bug, not
        // foreign input, and it propagates rather than being shrugged off.
        digest: oci::Digest::try_from(digest.as_str())
            .map_err(|error| ClientError::InvalidManifest(error.to_string()))?,
        // A sidecar is reached by tag, not by a referrers-index descriptor, so
        // there is no `artifactType` to report.
        artifact_type: None,
        certificate_identity: None,
        certificate_issuer: None,
        public_key_hint: None,
    }))
}

/// What a bundle claims about its signer, with none of it checked.
///
/// A named struct rather than a tuple of three `Option<String>`s, for the
/// reason `pipeline::VerifiedSigner` exists: two of the fields are the same
/// type and a swap would type-check.
#[derive(Default)]
struct BundleIdentity {
    certificate_identity: Option<String>,
    certificate_issuer: Option<String>,
    public_key_hint: Option<String>,
}

/// Reads `descriptor`'s referrer manifest and its bundle blob, and returns the
/// identity the bundle claims.
///
/// Every non-transport failure — an over-cap manifest, bytes that are not a
/// referrer manifest, a manifest with no layers, an over-cap or unparseable
/// bundle — yields an empty identity rather than an error. The candidate was
/// still discovered, and this function's contract is to describe it, not to
/// judge it.
///
/// `Ok(None)` is the one non-error rejection: the bundle parsed and its DSSE
/// Statement is an attestation, so this referrer is not a signature candidate
/// at all and the caller drops it.
async fn read_referrer_identity(
    transport: &dyn OciTransport,
    image: &oci::native::Reference,
    descriptor: &oci::Descriptor,
) -> Result<Option<BundleIdentity>, ClientError> {
    let referrer_ref = image.clone_with_digest(descriptor.digest.clone());
    let (manifest_bytes, _) = transport
        .pull_manifest_raw(&referrer_ref, ACCEPTED_MANIFEST_TYPES)
        .await?;
    if manifest_bytes.len() as u64 > MAX_REFERRER_MANIFEST_BYTES {
        return Ok(Some(BundleIdentity::default()));
    }
    let Ok(manifest) = serde_json::from_slice::<crate::oci::referrer::ReferrerManifest>(&manifest_bytes) else {
        return Ok(Some(BundleIdentity::default()));
    };
    let Some(layer) = manifest.layers.first() else {
        return Ok(Some(BundleIdentity::default()));
    };
    // The declared size is untrusted, so it is only a cheap pre-fetch reject;
    // `pull_bundle_capped` bounds the read itself (CWE-400). `try_from` rather
    // than a `< 0` guard plus `as usize`: it refuses a negative and a value
    // past `usize::MAX` in one step, on a 32-bit target too.
    if !usize::try_from(layer.size).is_ok_and(|declared| declared <= MAX_BUNDLE_SIZE_BYTES) {
        return Ok(Some(BundleIdentity::default()));
    }
    let Ok(blob_digest) = oci::Digest::try_from(layer.digest.as_str()) else {
        return Ok(Some(BundleIdentity::default()));
    };
    let Some(bundle_bytes) = pull_bundle_capped(transport, image, &blob_digest).await? else {
        return Ok(Some(BundleIdentity::default()));
    };
    Ok(read_bundle_identity(&bundle_bytes))
}

/// Reads a bundle blob under [`MAX_BUNDLE_SIZE_BYTES`], returning `None` when
/// the registry served more than that.
///
/// Reads at most `cap + 1` bytes so an over-cap body is detected without
/// buffering the whole thing — the descriptor pre-check bounds the honest
/// case, this bounds a registry that lied about the size.
async fn pull_bundle_capped(
    transport: &dyn OciTransport,
    image: &oci::native::Reference,
    blob_digest: &oci::Digest,
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

/// Splits a parsed bundle's verification material into the identity fields,
/// **validating none of them** — or answers `None`, meaning "this is an
/// attestation, not a signature candidate".
///
/// The attestation exclusion lives here because this is where the bundle is
/// already parsed: an attestation referrer carries the same
/// [`SIGSTORE_BUNDLE_V03`] artifactType as a signature, so the discriminator
/// is the DSSE Statement's `predicateType`, read through the pipeline's own
/// `dsse::is_cosign_image_signature` router rather than through the
/// producer-controlled annotations. A bundle that does not parse at all keeps
/// its candidate: discovery describes, it does not judge, and the safe
/// direction for a backfill is a redundant re-sign rather than a skipped
/// subject.
///
/// Keyless material yields the leaf certificate's SubjectAltName and Fulcio
/// OIDC-issuer extension, read with the pipeline's own extractors; key-mode
/// material yields `publicKey.hint`. The two are mutually exclusive by the
/// protobuf's own oneof, which is why a bundle never populates both.
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
