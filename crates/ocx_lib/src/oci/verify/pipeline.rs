// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Verify pipeline — full keyless Sigstore verification state machine.
//!
//! Per
//! [`adr_oci_referrers_signing_v1.md`](../../../../../.claude/artifacts/adr_oci_referrers_signing_v1.md)
//! S1-H: resolve target → list referrers (capability cache) → pick the v0.3
//! bundle → parse → verify the Fulcio cert chain against the trust root →
//! bind the message signature to the subject digest → verify the ECDSA
//! signature → verify the Rekor SET → match identity + issuer → emit
//! [`VerifyResult`].
//!
//! The trust root (Fulcio CA) is injected via [`VerifyContext::trust_root`]
//! (C-S1-3); the Rekor public key used for SET verification is fetched from
//! [`VerifyContext::rekor_url`] `/api/v1/log/publicKey`.

use std::path::Path;

use p256::ecdsa::signature::{Verifier, hazmat::PrehashVerifier};
use url::Url;

use super::error::{VerifyError, VerifyErrorKind};
use super::identity::{oidc_issuer, parse_certificate, subject_identity, verify_policies};
use super::trust_root::TrustRoot;
use crate::oci::client::OciTransport;
use crate::oci::client::error::ClientError;
use crate::oci::index::{Index, IndexOperation, SelectResult};
use crate::oci::referrer::capability::{ReferrersApiCapability, ReferrersSupport};
use crate::oci::referrer::media_types::SIGSTORE_BUNDLE_V03;
use crate::oci::sign::bundle::parse_bundle;
use crate::oci::sign::rekor::set_signing_payload;
use crate::oci::{Digest, Identifier, Platform, native};
use sigstore_protobuf_specs::dev::sigstore::bundle::v1::{bundle, verification_material};

const ACCEPTED_MANIFEST_TYPES: &[&str] = &[
    crate::oci::OCI_IMAGE_MEDIA_TYPE,
    "application/vnd.docker.distribution.manifest.v2+json",
];

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
    /// Registry transport.
    pub transport: &'a dyn OciTransport,
    /// Index for resolving tag → digest.
    pub index: &'a Index,
    /// Trust root (Fulcio CA certs); C-S1-3 injection seam.
    pub trust_root: &'a TrustRoot,
    /// Rekor URL (C-S1-3 injection seam). Default: `https://rekor.sigstore.dev`.
    pub rekor_url: &'a Url,
    /// `$OCX_HOME` root for the referrers-capability cache.
    pub cache_root: &'a Path,
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
    pub async fn run(ctx: VerifyContext<'_>) -> Result<VerifyResult, VerifyError> {
        let identifier = ctx.identifier.clone();
        Self::run_inner(ctx)
            .await
            .map_err(|kind| VerifyError::new(identifier, kind))
    }

    async fn run_inner(ctx: VerifyContext<'_>) -> Result<VerifyResult, VerifyErrorKind> {
        // 1. Resolve the per-platform target manifest.
        let resolved = match ctx
            .index
            .select(ctx.identifier, vec![ctx.platform.clone()], IndexOperation::Resolve)
            .await
            .map_err(|e| VerifyErrorKind::Internal(Box::new(e)))?
        {
            SelectResult::Found(id) => id,
            SelectResult::Ambiguous(_) | SelectResult::NotFound => return Err(VerifyErrorKind::NoSignaturesFound),
        };
        let subject_digest = resolved.digest().ok_or(VerifyErrorKind::NoSignaturesFound)?;
        let registry = resolved.registry().to_string();
        let repo = resolved.repository().to_string();
        let image = native::Reference::with_tag(registry.clone(), repo.clone(), "latest".to_string());

        // 2. List signature referrers (capability cache short-circuits a known
        //    Unsupported registry without re-listing).
        let referrers = Self::list_signature_referrers(&ctx, &image, &registry, &repo, &subject_digest).await?;
        let referrer_descriptor = referrers.into_iter().next().ok_or(VerifyErrorKind::NoSignaturesFound)?;
        let referrer_digest = Digest::try_from(referrer_descriptor.digest.as_str())
            .map_err(|e| VerifyErrorKind::Internal(Box::new(e)))?;

        // 3. Fetch the referrer manifest → its bundle-blob layer → the blob.
        let referrer_ref =
            native::Reference::with_digest(registry.clone(), repo.clone(), referrer_descriptor.digest.clone());
        let (referrer_bytes, _) = ctx
            .transport
            .pull_manifest_raw(&referrer_ref, ACCEPTED_MANIFEST_TYPES)
            .await
            .map_err(map_client_error)?;
        let referrer_manifest: crate::oci::referrer::ReferrerManifest =
            serde_json::from_slice(&referrer_bytes).map_err(|_| VerifyErrorKind::BundleParseFailed)?;
        let bundle_layer = referrer_manifest
            .layers
            .first()
            .ok_or(VerifyErrorKind::NoUsableBundle)?;
        let bundle_blob_digest =
            Digest::try_from(bundle_layer.digest.as_str()).map_err(|_| VerifyErrorKind::BundleParseFailed)?;
        let bundle_bytes = ctx
            .transport
            .pull_blob(&image, &bundle_blob_digest)
            .await
            .map_err(map_client_error)?;

        // 4. Parse the bundle.
        let bundle = parse_bundle(&bundle_bytes).ok_or(VerifyErrorKind::BundleParseFailed)?;
        let parts = BundleParts::from_bundle(&bundle)?;

        // 5. Verify the Fulcio cert chain against the trust root.
        verify_cert_chain(&parts.leaf_der, ctx.trust_root)?;

        // 6. Bind the message signature to the subject digest (GHSA-whqx class).
        let subject_raw = hex::decode(subject_digest.hex()).map_err(|_| VerifyErrorKind::BundleParseFailed)?;
        if parts.message_digest != subject_raw {
            return Err(VerifyErrorKind::SignatureInvalid);
        }

        // 7. Verify the ECDSA signature over the subject digest with the leaf key.
        verify_signature(&parts.leaf_der, &subject_raw, &parts.signature)?;

        // 8. Verify the Rekor SET against the log's published public key.
        verify_rekor_set(&ctx, &parts).await?;

        // 9. Identity + issuer match against the resolved trust policies (ANY-of).
        verify_policies(&parts.leaf_der, ctx.policies)?;

        // 10. Emit the result (identity/issuer read back from the cert).
        let cert = parse_certificate(&parts.leaf_der)?;
        Ok(VerifyResult {
            subject_digest,
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
        ctx: &VerifyContext<'_>,
        image: &native::Reference,
        registry: &str,
        repo: &str,
        subject_digest: &Digest,
    ) -> Result<Vec<crate::oci::Descriptor>, VerifyErrorKind> {
        // Capability: a fresh cache entry avoids a re-probe; otherwise probe
        // and persist. `Unsupported` fails hard (no fallback-tag reads, S1-F).
        let cached = if ctx.no_cache {
            None
        } else {
            ReferrersApiCapability::from_cache(registry, ctx.cache_root)
                .await
                .ok()
                .flatten()
                .filter(ReferrersApiCapability::is_fresh)
        };
        let capability = match cached {
            Some(hit) => hit,
            None => {
                let probed = ReferrersApiCapability::probe(ctx.transport, registry, repo, subject_digest)
                    .await
                    .map_err(map_client_error)?;
                let _ = probed.write_cache(ctx.cache_root).await;
                probed
            }
        };
        if capability.supported == ReferrersSupport::Unsupported {
            return Err(VerifyErrorKind::ReferrersUnsupported);
        }

        // Fetch the signature referrers (server-side artifactType filter).
        ctx.transport
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

/// Fetch the Rekor log's public key and verify the SET over the entry payload.
async fn verify_rekor_set(ctx: &VerifyContext<'_>, parts: &BundleParts) -> Result<(), VerifyErrorKind> {
    use ed25519_dalek::pkcs8::DecodePublicKey;

    let endpoint = ctx
        .rekor_url
        .join("api/v1/log/publicKey")
        .map_err(|e| VerifyErrorKind::Internal(Box::new(e)))?;
    let response = reqwest::Client::new()
        .get(endpoint)
        .send()
        .await
        .map_err(|_| VerifyErrorKind::RekorUnavailable)?;
    if !response.status().is_success() {
        return Err(VerifyErrorKind::RekorUnavailable);
    }
    let pem = response.text().await.map_err(|_| VerifyErrorKind::RekorUnavailable)?;
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
        .map_err(|_| VerifyErrorKind::RekorSetInvalid)
}

/// Map an OCI client error into the verify taxonomy.
fn map_client_error(error: ClientError) -> VerifyErrorKind {
    match error {
        ClientError::ReferrersUnsupported { .. } => VerifyErrorKind::ReferrersUnsupported,
        ClientError::ManifestNotFound(_) | ClientError::BlobNotFound { .. } => VerifyErrorKind::NoSignaturesFound,
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
}
