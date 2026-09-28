// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fulcio CSR client — `POST /api/v2/signingCert`.
//!
//! Not `sigstore::fulcio::FulcioClient`: it builds an unguarded, timeout-free `reqwest::Client`
//! (`adr_real_sigstore_stack_and_delegation.md`).

use serde::{Deserialize, Serialize};
use url::Url;

use super::error::SignErrorKind;

/// Fulcio-issued signing certificate leaf; the PEM form is what the Rekor `hashedrekord` entry references.
pub(super) struct FulcioCertificate {
    pub(super) leaf_der: Vec<u8>,
    pub(super) leaf_pem: String,
}

/// Minimal Fulcio v2 client.
pub(super) struct FulcioClient {
    url: Url,
}

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
    /// The PEM verbatim, NOT base64 (a protobuf `string`): base64 here makes real Fulcio answer 400.
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
    /// `proof_of_possession` is base64 of the ephemeral key's signature over the token subject.
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
        let response = ocx_oci::endpoint::sigstore_http_client()
            .post(endpoint)
            .json(&body)
            .send()
            .await
            // Transport failures are a retryable outage, never `Internal`.
            .map_err(|_| SignErrorKind::FulcioUnavailable)?;

        let status = response.status();
        if !status.is_success() {
            // The cause is only in the body, so it is logged.
            // Read before the macro: an `.await` in its arguments makes `sign` non-`Send`.
            let body = ocx_oci::endpoint::read_body_capped(response)
                .await
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned()); // LOSSY-OK: display
            // Say when the body was unreadable, or `fulcio_bad_request` reads as a positive finding.
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

        // Capped: the endpoint is operator-supplied (`--fulcio-url`).
        let raw = ocx_oci::endpoint::read_body_capped(response)
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

/// The fragment Fulcio puts in its body when the OIDC token itself is bad.
///
/// Fulcio answers every client rejection with 400, so this is the only auth-vs-config discriminator;
/// matched as a substring, and a miss degrades exit 80 to 78.
const FULCIO_IDENTITY_TOKEN_ERROR: &str = "error processing the identity token";

/// Map a Fulcio rejection onto the sign error it is.
fn classify_rejection(status: u16, body: &str) -> SignErrorKind {
    match status {
        401 | 403 => SignErrorKind::OidcTokenRejected,
        // 429 leads the 4xx arms: it is retryable.
        429 => SignErrorKind::FulcioUnavailable,
        400..=499 if body.contains(FULCIO_IDENTITY_TOKEN_ERROR) => SignErrorKind::OidcTokenRejected,
        400..=499 => SignErrorKind::FulcioBadRequest,
        500..=599 => SignErrorKind::FulcioUnavailable,
        other => SignErrorKind::Internal(format!("Fulcio returned HTTP {other}").into()),
    }
}

/// Longest Fulcio error body we will put on a log line.
const MAX_FULCIO_DETAIL: usize = 512;

/// Render an untrusted Fulcio response body as one safe log line.
///
/// A printable-ASCII allowlist, never a blocklist: `tracing-subscriber` forwards control bytes verbatim (CWE-150).
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
    fn an_explicit_auth_status_needs_no_body() {
        assert!(matches!(classify_rejection(401, ""), SignErrorKind::OidcTokenRejected));
        assert!(matches!(classify_rejection(403, ""), SignErrorKind::OidcTokenRejected));
    }

    /// PKG-16's retryable set — 429 plus every 5xx — is a transient Fulcio
    /// outage, not a defect in the request. It has to reach exit 75 so a CI
    /// runner can retry it, and it has to stay a *different* integer from the
    /// hard 4xx refusals so the two are told apart (PKG-28).
    #[test]
    fn a_transient_fulcio_fault_is_retryable_and_a_hard_refusal_is_not() {
        for status in [429, 500, 502, 503, 504] {
            assert!(
                matches!(classify_rejection(status, ""), SignErrorKind::FulcioUnavailable),
                "HTTP {status} from Fulcio must be retryable"
            );
        }

        // The hard refusals keep their own codes, and none of them is 75.
        // The hard refusals keep their own variants; their exit codes are
        // asserted beside the impl, in `ocx_cli::exit`.
        for (status, expected) in [
            (400, SignErrorKind::FulcioBadRequest),
            (401, SignErrorKind::OidcTokenRejected),
        ] {
            assert_eq!(
                std::mem::discriminant(&classify_rejection(status, "")),
                std::mem::discriminant(&expected),
                "HTTP {status}"
            );
        }
    }

    /// A Fulcio that cannot be dialled at all is the same transient class as a
    /// 503, and used to land on `Internal` -> exit 1: indistinguishable from a
    /// bug in ocx, and unretryable by anything reading `$?`.
    ///
    /// The port is bound and released so it is known-closed, which is what
    /// makes the connect refusal deterministic rather than a race.
    #[tokio::test]
    async fn an_unreachable_fulcio_is_retryable_not_an_internal_bug() {
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
            listener.local_addr().expect("local addr").port()
        };
        let url = url::Url::parse(&format!("http://127.0.0.1:{port}/")).expect("loopback url");

        // `let Err(..) else` rather than `expect_err`: the Ok type is a
        // certificate and does not need a `Debug` impl minted for a test.
        let Err(err) = super::FulcioClient::new(url)
            .request_certificate("token", "-----BEGIN PUBLIC KEY-----", "sig")
            .await
        else {
            panic!("a closed port cannot issue a certificate");
        };

        assert!(
            matches!(err, SignErrorKind::FulcioUnavailable),
            "connect-refused must be retryable, got {err:?}"
        );
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
