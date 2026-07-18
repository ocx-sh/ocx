// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! [`Signer`] trait — the cryptographic half of keyless signing.
//!
//! A signer turns a target digest + an acquired OIDC token into a Sigstore
//! bundle v0.3. The registry push is a separate concern owned by
//! [`pipeline::SignPipeline`](super::pipeline). This split (Architect F2) lets
//! v2 add HSM/KMS signers without touching the push state machine, and lets
//! tests inject a fake signer.

use async_trait::async_trait;
use p256::ecdsa::SigningKey;
use p256::ecdsa::signature::hazmat::PrehashSigner;
use p256::elliptic_curve::rand_core::OsRng;
use p256::pkcs8::{EncodePublicKey, LineEnding};
use url::Url;

use super::bundle::{SignedBundle, build_bundle};
use super::error::SignErrorKind;
use super::fulcio::FulcioClient;
use super::oidc::OidcToken;
use super::rekor::RekorClient;
use crate::oci::Digest;

/// Produces a Sigstore bundle for a target digest.
///
/// The keyless v1 implementation is [`KeylessSigner`]; the trait exists so v2
/// signers (KMS, private CA) reuse the same push pipeline.
#[async_trait]
pub trait Signer: Send + Sync {
    /// Sign `target_digest` with the identity in `token`, returning a bundle.
    ///
    /// `fulcio_url` / `rekor_url` are the validated endpoints (the SSRF guard
    /// runs at the CLI boundary). Returns the leaf error kind; the pipeline
    /// composes it into a [`SignError`](super::SignError) with the identifier.
    async fn sign(
        &self,
        target_digest: &Digest,
        token: &OidcToken,
        fulcio_url: &Url,
        rekor_url: &Url,
    ) -> Result<SignedBundle, SignErrorKind>;

    /// Static string identifying this signer kind (e.g. `keyless-fulcio`).
    fn signer_kind(&self) -> &'static str;
}

/// Keyless signer: ephemeral P-256 key → Fulcio cert → Rekor entry → bundle.
pub struct KeylessSigner;

impl KeylessSigner {
    /// Construct a keyless signer.
    pub fn new() -> Self {
        Self
    }
}

impl Default for KeylessSigner {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Signer for KeylessSigner {
    async fn sign(
        &self,
        target_digest: &Digest,
        token: &OidcToken,
        fulcio_url: &Url,
        rekor_url: &Url,
    ) -> Result<SignedBundle, SignErrorKind> {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD;

        // 1. Ephemeral P-256 keypair.
        let signing_key = SigningKey::random(&mut OsRng);
        let public_key_pem = signing_key
            .verifying_key()
            .to_public_key_pem(LineEnding::LF)
            .map_err(|e| SignErrorKind::Internal(Box::new(e)))?;

        // 2. Proof of possession over the token subject (public-good Fulcio
        //    validates it; the offline fake ignores it).
        let subject = jwt_subject(token.as_str()).unwrap_or_default();
        let pop_sig: p256::ecdsa::Signature = signing_key
            .sign_prehash(&sha256(subject.as_bytes()))
            .map_err(|e| SignErrorKind::Internal(Box::new(e)))?;
        let pop_b64 = b64.encode(pop_sig.to_der().as_bytes());

        // 3. Fulcio: exchange OIDC token + pubkey for a signing certificate.
        let cert = FulcioClient::new(fulcio_url.clone())
            .request_certificate(token.as_str(), &public_key_pem, &pop_b64)
            .await?;

        // 4. Sign the subject digest (the raw sha256 bytes of the target
        //    manifest) with the ephemeral key.
        let subject_raw = hex::decode(target_digest.hex()).map_err(|e| SignErrorKind::Internal(Box::new(e)))?;
        let signature: p256::ecdsa::Signature = signing_key
            .sign_prehash(&subject_raw)
            .map_err(|e| SignErrorKind::Internal(Box::new(e)))?;
        let signature_der = signature.to_der().as_bytes().to_vec();

        // 5. Rekor: upload the hashedrekord entry, obtain the SET.
        let rekor = RekorClient::new(rekor_url.clone())
            .upload_entry(&signature_der, &cert.leaf_pem, target_digest.hex())
            .await?;

        // 6. Assemble the Sigstore bundle v0.3.
        build_bundle(&cert, &signature_der, &rekor, target_digest)
    }

    fn signer_kind(&self) -> &'static str {
        "keyless-fulcio"
    }
}

/// SHA-256 of `bytes` as a 32-byte array.
fn sha256(bytes: &[u8]) -> Vec<u8> {
    use sha2::Digest as _;
    sha2::Sha256::digest(bytes).to_vec()
}

/// Extract the `sub` (or `email`) claim from a JWT without verifying it.
///
/// Used only to build the Fulcio proof-of-possession message. Returns `None`
/// on any structural failure — the caller falls back to an empty subject.
fn jwt_subject(jwt: &str) -> Option<String> {
    use base64::Engine as _;
    let payload_b64 = jwt.split('.').nth(1)?;
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload_b64)
        .ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&payload).ok()?;
    claims
        .get("sub")
        .or_else(|| claims.get("email"))
        .and_then(|v| v.as_str())
        .map(str::to_owned)
}
