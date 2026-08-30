// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! [`Signer`] trait — the cryptographic half of keyless signing.
//!
//! A signer turns an in-toto Statement + an acquired OIDC token into a
//! Sigstore bundle v0.3. The registry push is a separate concern owned by
//! [`pipeline::SignPipeline`](super::pipeline). This split (Architect F2) lets
//! v2 add HSM/KMS signers without touching the push state machine, and lets
//! tests inject a fake signer.

use async_trait::async_trait;
use p256::ecdsa::SigningKey;
use p256::ecdsa::signature::hazmat::PrehashSigner;
use p256::elliptic_curve::rand_core::OsRng;
use p256::pkcs8::{EncodePublicKey, LineEnding};
use url::Url;

use super::bundle::{SignedBundle, SignedEnvelope, SigningMaterial, build_dsse_bundle};
use super::error::SignErrorKind;
use super::fulcio::{FulcioCertificate, FulcioClient};
use super::oidc::OidcToken;
use super::rekor::RekorClient;
use crate::oci::attest::dsse::{DsseEnvelope, DsseSignature, pae};
use crate::oci::attest::{DSSE_PAYLOAD_TYPE, MAX_STATEMENT_PAYLOAD_BYTES};

/// A signature over opaque bytes, plus the verification material a cosign
/// simplesigning sidecar puts in layer annotations.
///
/// Every field but `signature` is optional because their absence is a **legal
/// shape**, not malformed output (spec D5): under a key there is no certificate
/// and no chain, and with no transparency-log upload there is no offline Rekor
/// bundle. A reader must not treat any of them as required.
#[derive(Debug, Clone)]
pub struct SignedBlob {
    /// DER-encoded ECDSA signature over `sha256(payload)`.
    pub signature: Vec<u8>,
    /// PEM leaf certificate. Keyless only.
    pub certificate_pem: Option<String>,
    /// PEM intermediate chain.
    ///
    /// Always `None` today, and deliberately: bundle v0.3 replaced the chain
    /// field with a single leaf, Fulcio's intermediates come from the trust
    /// root, and cosign v3.1.1's own `.sig` manifests carry no `/chain`
    /// annotation either (`test/tests/fixtures/golden/simplesigning_keyless_manifest.json`).
    /// Modelled rather than dropped so a private-CA signer with real
    /// intermediates has somewhere to put them.
    pub chain_pem: Option<String>,
    /// The offline Rekor bundle for the `dev.sigstore.cosign/bundle`
    /// annotation, when an entry was created.
    pub rekor_bundle: Option<String>,
    /// The Rekor log index, when a transparency record was created.
    pub transparency_log_index: Option<u64>,
    /// Which key model produced the signature.
    pub key_backend: crate::oci::sign::KeyBackendKind,
    /// The signing key's cosign hint, in key mode only.
    pub public_key_hint: Option<String>,
}

/// Produces a Sigstore bundle over an in-toto Statement.
///
/// The keyless v1 implementation is [`KeylessSigner`]; the trait exists so v2
/// signers (KMS, private CA) reuse the same push pipeline.
#[async_trait]
pub trait Signer: Send + Sync {
    /// Sign an in-toto Statement as a DSSE envelope, returning a bundle.
    ///
    /// The payload type is fixed (`DSSE_PAYLOAD_TYPE`): v1 writes exactly one,
    /// so it is a constant rather than a stringly-typed parameter (ARCH-05).
    /// What is signed is `sha256(PAE(payload_type, statement_bytes))` — never
    /// the statement bytes alone and never the base64 text — the Rekor entry is
    /// `dsse:0.0.1`, and the returned bundle's content oneof is `dsseEnvelope`.
    ///
    /// **Preconditions.** `statement_bytes` is bounded here, against
    /// `MAX_STATEMENT_PAYLOAD_BYTES` — the same ceiling the read side applies
    /// in [`DsseEnvelope::parse`](crate::oci::attest::dsse::DsseEnvelope::parse)
    /// — and the refusal comes *before* any network contact. A Rekor entry is
    /// permanent, so an over-cap statement that reached the log would be
    /// published forever and then refused by this tool's own verifier.
    ///
    /// A signer that only wants message signatures still supplies this; that
    /// cost was taken deliberately over generalizing [`Self::sign`].
    async fn sign_dsse(
        &self,
        statement_bytes: &[u8],
        token: Option<&OidcToken>,
        fulcio_url: &Url,
        rekor_url: &Url,
    ) -> Result<SignedBundle, SignErrorKind>;

    /// Sign `payload` verbatim, returning the signature and the verification
    /// material a cosign simplesigning sidecar carries in annotations.
    ///
    /// **The bytes are the message.** Unlike [`Self::sign_dsse`], which signs
    /// `sha256(PAE(type, payload))`, this signs `sha256(payload)` — cosign's
    /// simplesigning claim is signed as an opaque blob and its Rekor entry is a
    /// `hashedrekord` over the same digest. A signature produced by one rule
    /// does not verify under the other, which is why this is a separate method
    /// rather than a flag on `sign_dsse`.
    ///
    /// The caller owns the payload's meaning; this signer only signs it.
    async fn sign_blob(
        &self,
        payload: &[u8],
        token: Option<&OidcToken>,
        fulcio_url: &Url,
        rekor_url: &Url,
    ) -> Result<SignedBlob, SignErrorKind>;

    /// Whether this signer needs an OIDC identity token.
    ///
    /// `true` for keyless, `false` under a key pair. The pipeline reads it to
    /// decide whether to acquire a token at all — a key-mode run in an
    /// air-gapped org has no issuer to ask, and spending an ambient token there
    /// would fail a signature that needs no identity. The same answer gates the
    /// Fulcio SSRF pre-flight: a URL that is never dialled must not have to
    /// resolve.
    ///
    /// **Contract:** `sign_dsse` receives `Some` exactly when this is `true`. A
    /// signer that answers `true` and is handed `None` must fail rather than
    /// sign, which is what keeps the pair honest.
    fn requires_identity_token(&self) -> bool {
        true
    }

    /// Whether this signer will upload a transparency-log entry.
    ///
    /// Keyless always does — the Rekor timestamp is the only durable proof the
    /// signature happened inside its ten-minute certificate window — so the
    /// default is `true`. Key mode answers whether `--rekor-upload` (or
    /// `[trust.sigstore] rekor_upload`) opted in.
    ///
    /// The pipeline reads it to decide whether the Rekor SSRF pre-flight runs:
    /// an endpoint that is never dialled must not have to resolve, or an
    /// air-gapped key-mode sign fails on DNS for a host it never contacts.
    fn uploads_to_transparency_log(&self) -> bool {
        true
    }

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
    async fn sign_blob(
        &self,
        payload: &[u8],
        token: Option<&OidcToken>,
        fulcio_url: &Url,
        rekor_url: &Url,
    ) -> Result<SignedBlob, SignErrorKind> {
        let token = token.ok_or(SignErrorKind::OidcTokenRejected)?;
        let identity = issue_ephemeral_certificate(token, fulcio_url).await?;

        // `sha256(payload)`, not the PAE: cosign signs a simplesigning claim as
        // an opaque blob and logs a `hashedrekord` over the same digest.
        let payload_digest = sha256(payload);
        let signature: p256::ecdsa::Signature = identity
            .signing_key
            .sign_prehash(&payload_digest)
            .map_err(|e| SignErrorKind::Internal(Box::new(e)))?;
        let signature_der = signature.to_der().as_bytes().to_vec();

        let entry = RekorClient::new(rekor_url.clone())
            .upload_entry(
                &signature_der,
                &identity.certificate.leaf_pem,
                &hex::encode(&payload_digest),
            )
            .await?;

        Ok(SignedBlob {
            signature: signature_der,
            certificate_pem: Some(identity.certificate.leaf_pem),
            chain_pem: None,
            transparency_log_index: Some(entry.log_index),
            rekor_bundle: Some(super::simplesigning_write::offline_bundle(&entry)?),
            key_backend: crate::oci::sign::KeyBackendKind::Keyless,
            public_key_hint: None,
        })
    }

    async fn sign_dsse(
        &self,
        statement_bytes: &[u8],
        token: Option<&OidcToken>,
        fulcio_url: &Url,
        rekor_url: &Url,
    ) -> Result<SignedBundle, SignErrorKind> {
        // The trait's contract says `Some` whenever `requires_identity_token`
        // is true, and this signer never answers false. A `None` here is a
        // pipeline bug, and a keyless signature without an identity is exactly
        // what must not be produced — so it refuses rather than improvising.
        let token = token.ok_or(SignErrorKind::OidcTokenRejected)?;
        // First, before the Fulcio round trip and long before the irreversible
        // Rekor write: a statement over the verifier's ceiling would be signed,
        // published to a permanent log, and then refused by this tool's own
        // verify path. Cheapest possible refusal, at the only point it is
        // still free.
        if statement_bytes.len() > MAX_STATEMENT_PAYLOAD_BYTES {
            return Err(SignErrorKind::PredicateTooLarge {
                limit: MAX_STATEMENT_PAYLOAD_BYTES as u64,
                actual: statement_bytes.len() as u64,
            });
        }

        // Same first step as `sign`; the two then diverge exactly where the
        // protocols do — what is signed, what is logged, what the bundle holds.
        let identity = issue_ephemeral_certificate(token, fulcio_url).await?;
        let signed = SignedEnvelope::new(sign_envelope(&identity.signing_key, statement_bytes)?)?;

        let rekor = RekorClient::new(rekor_url.clone())
            .upload_dsse_entry(signed.json(), &identity.certificate.leaf_pem)
            .await?;

        build_dsse_bundle(
            SigningMaterial::Certificate(&identity.certificate),
            &signed,
            Some(&rekor),
        )
    }

    fn signer_kind(&self) -> &'static str {
        "keyless-fulcio"
    }
}

/// An ephemeral signing key and the Fulcio certificate issued for it.
struct EphemeralIdentity {
    signing_key: SigningKey,
    certificate: FulcioCertificate,
}

/// Mint an ephemeral P-256 keypair and exchange `token` for a Fulcio
/// certificate over it.
///
/// The half [`Signer::sign`] and [`Signer::sign_dsse`] share. Which bytes get
/// signed afterwards is the only thing that differs between them, so keeping
/// this in one place is what stops the two paths drifting into obtaining
/// certificates differently.
///
/// A free function rather than a method: `KeylessSigner` is a unit struct and
/// there is no receiver state to reach.
async fn issue_ephemeral_certificate(token: &OidcToken, fulcio_url: &Url) -> Result<EphemeralIdentity, SignErrorKind> {
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD;

    let signing_key = SigningKey::random(&mut OsRng);
    let public_key_pem = signing_key
        .verifying_key()
        .to_public_key_pem(LineEnding::LF)
        .map_err(|e| SignErrorKind::Internal(Box::new(e)))?;

    // Proof of possession over the identity Fulcio will put in the SAN.
    //
    // Not `unwrap_or_default()`: an empty subject produces a signature Fulcio
    // cannot verify, and it answered 400 with a message this side discarded. A
    // token carrying no usable identity claim is a rejected token, and saying
    // so costs one round trip less to diagnose.
    let subject = jwt_subject(token.as_str()).ok_or(SignErrorKind::OidcTokenRejected)?;
    let pop_sig: p256::ecdsa::Signature = signing_key
        .sign_prehash(&sha256(subject.as_bytes()))
        .map_err(|e| SignErrorKind::Internal(Box::new(e)))?;
    let pop_b64 = b64.encode(pop_sig.to_der().as_bytes());

    let certificate = FulcioClient::new(fulcio_url.clone())
        .request_certificate(token.as_str(), &public_key_pem, &pop_b64)
        .await?;

    Ok(EphemeralIdentity {
        signing_key,
        certificate,
    })
}

/// Build the DSSE envelope for `statement_bytes`, signed with `signing_key`.
///
/// **What gets signed is `sha256(PAE(payload_type, statement_bytes))`.** Not the
/// statement bytes, not their digest, and not the base64 text of either — the
/// PAE is what binds the payload to its declared type, and dropping it makes a
/// signature over a CycloneDX SBOM equally valid over the same bytes claimed as
/// SLSA provenance.
///
/// Pure: no network, no clock. The Fulcio and Rekor halves are the caller's.
fn sign_envelope(signing_key: &SigningKey, statement_bytes: &[u8]) -> Result<DsseEnvelope, SignErrorKind> {
    let signature: p256::ecdsa::Signature = signing_key
        .sign_prehash(&sha256(&pae(DSSE_PAYLOAD_TYPE, statement_bytes)))
        .map_err(|e| SignErrorKind::Internal(Box::new(e)))?;

    Ok(DsseEnvelope {
        payload: statement_bytes.to_vec(),
        payload_type: DSSE_PAYLOAD_TYPE.to_string(),
        signatures: vec![DsseSignature {
            sig: signature.to_der().as_bytes().to_vec(),
            // Empty by design: cosign omits it on a keyless signature, and it
            // is a lookup hint that verification never reads.
            keyid: String::new(),
        }],
    })
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
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;

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

    // ---- DSSE envelope signing ------------------------------------------

    /// A fixed non-zero scalar, so the signature is reproducible across runs
    /// and a failure names a real disagreement rather than a fresh key.
    fn fixed_key() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32].into()).expect("a fixed non-zero scalar is a valid P-256 key")
    }

    const STATEMENT: &[u8] = br#"{"_type":"https://in-toto.io/Statement/v1"}"#;

    #[test]
    fn sign_envelope_signs_the_pae_and_nothing_else() {
        // The property the whole envelope rests on, and the one a plausible
        // wrong implementation satisfies structurally: signing the statement
        // bytes (or their digest) produces an envelope that parses, carries a
        // real ECDSA signature, and binds the payload to no declared type at
        // all. Only verifying against the PAE distinguishes the two.
        use p256::ecdsa::signature::hazmat::PrehashVerifier as _;

        let key = fixed_key();
        let envelope = sign_envelope(&key, STATEMENT).expect("signing a well-formed statement succeeds");
        let signature = p256::ecdsa::Signature::from_der(&envelope.signatures[0].sig)
            .expect("the envelope carries a DER-encoded ECDSA signature");
        let verifying = key.verifying_key();

        assert!(
            verifying
                .verify_prehash(&sha256(&pae(DSSE_PAYLOAD_TYPE, STATEMENT)), &signature)
                .is_ok(),
            "the signature must verify against sha256(PAE(payload_type, statement))"
        );
        assert!(
            verifying.verify_prehash(&sha256(STATEMENT), &signature).is_err(),
            "a signature over the bare statement bytes would not bind the payload type"
        );
        assert!(
            verifying
                .verify_prehash(
                    &sha256(&pae(DSSE_PAYLOAD_TYPE, BASE64_STANDARD.encode(STATEMENT).as_bytes())),
                    &signature
                )
                .is_err(),
            "checklist row 1: the PAE consumes the decoded payload, never the base64 text"
        );
    }

    #[test]
    fn sign_envelope_emits_the_fixed_payload_type_and_exactly_one_signature() {
        // The payload type is a constant rather than a parameter, and the
        // one-signature rule is what `DsseEnvelope::parse` enforces on the way
        // back in — an envelope this side writes must satisfy it.
        let envelope = sign_envelope(&fixed_key(), STATEMENT).expect("signing succeeds");
        assert_eq!(envelope.payload_type, DSSE_PAYLOAD_TYPE);
        assert_eq!(envelope.payload, STATEMENT, "the payload is held decoded");
        assert_eq!(envelope.signatures.len(), 1);
    }

    #[tokio::test]
    async fn an_oversized_statement_is_refused_before_any_endpoint_is_contacted() {
        // A Rekor entry is permanent. Signing first and discovering the size
        // afterwards publishes an attestation forever that this tool's own
        // verifier then refuses — so the bound has to run ahead of both the
        // Fulcio round trip and the log write, not merely somewhere.
        //
        // Both endpoints point at a port nothing listens on, so reaching
        // either one produces a transport error rather than this refusal:
        // the assertion on the specific kind is what makes "before" testable
        // without a recording double.
        let dead = Url::parse("http://127.0.0.1:1/").expect("a literal URL parses");
        let oversized = vec![b'x'; MAX_STATEMENT_PAYLOAD_BYTES + 1];

        let err = KeylessSigner::new()
            .sign_dsse(
                &oversized,
                Some(&OidcToken::new(EMAIL_AND_SUB.to_string())),
                &dead,
                &dead,
            )
            .await
            .expect_err("an over-cap statement must not be signed");

        assert!(
            matches!(
                err,
                SignErrorKind::PredicateTooLarge { limit, actual }
                    if limit == MAX_STATEMENT_PAYLOAD_BYTES as u64 && actual == oversized.len() as u64
            ),
            "expected PredicateTooLarge naming the limit and the actual size, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn a_statement_exactly_at_the_cap_is_not_refused_by_the_bound() {
        // The other side of the boundary: `>` not `>=`, so the cap itself is
        // allowed through. It then fails at the unreachable endpoint, which is
        // what proves the bound let it past rather than never having run.
        let dead = Url::parse("http://127.0.0.1:1/").expect("a literal URL parses");
        let at_cap = vec![b'x'; MAX_STATEMENT_PAYLOAD_BYTES];

        let err = KeylessSigner::new()
            .sign_dsse(&at_cap, Some(&OidcToken::new(EMAIL_AND_SUB.to_string())), &dead, &dead)
            .await
            .expect_err("the dead endpoint fails the sign");

        assert!(
            !matches!(err, SignErrorKind::PredicateTooLarge { .. }),
            "a statement at exactly the cap must pass the bound, got: {err:?}"
        );
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
