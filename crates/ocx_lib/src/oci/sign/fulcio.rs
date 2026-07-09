// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fulcio CSR client — `POST /api/v2/signingCert`.
//!
//! Keyless signing exchanges a short-lived OIDC identity token plus an
//! ephemeral P-256 public key for a Fulcio-issued signing certificate whose
//! SAN is the OIDC subject. This module speaks the Fulcio v2 `publicKeyRequest`
//! shape directly (rather than via `sigstore::fulcio::FulcioClient`, which
//! sends a CSR + Bearer header and mandates a CT-log SCT the offline fake
//! stack cannot mint — see `research_sigstore_rs_spike.md`).
//!
//! HTTP failures map to typed [`SignErrorKind`]s: 401/403 →
//! [`SignErrorKind::OidcTokenRejected`], other 4xx →
//! [`SignErrorKind::FulcioBadRequest`], 5xx / transport →
//! [`SignErrorKind::Internal`].

use serde::{Deserialize, Serialize};
use url::Url;

use super::error::SignErrorKind;

/// Fulcio-issued signing certificate leaf.
///
/// `leaf_der` holds DER bytes (the bundle embeds the leaf as
/// `X509Certificate.raw_bytes`; Sigstore bundles omit the root, which lives in
/// the trust root). `leaf_pem` is retained because the Rekor `hashedrekord`
/// entry references the certificate as base64-encoded PEM.
pub(super) struct FulcioCertificate {
    pub(super) leaf_der: Vec<u8>,
    pub(super) leaf_pem: String,
}

/// Minimal Fulcio v2 client.
pub(super) struct FulcioClient {
    url: Url,
}

// ── Fulcio v2 request/response wire types (subset we use) ────────────────────

#[derive(Serialize)]
struct SigningCertRequest<'a> {
    credentials: Credentials<'a>,
    #[serde(rename = "publicKeyRequest")]
    public_key_request: PublicKeyRequest<'a>,
}

#[derive(Serialize)]
struct Credentials<'a> {
    #[serde(rename = "oidcIdentityToken")]
    oidc_identity_token: &'a str,
}

#[derive(Serialize)]
struct PublicKeyRequest<'a> {
    #[serde(rename = "publicKey")]
    public_key: PublicKeyField<'a>,
    #[serde(rename = "proofOfPossession")]
    proof_of_possession: &'a str,
}

#[derive(Serialize)]
struct PublicKeyField<'a> {
    algorithm: &'a str,
    /// Base64-encoded PEM of the ephemeral public key.
    content: &'a str,
}

#[derive(Deserialize)]
struct SigningCertResponse {
    #[serde(rename = "signedCertificateEmbeddedSct")]
    embedded: Option<SignedCertificate>,
    #[serde(rename = "signedCertificateDetachedSct")]
    detached: Option<SignedCertificate>,
}

#[derive(Deserialize)]
struct SignedCertificate {
    chain: CertChain,
}

#[derive(Deserialize)]
struct CertChain {
    certificates: Vec<String>,
}

impl FulcioClient {
    pub(super) fn new(url: Url) -> Self {
        Self { url }
    }

    /// Exchange an OIDC token + ephemeral public key for a signing certificate.
    ///
    /// `public_key_pem` is the SubjectPublicKeyInfo PEM of the ephemeral P-256
    /// key; `proof_of_possession` is base64 of the ephemeral key's signature
    /// over the token subject (the public-good Fulcio validates it; the offline
    /// fake ignores it, but we send a real one for interop fidelity).
    pub(super) async fn request_certificate(
        &self,
        token: &str,
        public_key_pem: &str,
        proof_of_possession: &str,
    ) -> Result<FulcioCertificate, SignErrorKind> {
        use base64::Engine as _;
        let content = base64::engine::general_purpose::STANDARD.encode(public_key_pem.as_bytes());
        let body = SigningCertRequest {
            credentials: Credentials {
                oidc_identity_token: token,
            },
            public_key_request: PublicKeyRequest {
                public_key: PublicKeyField {
                    algorithm: "ECDSA",
                    content: &content,
                },
                proof_of_possession,
            },
        };

        let endpoint = self
            .url
            .join("api/v2/signingCert")
            .map_err(|e| SignErrorKind::Internal(Box::new(e)))?;
        let response = reqwest::Client::new()
            .post(endpoint)
            .json(&body)
            .send()
            .await
            .map_err(|e| SignErrorKind::Internal(Box::new(e)))?;

        let status = response.status();
        if !status.is_success() {
            return Err(match status.as_u16() {
                401 | 403 => SignErrorKind::OidcTokenRejected,
                400..=499 => SignErrorKind::FulcioBadRequest,
                _ => SignErrorKind::Internal(format!("Fulcio returned HTTP {status}").into()),
            });
        }

        let parsed: SigningCertResponse = response
            .json()
            .await
            .map_err(|e| SignErrorKind::Internal(Box::new(e)))?;
        let certificates = parsed
            .embedded
            .or(parsed.detached)
            .map(|c| c.chain.certificates)
            .ok_or_else(|| SignErrorKind::Internal("Fulcio response missing certificate chain".into()))?;
        if certificates.len() < 2 {
            return Err(SignErrorKind::Internal(
                format!("Fulcio chain too short: {} cert(s)", certificates.len()).into(),
            ));
        }

        let leaf_pem = certificates[0].clone();
        let leaf_der = pem_to_der(&leaf_pem)?;
        Ok(FulcioCertificate { leaf_der, leaf_pem })
    }
}

/// Decode a PEM `CERTIFICATE` block into DER bytes.
fn pem_to_der(pem_str: &str) -> Result<Vec<u8>, SignErrorKind> {
    let parsed = pem::parse(pem_str).map_err(|e| SignErrorKind::Internal(Box::new(e)))?;
    Ok(parsed.into_contents())
}
