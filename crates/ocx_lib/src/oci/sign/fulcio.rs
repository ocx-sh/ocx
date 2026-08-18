// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fulcio CSR client — `POST /api/v2/signingCert`.
//!
//! Keyless signing exchanges a short-lived OIDC identity token plus an
//! ephemeral P-256 public key for a Fulcio-issued signing certificate whose
//! SAN is the OIDC subject. This module speaks the Fulcio v2 `publicKeyRequest`
//! shape directly rather than via `sigstore::fulcio::FulcioClient`. The reason
//! is not SCT support — the local TesseraCT stack mints one and the issued leaf
//! carries the embedded `1.3.6.1.4.1.11129.2.4.2` extension. It is that
//! `request_cert_v2` constructs `reqwest::Client::new()` internally, so a
//! delegated Fulcio call would carry no connect or request timeout and could
//! not be routed through the shared, guarded client (ADR
//! `adr_real_sigstore_stack_and_delegation.md` D4; upstream sigstore-rs#176).
//!
//! HTTP failures map to typed [`SignErrorKind`]s: 401/403 →
//! [`SignErrorKind::OidcTokenRejected`], other 4xx →
//! [`SignErrorKind::FulcioBadRequest`], 5xx / transport →
//! [`SignErrorKind::Internal`]. Fulcio states the actual cause only in the
//! response body, which the typed kinds cannot carry, so it is logged.

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
    /// The ephemeral public key as a PEM string, verbatim.
    ///
    /// NOT base64: `PublicKey.content` is a protobuf `string`, and protobuf-JSON
    /// renders a string as itself. Only `proofOfPossession` is a `bytes` field,
    /// which is why that one is base64 and this one is not. Sending base64 here
    /// makes real Fulcio answer 400 "malformed CSR".
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
        let body = SigningCertRequest {
            credentials: Credentials {
                oidc_identity_token: token,
            },
            public_key_request: PublicKeyRequest {
                public_key: PublicKeyField {
                    algorithm: "ECDSA",
                    content: public_key_pem,
                },
                proof_of_possession,
            },
        };

        let endpoint = self
            .url
            .join("api/v2/signingCert")
            .map_err(|e| SignErrorKind::Internal(Box::new(e)))?;
        let response = crate::oci::endpoint::sigstore_http_client()
            .post(endpoint)
            .json(&body)
            .send()
            .await
            .map_err(|e| SignErrorKind::Internal(Box::new(e)))?;

        let status = response.status();
        if !status.is_success() {
            // Fulcio puts the actionable cause ("invalid audience", "issuer not
            // configured") only in the body. Without it every failure reads as a
            // bare slug, which is unactionable in CI. The typed kinds carry no
            // payload, so log it rather than reshape the error contract.
            // Read the body before the macro: an `.await` inside the argument
            // list holds a `!Send` `dyn Value` across the suspension point and
            // makes the whole `sign` future non-`Send`.
            let body = crate::oci::endpoint::read_body_capped(response)
                .await
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned()); // LOSSY-OK: display
            // `classify_rejection` reads the body to tell an identity-token
            // rejection from a generic bad request, so an unreadable or
            // over-cap body silently downgrades the slug. Say so on the log
            // line rather than letting the operator read `fulcio_bad_request`
            // as a positive finding.
            tracing::warn!(
                status = status.as_u16(),
                detail = %body.as_deref().map_or_else(
                    || "<body unreadable or over the size cap; cause not classified>".to_string(),
                    detail_snippet,
                ),
                "Fulcio rejected the signing-certificate request"
            );
            let body = body.unwrap_or_default();
            return Err(classify_rejection(status.as_u16(), &body));
        }

        // Capped: a certificate chain is kilobytes, and this endpoint is
        // operator-supplied (`--fulcio-url`).
        let raw = crate::oci::endpoint::read_body_capped(response)
            .await
            .ok_or_else(|| SignErrorKind::Internal("Fulcio response unreadable or over the size cap".into()))?;
        let parsed: SigningCertResponse =
            serde_json::from_slice(&raw).map_err(|e| SignErrorKind::Internal(Box::new(e)))?;
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
/// The fragment Fulcio puts in its body when the OIDC token itself is bad.
///
/// Fulcio collapses every client-side rejection to HTTP 400 with gRPC status
/// code 3 (`INVALID_ARGUMENT`) and never uses 401/403, so the status line
/// alone cannot separate "your token is not acceptable" (exit 80, AuthError)
/// from "your CSR is malformed" (exit 78, ConfigError). The message is the
/// only discriminator the API offers; these are Fulcio's own public error
/// constants (`pkg/api/error.go`), the same strings cosign surfaces.
///
/// Measured against Fulcio v1.8.8 on the local stack:
///
/// | request defect | message |
/// |---|---|
/// | token signed by a foreign key, or not a JWT | `There was an error processing the identity token` |
/// | bad proof-of-possession | `The signature supplied in the request could not be verified` |
/// | unparseable public key | `The public key supplied in the request could not be parsed` |
///
/// Matched as a substring so a future prefix or suffix change does not silently
/// reclassify an auth failure as a config error. A miss is not a security
/// failure -- it degrades exit 80 to exit 78, and the request is refused either
/// way.
const FULCIO_IDENTITY_TOKEN_ERROR: &str = "error processing the identity token";

/// Map a Fulcio rejection onto the sign error it is.
///
/// A pure seam over the status line plus the body, so the status-to-exit-code
/// contract is testable without an HTTP round-trip.
fn classify_rejection(status: u16, body: &str) -> SignErrorKind {
    match status {
        401 | 403 => SignErrorKind::OidcTokenRejected,
        400..=499 if body.contains(FULCIO_IDENTITY_TOKEN_ERROR) => SignErrorKind::OidcTokenRejected,
        400..=499 => SignErrorKind::FulcioBadRequest,
        other => SignErrorKind::Internal(format!("Fulcio returned HTTP {other}").into()),
    }
}

/// Longest Fulcio error body we will put on a log line.
///
/// Fulcio's own error payloads are a short JSON object; anything past this is a
/// misconfigured proxy's HTML, which helps nobody and floods the log.
const MAX_FULCIO_DETAIL: usize = 512;

/// Render an untrusted Fulcio response body as one safe log line.
///
/// The body is remote-controlled text on its way to a terminal, and
/// `tracing-subscriber` forwards control bytes verbatim (CWE-150), so this
/// keeps a printable-ASCII allowlist rather than stripping a blocklist: every
/// C0/C1 control, every escape introducer and the whole bidi and zero-width
/// range fall outside it by construction. Truncation is by byte on an
/// already-ASCII string, so it cannot split a character.
fn detail_snippet(body: &str) -> String {
    let mut out: String = body
        .chars()
        .map(|c| if c.is_ascii_graphic() || c == ' ' { c } else { ' ' })
        .take(MAX_FULCIO_DETAIL)
        .collect();
    if body.len() > MAX_FULCIO_DETAIL {
        out.push('\u{2026}');
    }
    out.trim().to_string()
}

fn pem_to_der(pem_str: &str) -> Result<Vec<u8>, SignErrorKind> {
    let parsed = pem::parse(pem_str).map_err(|e| SignErrorKind::Internal(Box::new(e)))?;
    Ok(parsed.into_contents())
}

#[cfg(test)]
mod tests {
    use super::{MAX_FULCIO_DETAIL, SignErrorKind, classify_rejection, detail_snippet};

    /// Fulcio answers a bad OIDC token with 400, never 401/403, so the status
    /// line alone would classify an auth failure as a config error: exit 78
    /// where the CLI contract promises exit 80. Bodies are verbatim from
    /// Fulcio v1.8.8 on the local stack.
    #[test]
    fn a_rejected_identity_token_is_an_auth_failure_whatever_the_status_line_says() {
        for body in [
            r#"{"code":3,"message":"There was an error processing the identity token","details":[]}"#,
            // A prefix/suffix change upstream must not silently reclassify it.
            r#"{"message":"fulcio: there was an error processing the identity token (aud)"}"#,
        ] {
            assert!(
                matches!(classify_rejection(400, body), SignErrorKind::OidcTokenRejected),
                "identity-token rejection must be an auth failure: {body}"
            );
        }
    }

    /// The sibling 400s stay config errors -- a malformed CSR is the caller's
    /// request being wrong, not their identity being refused.
    #[test]
    fn a_malformed_request_stays_a_config_error() {
        for body in [
            r#"{"code":3,"message":"The signature supplied in the request could not be verified","details":[]}"#,
            r#"{"code":3,"message":"The public key supplied in the request could not be parsed","details":[]}"#,
        ] {
            assert!(
                matches!(classify_rejection(400, body), SignErrorKind::FulcioBadRequest),
                "malformed-request rejection must stay a config error: {body}"
            );
        }
    }

    #[test]
    fn an_explicit_auth_status_needs_no_body_and_a_server_fault_is_internal() {
        assert!(matches!(classify_rejection(401, ""), SignErrorKind::OidcTokenRejected));
        assert!(matches!(classify_rejection(403, ""), SignErrorKind::OidcTokenRejected));
        assert!(matches!(classify_rejection(503, ""), SignErrorKind::Internal(_)));
    }

    #[test]
    fn detail_snippet_passes_an_ordinary_fulcio_error_body_through() {
        let body = r#"{"code":3,"message":"invalid audience"}"#;
        assert_eq!(detail_snippet(body), body);
    }

    #[test]
    fn detail_snippet_neutralizes_escape_control_and_bidi_bytes() {
        // One case per class the log line must not carry: a CSI colour
        // sequence, an OSC 8 hyperlink, a carriage return, a NUL, a bidi
        // override and a zero-width joiner.
        let hostile = "ok\x1b[31mred\x1b]8;;http://evil\x07link\r\n\0\u{202e}\u{200d}done";
        let out = detail_snippet(hostile);
        for bad in ['\x1b', '\r', '\n', '\0', '\u{202e}', '\u{200d}', '\x07'] {
            assert!(!out.contains(bad), "{bad:?} survived in {out:?}");
        }
        // Neutralized, not deleted: the readable text is still there.
        assert!(out.contains("ok"), "{out:?}");
        assert!(out.contains("done"), "{out:?}");
    }

    #[test]
    fn detail_snippet_caps_a_flood_and_marks_the_truncation() {
        let out = detail_snippet(&"a".repeat(MAX_FULCIO_DETAIL * 3));
        assert!(out.ends_with('\u{2026}'), "{out:?}");
        assert_eq!(out.chars().filter(|c| *c == 'a').count(), MAX_FULCIO_DETAIL);
    }
}
