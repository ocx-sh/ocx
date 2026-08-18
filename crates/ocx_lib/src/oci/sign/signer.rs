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

        // 2. Proof of possession over the identity Fulcio will put in the SAN.
        //
        //    Not `unwrap_or_default()`: an empty subject produces a signature
        //    Fulcio cannot verify, and it answered 400 with a message this side
        //    discarded. A token carrying no usable identity claim is a rejected
        //    token, and saying so costs one round trip less to diagnose.
        let subject = jwt_subject(token.as_str()).ok_or(SignErrorKind::OidcTokenRejected)?;
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

/// Extract the identity claim a Fulcio proof-of-possession must be signed over.
///
/// **`email` first, `sub` second, and the order is the contract.** Fulcio signs
/// over its *principal name*, which for an email-type issuer is the `email`
/// claim, not `sub` — dex's `sub` is an opaque base64 blob, and signing it
/// yields "The signature supplied in the request could not be verified". `sub`
/// remains the fallback for issuers with no email: a GitHub Actions token's
/// principal name genuinely is its `sub` (`repo:owner/name:ref`).
///
/// Returns `None` on any structural failure; the caller treats that as a
/// rejected token rather than signing an empty string.
fn jwt_subject(jwt: &str) -> Option<String> {
    use base64::Engine as _;
    let payload_b64 = jwt.split('.').nth(1)?;
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload_b64)
        .ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&payload).ok()?;
    // Emptiness is tested per claim, before the fallback, not once after it.
    // A trailing `.filter(non-empty)` let a present-but-empty `email` win the
    // `or_else` and then be discarded, so a token carrying `"email": ""`
    // alongside a perfectly signable `sub` came back rejected — exit 80 with
    // "refresh the token", for a token that needed nothing. "Issuers with no
    // email" is what the paragraph above promises, and an empty claim is one.
    let claim = |name: &str| claims.get(name).and_then(|v| v.as_str()).filter(|s| !s.is_empty());
    claim("email").or_else(|| claim("sub")).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    //! `jwt_subject` decides what a Fulcio proof-of-possession is signed over,
    //! and getting it wrong costs a round trip that fails with "The signature
    //! supplied in the request could not be verified" — a message that names
    //! neither the claim nor the order. Separate named tests rather than
    //! `#[rstest] #[case(...)]` rows because `rstest` is not a workspace
    //! dependency; a `for` loop would abort at the first failure under one
    //! opaque name (TEST-04).

    use super::*;

    /// A JWT with the given `header.payload.signature` shape; only the payload
    /// is ever decoded, so the other two segments are placeholders.
    fn jwt(payload_b64: &str) -> String {
        format!("header.{payload_b64}.signature")
    }

    #[test]
    fn email_wins_over_sub() {
        // The contract, and the reason the function exists: dex's `sub` is an
        // opaque base64 blob, and Fulcio signs over its principal name, which
        // for an email-type issuer is `email`.
        assert_eq!(
            jwt_subject(EMAIL_AND_SUB).as_deref(),
            Some("me@example.com"),
            "an email-bearing token must sign over the email, not the opaque sub"
        );
    }

    #[test]
    fn sub_is_the_fallback_when_there_is_no_email() {
        // A GitHub Actions token's principal name genuinely is its `sub`.
        assert_eq!(
            jwt_subject(SUB_ONLY).as_deref(),
            Some("repo:owner/name:ref:refs/heads/main")
        );
    }

    #[test]
    fn an_empty_email_does_not_shadow_a_usable_sub() {
        // `.filter(non-empty)` sits after `.or_else`, so an empty `email`
        // would otherwise win the `or_else` and then be discarded, rejecting a
        // token whose `sub` was perfectly usable.
        assert_eq!(jwt_subject(EMAIL_EMPTY).as_deref(), Some("repo:owner/name:ref"));
    }

    #[test]
    fn a_token_with_only_empty_claims_is_rejected() {
        // Not `unwrap_or_default()`: signing an empty subject produces a
        // signature Fulcio cannot verify and costs a round trip to diagnose.
        assert_eq!(jwt_subject(BOTH_EMPTY), None);
    }

    #[test]
    fn a_token_carrying_neither_claim_is_rejected() {
        assert_eq!(jwt_subject(NO_CLAIMS), None);
    }

    #[test]
    fn a_non_string_claim_is_rejected() {
        assert_eq!(jwt_subject(SUB_NOT_STRING), None);
    }

    #[test]
    fn a_payload_that_is_not_base64url_is_rejected() {
        assert_eq!(jwt_subject(&jwt("not base64!!")), None);
    }

    #[test]
    fn a_payload_that_is_not_json_is_rejected() {
        // Base64 that decodes cleanly to something that is not an object.
        assert_eq!(jwt_subject(&jwt("bm90LWpzb24")), None);
    }

    #[test]
    fn a_string_with_no_second_segment_is_rejected() {
        assert_eq!(jwt_subject("not-a-jwt"), None);
    }

    // Fixtures: `header.<base64url payload>.signature`.
    const EMAIL_AND_SUB: &str =
        "header.eyJlbWFpbCI6Im1lQGV4YW1wbGUuY29tIiwic3ViIjoiQ2djeE1qTTBOVFkzRWdSc1pHRncifQ.signature";
    const SUB_ONLY: &str = "header.eyJzdWIiOiJyZXBvOm93bmVyL25hbWU6cmVmOnJlZnMvaGVhZHMvbWFpbiJ9.signature";
    const EMAIL_EMPTY: &str = "header.eyJlbWFpbCI6IiIsInN1YiI6InJlcG86b3duZXIvbmFtZTpyZWYifQ.signature";
    const BOTH_EMPTY: &str = "header.eyJlbWFpbCI6IiIsInN1YiI6IiJ9.signature";
    const NO_CLAIMS: &str = "header.eyJhdWQiOiJzaWdzdG9yZSJ9.signature";
    const SUB_NOT_STRING: &str = "header.eyJzdWIiOjQyfQ.signature";
}
