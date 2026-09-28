// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! [`KeySigner`] — the key-pair half of signing, delegated to a [`KeyBackend`].
//!
//! Bypasses `sigstore::bundle::sign`, whose `to_bundle()` has no `Content::PublicKey` arm.

use std::sync::Arc;

use async_trait::async_trait;
use url::Url;

use super::bundle::{SignedBundle, SignedEnvelope, SigningMaterial, build_dsse_bundle};
use super::error::SignErrorKind;
use super::key_backend::KeyBackend;
use super::oidc::OidcToken;
use super::rekor::RekorClient;
use super::signer::Signer;
use crate::attest::dsse::{DsseEnvelope, DsseSignature, pae};
use crate::attest::{DSSE_PAYLOAD_TYPE, MAX_STATEMENT_PAYLOAD_BYTES};

/// Signs with a key pair held by a [`KeyBackend`], with no Fulcio and no OIDC.
pub struct KeySigner {
    backend: Arc<dyn KeyBackend>,
    /// Where to upload, when uploading; `None` by default under a key, unlike cosign.
    ///
    /// Default-on would publish a private artifact's digest and signer to the public Rekor, irreversibly.
    rekor_url: Option<Url>,
}

impl KeySigner {
    /// Sign with `backend`, uploading to `rekor_url` when it is `Some`.
    pub fn new(backend: Arc<dyn KeyBackend>, rekor_url: Option<Url>) -> Self {
        Self { backend, rekor_url }
    }
}

#[async_trait]
impl Signer for KeySigner {
    async fn sign_blob(
        &self,
        payload: &[u8],
        token: Option<&OidcToken>,
        _fulcio_url: &Url,
        _rekor_url: &Url,
    ) -> Result<super::signer::SignedBlob, SignErrorKind> {
        debug_assert!(
            token.is_none(),
            "the pipeline must not spend an OIDC token on a signer that declares it needs none",
        );

        // `sha256(payload)`, not the PAE: cosign's `hashedrekord` entry is over the same digest.
        let payload_digest = sha256(payload);
        let signature = self.backend.sign_prehash(&payload_digest).await?;

        let entry = match &self.rekor_url {
            Some(url) => Some(
                RekorClient::new(url.clone())
                    .upload_entry(
                        &signature,
                        &pem_public_key(self.backend.public_key_der())?,
                        &hex::encode(&payload_digest),
                    )
                    .await?,
            ),
            None => None,
        };

        Ok(super::signer::SignedBlob {
            signature,
            certificate_pem: None,
            chain_pem: None,
            transparency_log_index: entry.as_ref().map(|entry| entry.log_index),
            rekor_bundle: entry
                .as_ref()
                .map(super::simplesigning_write::offline_bundle)
                .transpose()?,
            key_backend: self.backend.kind(),
            public_key_hint: Some(self.backend.hint()),
        })
    }

    fn requires_identity_token(&self) -> bool {
        false
    }

    fn uploads_to_transparency_log(&self) -> bool {
        self.rekor_url.is_some()
    }

    async fn sign_dsse(
        &self,
        statement_bytes: &[u8],
        token: Option<&OidcToken>,
        _fulcio_url: &Url,
        _rekor_url: &Url,
    ) -> Result<SignedBundle, SignErrorKind> {
        debug_assert!(
            token.is_none(),
            "the pipeline must not spend an OIDC token on a signer that declares it needs none",
        );

        // Capped before any network: an over-cap Rekor entry is permanent yet refused by our own verifier.
        if statement_bytes.len() > MAX_STATEMENT_PAYLOAD_BYTES {
            return Err(SignErrorKind::PredicateTooLarge {
                limit: MAX_STATEMENT_PAYLOAD_BYTES as u64,
                actual: statement_bytes.len() as u64,
            });
        }

        // Sign the PAE, never the bare bytes, or one signature holds for the same bytes under another payload type.
        let signature = self
            .backend
            .sign_prehash(&sha256(&pae(DSSE_PAYLOAD_TYPE, statement_bytes)))
            .await?;

        let hint = self.backend.hint();
        let signed = SignedEnvelope::new(DsseEnvelope {
            payload: statement_bytes.to_vec(),
            payload_type: DSSE_PAYLOAD_TYPE.to_string(),
            signatures: vec![DsseSignature {
                sig: signature,
                // Empty: `cosign verify` drops every signature whose `keyid` it cannot match, before any crypto runs.
                keyid: String::new(),
            }],
        })?;

        let entry = match &self.rekor_url {
            Some(url) => Some(
                RekorClient::new(url.clone())
                    .upload_dsse_entry(signed.json(), &pem_public_key(self.backend.public_key_der())?)
                    .await?,
            ),
            None => None,
        };

        build_dsse_bundle(
            SigningMaterial::PublicKey {
                hint: &hint,
                kind: self.backend.kind(),
            },
            &signed,
            entry.as_ref(),
        )
    }

    fn signer_kind(&self) -> &'static str {
        "key"
    }
}

fn sha256(bytes: &[u8]) -> Vec<u8> {
    use sha2::Digest as _;
    sha2::Sha256::digest(bytes).to_vec()
}

/// PEM-encode an SPKI DER public key, the form Rekor's `verifier` field takes.
fn pem_public_key(spki_der: &[u8]) -> Result<String, SignErrorKind> {
    use base64::Engine as _;
    let body = base64::engine::general_purpose::STANDARD.encode(spki_der);
    let mut pem = String::from("-----BEGIN PUBLIC KEY-----\n");
    for line in body.as_bytes().chunks(64) {
        pem.push_str(std::str::from_utf8(line).map_err(|e| SignErrorKind::Internal(Box::new(e)))?);
        pem.push('\n');
    }
    pem.push_str("-----END PUBLIC KEY-----\n");
    Ok(pem)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sign::key_backend::{KeyBackendError, StubKeyBackend};
    use ocx_trust::key_ref::KeyBackendKind;

    fn signer() -> KeySigner {
        KeySigner::new(Arc::new(StubKeyBackend::fixed()), None)
    }

    fn statement() -> Vec<u8> {
        br#"{"_type":"https://in-toto.io/Statement/v1"}"#.to_vec()
    }

    fn fulcio() -> Url {
        Url::parse("http://127.0.0.1:5555").expect("fulcio url")
    }

    fn rekor() -> Url {
        Url::parse("http://127.0.0.1:3000").expect("rekor url")
    }

    /// The whole point of key mode: no Fulcio round trip, no OIDC token, and a
    /// bundle whose verification material is a `publicKey` hint rather than a
    /// certificate — which is the shape `key_bundle.json` carries.
    #[tokio::test]
    async fn a_key_signature_carries_a_public_key_hint_and_no_certificate() {
        let bundle = signer()
            .sign_dsse(&statement(), None, &fulcio(), &rekor())
            .await
            .expect("key-mode signing needs no network");

        let parsed: serde_json::Value = serde_json::from_slice(&bundle.bytes).expect("bundle is JSON");
        assert!(
            parsed["verificationMaterial"]["publicKey"]["hint"].is_string(),
            "a key-mode bundle identifies its key by hint: {parsed}",
        );
        assert!(
            parsed["verificationMaterial"]["certificate"].is_null(),
            "a key-mode bundle carries no Fulcio certificate: {parsed}",
        );
        assert!(parsed["dsseEnvelope"].is_object(), "the payload is a DSSE envelope");
        // The interop half of the same identification: cosign omits `keyid` in
        // key mode too (golden `key_bundle.json`) and matches DSSE candidates
        // on it, so a hint here makes `cosign verify --key` accept 0 of 1
        // signatures. The hint asserted above is where the key is named.
        assert!(
            parsed["dsseEnvelope"]["signatures"][0]["keyid"].is_null(),
            "the DSSE envelope must carry no keyid, as cosign's own does not: {parsed}",
        );

        assert_eq!(bundle.key_backend, KeyBackendKind::File);
        assert_eq!(
            bundle.public_key_hint.as_deref(),
            Some(StubKeyBackend::fixed().hint().as_str())
        );
        assert!(
            bundle.certificate_identity.is_empty() && bundle.certificate_oidc_issuer.is_empty(),
            "no certificate means no identity and no issuer, rather than invented ones",
        );
    }

    /// `--no-rekor-upload` under a key is the default, and it is a **legal**
    /// shape (spec D5): the bundle carries no transparency-log entry and the
    /// result says so, rather than leaving the operator to infer it.
    #[tokio::test]
    async fn a_key_signature_without_an_upload_reports_no_transparency_record() {
        let bundle = signer()
            .sign_dsse(&statement(), None, &fulcio(), &rekor())
            .await
            .expect("signing");

        assert_eq!(bundle.transparency_log_index, None);
        let parsed: serde_json::Value = serde_json::from_slice(&bundle.bytes).expect("bundle is JSON");
        assert_eq!(
            parsed["verificationMaterial"]["tlogEntries"]
                .as_array()
                .map_or(0, Vec::len),
            0,
            "no upload means no tlog entry: {parsed}",
        );
    }

    /// The DSSE signature covers `sha256(PAE(payloadType, statement))`, not the
    /// statement bytes — the rule that stops one signature being valid over the
    /// same bytes under a different declared type.
    #[tokio::test]
    async fn the_signature_covers_the_pae_of_the_statement_not_the_statement() {
        let backend = StubKeyBackend::fixed();
        let expected = backend
            .sign_prehash(&sha256(&pae(DSSE_PAYLOAD_TYPE, &statement())))
            .await
            .expect("the stub signs");

        let bundle = signer()
            .sign_dsse(&statement(), None, &fulcio(), &rekor())
            .await
            .expect("signing");
        let parsed: serde_json::Value = serde_json::from_slice(&bundle.bytes).expect("bundle is JSON");
        let sig = parsed["dsseEnvelope"]["signatures"][0]["sig"]
            .as_str()
            .expect("the envelope carries a signature");

        use base64::Engine as _;
        assert_eq!(
            base64::engine::general_purpose::STANDARD.decode(sig).expect("base64"),
            expected,
            "the signed bytes must be the PAE digest",
        );
    }

    /// A backend that cannot sign fails the run rather than producing an
    /// unsigned bundle, and it fails with the backend's own error class — a
    /// KMS being unreachable (retry, exit 75) is a different operator action
    /// from a malformed key (exit 65).
    #[tokio::test]
    async fn a_backend_failure_fails_the_signature_with_its_own_class() {
        struct UnavailableBackend;

        #[async_trait::async_trait]
        impl KeyBackend for UnavailableBackend {
            async fn sign_prehash(&self, _: &[u8]) -> Result<Vec<u8>, KeyBackendError> {
                Err(KeyBackendError::Unavailable {
                    reason: "the KMS did not answer".to_owned(),
                })
            }

            fn public_key_der(&self) -> &[u8] {
                &[]
            }

            fn kind(&self) -> KeyBackendKind {
                KeyBackendKind::AwsKms
            }
        }

        let signer = KeySigner::new(Arc::new(UnavailableBackend), None);
        let error = signer
            .sign_dsse(&statement(), None, &fulcio(), &rekor())
            .await
            .expect_err("a failing backend must not yield a bundle");
        assert!(
            matches!(error, SignErrorKind::KeyBackend(KeyBackendError::Unavailable { .. })),
            "expected the backend's own error class, got: {error:?}",
        );
        // The backend's class survives into the exit code: a KMS being
        // unreachable is retryable (75), which is a different operator action
        // from a malformed key (65).
    }

    /// The public-key PEM Rekor receives is the standard 64-column SPKI form.
    #[test]
    fn the_rekor_verifier_is_a_standard_spki_pem() {
        let pem = pem_public_key(&[0xab; 100]).expect("encode");
        assert!(pem.starts_with("-----BEGIN PUBLIC KEY-----\n"));
        assert!(pem.ends_with("-----END PUBLIC KEY-----\n"));
        let body: Vec<&str> = pem.lines().filter(|line| !line.starts_with("-----")).collect();
        assert!(
            body.iter().all(|line| line.len() <= 64),
            "PEM body wraps at 64 columns, got: {body:?}",
        );
    }
}
