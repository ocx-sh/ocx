// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Inline env-inspection ambient OIDC provider: GitHub Actions (token exchange),
//! GitLab CI (`SIGSTORE_ID_TOKEN`), CircleCI (`CIRCLE_OIDC_TOKEN_V2`).

use async_trait::async_trait;
use ocx_env::{EnvVar, SecretVar};
use ocx_oci::endpoint::BodyReadError;
use zeroize::Zeroizing;

use super::error::SignErrorKind;
use super::oidc::{AmbientProvider, OidcToken, TokenProvider};

const GHA_URL: &EnvVar = &ocx_env::ACTIONS_ID_TOKEN_REQUEST_URL;
const GHA_TOKEN: &SecretVar = &ocx_env::ACTIONS_ID_TOKEN_REQUEST_TOKEN;
const GITLAB_TOKEN: &SecretVar = &ocx_env::SIGSTORE_ID_TOKEN;
const CIRCLE_TOKEN: &SecretVar = &ocx_env::CIRCLE_OIDC_TOKEN_V2;

/// Inline env-inspection ambient token provider.
pub struct InlineAmbientProvider {
    /// The shared `trusted_hosts` list; this dial gets no carve-out of its own.
    trusted_hosts: Vec<String>,
}

/// Which ambient token source the environment selects.
///
/// Carries the variable's declaration, never the token: this type is `Debug`.
#[derive(Debug, Clone, Copy)]
enum AmbientSource {
    /// The token sits in this variable; no round trip, no endpoint to guard.
    Direct(&'static SecretVar),
    GithubExchange,
}

impl PartialEq for AmbientSource {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Direct(left), Self::Direct(right)) => std::ptr::eq(*left, *right),
            (Self::GithubExchange, Self::GithubExchange) => true,
            _ => false,
        }
    }
}

/// Pick the ambient source, given which variables are set and non-empty.
///
/// The order is the contract: a reorder silently changes which identity signs on a runner that sets several.
fn select_source(gitlab: bool, circle: bool, gha: bool) -> Option<AmbientSource> {
    if gitlab {
        Some(AmbientSource::Direct(GITLAB_TOKEN))
    } else if circle {
        Some(AmbientSource::Direct(CIRCLE_TOKEN))
    } else if gha {
        Some(AmbientSource::GithubExchange)
    } else {
        None
    }
}

/// The three presence facts [`select_source`] decides on, from the process environment.
///
/// The one reader for `detect` and `acquire`, or they disagree on a set-but-empty variable.
fn ambient_env() -> (bool, bool, bool) {
    (
        GITLAB_TOKEN.get().is_some(),
        CIRCLE_TOKEN.get().is_some(),
        GHA_URL.get_os().is_some() && GHA_TOKEN.get().is_some(),
    )
}

/// Append the audience and apply both endpoint gates before anything dials.
///
/// The request carries a bearer to an environment-supplied URL, so it gets the `--fulcio-url` gates.
async fn guarded_request_url(url: &str, audience: &str, trusted_hosts: &[String]) -> Result<String, SignErrorKind> {
    let refused = |reason: &str| SignErrorKind::OidcPreCheckFailed {
        reason: reason.to_string(),
    };
    let request_url = format!("{url}&audience={audience}");
    // Scheme gate first: the SSRF guard never judges schemes, so plaintext would leak the bearer.
    // Keep the reasons distinct: only `..._forbidden` is fixed by `trusted_hosts`.
    let parsed = ocx_oci::endpoint::validate_sigstore_url(&request_url, GHA_URL.name)
        .map_err(|_| refused("gha_id_token_url_insecure_scheme"))?;
    ocx_oci::endpoint::resolve_sigstore_url(&parsed, trusted_hosts)
        .await
        .map_err(|_| refused("gha_id_token_url_forbidden"))?;
    Ok(request_url)
}

impl AmbientProvider for InlineAmbientProvider {
    fn detect(trusted_hosts: &[String]) -> Option<Box<dyn TokenProvider>> {
        let (gitlab, circle, gha) = ambient_env();
        select_source(gitlab, circle, gha).map(|_| -> Box<dyn TokenProvider> {
            Box::new(Self {
                trusted_hosts: trusted_hosts.to_vec(),
            })
        })
    }
}

#[async_trait]
impl TokenProvider for InlineAmbientProvider {
    async fn acquire(&self, audience: &str) -> Result<OidcToken, SignErrorKind> {
        let no_token = || SignErrorKind::OidcPreCheckFailed {
            reason: "ambient_provider_no_token".to_string(),
        };
        let (gitlab, circle, gha) = ambient_env();
        match select_source(gitlab, circle, gha) {
            None => return Err(no_token()),
            Some(AmbientSource::Direct(var)) => {
                return Ok(OidcToken::new(var.get().ok_or_else(no_token)?.into_inner()));
            }
            Some(AmbientSource::GithubExchange) => {}
        }

        {
            let (url, bearer) = (
                GHA_URL.get().ok_or_else(no_token)?,
                GHA_TOKEN.get().ok_or_else(no_token)?,
            );
            let bearer = Zeroizing::new(bearer.into_inner());
            let request_url = guarded_request_url(&url, audience, &self.trusted_hosts).await?;
            let response = ocx_oci::endpoint::sigstore_http_client()
                .get(&request_url)
                .header("Authorization", format!("Bearer {}", bearer.as_str()))
                .send()
                .await
                .map_err(|e| {
                    if ocx_oci::transport_policy::is_transient_transport_error(&e) {
                        SignErrorKind::OidcTokenUnavailable
                    } else {
                        SignErrorKind::OidcPreCheckFailed {
                            reason: "gha_id_token_request_failed".to_string(),
                        }
                    }
                })?;
            let status = response.status();
            if !status.is_success() {
                return Err(if ocx_oci::transport_policy::is_transient_status(status.as_u16()) {
                    SignErrorKind::OidcTokenUnavailable
                } else {
                    SignErrorKind::OidcPreCheckFailed {
                        reason: "gha_id_token_request_rejected".to_string(),
                    }
                });
            }
            #[derive(serde::Deserialize)]
            struct IdTokenResponse {
                value: String,
            }
            // Capped: the endpoint comes from the runner environment, which a compromised job controls.
            let raw = ocx_oci::endpoint::read_body_capped(response)
                .await
                .map_err(|fault| match fault {
                    // The stream broke after the headers: the same retry as a failed send.
                    BodyReadError::Transport => SignErrorKind::OidcTokenUnavailable,
                    BodyReadError::Oversize => SignErrorKind::OidcPreCheckFailed {
                        reason: "gha_id_token_malformed".to_string(),
                    },
                })?;
            let body: IdTokenResponse =
                serde_json::from_slice(&raw).map_err(|_| SignErrorKind::OidcPreCheckFailed {
                    reason: "gha_id_token_malformed".to_string(),
                })?;
            Ok(OidcToken::new(body.value))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reason(err: SignErrorKind) -> String {
        match err {
            SignErrorKind::OidcPreCheckFailed { reason } => reason,
            other => panic!("expected an OIDC pre-check failure, got: {other}"),
        }
    }

    #[tokio::test]
    async fn ambient_token_endpoint_on_the_metadata_address_is_refused() {
        // The red half. `ACTIONS_ID_TOKEN_REQUEST_URL` is read from the
        // environment and the request carries `ACTIONS_ID_TOKEN_REQUEST_TOKEN`
        // as a bearer, so an unguarded dial is a credential-carrying request to
        // wherever that variable points -- the cloud metadata address included.
        //
        // HTTPS deliberately: the scheme gate would refuse a plaintext URL
        // first, which would leave this test green with the address guard
        // deleted. On https it can only red on the address.
        let err = guarded_request_url("https://169.254.169.254/token?api-version=1", "sigstore", &[])
            .await
            .expect_err("the link-local metadata address must not be dialed");
        assert_eq!(reason(err), "gha_id_token_url_forbidden");
    }

    #[tokio::test]
    async fn a_literal_loopback_endpoint_is_admitted_and_carries_the_audience() {
        // The green half, on an address the guard would otherwise forbid: a
        // test that only ever showed a refusal cannot tell a working guard from
        // one that refuses everything. Loopback passes because the URL says so
        // in the string, which is the shared opt-in `resolve_sigstore_url`
        // defines -- a name that merely resolves there still fails.
        let ok = guarded_request_url("http://127.0.0.1:9999/token?api-version=1", "sigstore", &[])
            .await
            .expect("a literal loopback endpoint is the operator's own local runner");
        assert!(
            ok.ends_with("&audience=sigstore"),
            "audience must survive the guard: {ok}"
        );
    }

    #[tokio::test]
    async fn a_private_endpoint_is_admitted_only_via_trusted_hosts() {
        // The escape hatch is the same list Fulcio, Rekor and the registry read,
        // so a self-hosted runner on a private network is configured once. Both
        // outcomes are asserted on one address, so neither arm can pass by
        // accident.
        //
        // HTTPS for the same reason as the metadata test: on http the scheme
        // gate refuses first and the admitted arm could never pass at all.
        let url = "https://10.1.2.3/token?api-version=1";
        let err = guarded_request_url(url, "sigstore", &[])
            .await
            .expect_err("a private address is forbidden by default");
        assert_eq!(reason(err), "gha_id_token_url_forbidden");
        guarded_request_url(url, "sigstore", &["10.1.2.3".to_string()])
            .await
            .expect("an operator-trusted host is admitted");
    }

    #[tokio::test]
    async fn a_plaintext_endpoint_on_a_non_loopback_host_is_refused() {
        // The gate this path was missing. `ACTIONS_ID_TOKEN_REQUEST_URL` comes
        // from the runner environment and the exchange carries
        // `ACTIONS_ID_TOKEN_REQUEST_TOKEN` as a bearer, so a plaintext endpoint
        // on a routable host puts that credential on the wire in the clear --
        // and the SSRF guard alone cannot see it, because it judges addresses.
        //
        // Asserted twice on one URL: `trusted_hosts` buys a private *address*,
        // never cleartext, so listing the host must not admit it.
        //
        // Arm 1 does not discriminate on refusal alone: with an empty
        // `trusted_hosts` the SSRF address guard refuses 10.1.2.3 on its own,
        // scheme-agnostically, so `expect_err` there passes with
        // `validate_sigstore_url` deleted. What discriminates is the *reason*
        // -- `..._insecure_scheme` is reachable from no other call, so under
        // that mutation arm 1 reds on `left: "gha_id_token_url_forbidden"`
        // (measured, not assumed). Arm 2 reds on the refusal itself, since
        // there the address guard admits the host.
        let url = "http://10.1.2.3/token?api-version=1";
        let err = guarded_request_url(url, "sigstore", &[])
            .await
            .expect_err("plaintext to a non-loopback host must not carry the bearer token");
        assert_eq!(reason(err), "gha_id_token_url_insecure_scheme");
        let err = guarded_request_url(url, "sigstore", &["10.1.2.3".to_string()])
            .await
            .expect_err("trusted_hosts admits a private address, not a plaintext scheme");
        assert_eq!(reason(err), "gha_id_token_url_insecure_scheme");
    }

    /// A set-but-empty variable is absent, and the exchange needs both halves of the Actions pair.
    #[test]
    fn ambient_env_reads_presence_as_set_and_non_empty() {
        let (gitlab, circle) = (GITLAB_TOKEN.declaration(), CIRCLE_TOKEN.declaration());
        let (url, token) = (GHA_URL, GHA_TOKEN.declaration());
        type Presence = (bool, bool, bool);
        let rows: [(&[(&EnvVar, &str)], Presence); 5] = [
            (&[], (false, false, false)),
            (&[(gitlab, ""), (circle, "")], (false, false, false)),
            (&[(gitlab, "t"), (circle, "t")], (true, true, false)),
            (&[(url, "https://x"), (token, "")], (false, false, false)),
            (&[(url, "https://x"), (token, "t")], (false, false, true)),
        ];
        let env = ocx_env::overrides::lock();
        for (set, expected) in rows {
            for key in [gitlab, circle, url, token] {
                env.remove(key);
            }
            for (key, value) in set {
                env.set(key, *value);
            }
            assert_eq!(ambient_env(), expected, "{set:?}");
        }
    }

    // ── GitHub token exchange over a loopback stub ───────────────────────────

    /// Run the GitHub exchange against a loopback stub that answers every connection with `response`.
    async fn exchange_against_stub(response: &'static [u8]) -> Option<SignErrorKind> {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind the token stub");
        let addr = listener.local_addr().expect("the token stub has an address");
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut scratch = [0_u8; 4096];
                let _ = socket.read(&mut scratch).await;
                let _ = socket.write_all(response).await;
            }
        });
        exchange_at(addr).await
    }

    /// Run the GitHub exchange against whatever listens on `addr`.
    async fn exchange_at(addr: std::net::SocketAddr) -> Option<SignErrorKind> {
        let env = ocx_env::overrides::lock();
        env.remove(GITLAB_TOKEN.declaration());
        env.remove(CIRCLE_TOKEN.declaration());
        env.set(GHA_URL, format!("http://{addr}/token?api-version=1"));
        env.set(GHA_TOKEN.declaration(), "bearer");
        let provider = InlineAmbientProvider { trusted_hosts: vec![] };
        provider.acquire("sigstore").await.err()
    }

    #[tokio::test]
    async fn a_503_from_the_token_endpoint_is_a_retry() {
        let failure = exchange_against_stub(
            b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(
            matches!(failure, Some(SignErrorKind::OidcTokenUnavailable)),
            "got: {failure:?}"
        );
    }

    #[tokio::test]
    async fn a_token_body_that_breaks_mid_stream_is_a_retry() {
        let failure =
            exchange_against_stub(b"HTTP/1.1 200 OK\r\nContent-Length: 500\r\nConnection: close\r\n\r\n{\"val").await;
        assert!(
            matches!(failure, Some(SignErrorKind::OidcTokenUnavailable)),
            "got: {failure:?}"
        );
    }

    #[tokio::test]
    async fn a_refused_dial_to_the_token_endpoint_is_a_retry() {
        // Bind then drop, so nothing listens on the address and the connect is refused.
        let addr = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind a throwaway port")
            .local_addr()
            .expect("the throwaway port has an address");
        let failure = exchange_at(addr).await;
        assert!(
            matches!(failure, Some(SignErrorKind::OidcTokenUnavailable)),
            "got: {failure:?}"
        );
    }

    #[tokio::test]
    async fn a_redirect_from_the_token_endpoint_is_a_refusal_not_a_retry() {
        // The refused redirect is the SSRF defence against re-sending the bearer to a `Location`; a retry cannot help.
        let failure = exchange_against_stub(
            b"HTTP/1.1 307 Temporary Redirect\r\nLocation: http://127.0.0.1:1/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert_eq!(
            reason(failure.expect("a redirect fails")),
            "gha_id_token_request_failed"
        );
    }

    #[tokio::test]
    async fn a_403_from_the_token_endpoint_is_a_refusal_not_a_retry() {
        let failure =
            exchange_against_stub(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
        assert_eq!(reason(failure.expect("a 403 fails")), "gha_id_token_request_rejected");
    }

    #[tokio::test]
    async fn an_oversize_token_body_is_malformed_not_a_retry() {
        let declared = ocx_oci::endpoint::MAX_SIGSTORE_RESPONSE_BYTES + 1;
        let head: &'static [u8] = Box::leak(
            format!("HTTP/1.1 200 OK\r\nContent-Length: {declared}\r\nConnection: close\r\n\r\n")
                .into_bytes()
                .into_boxed_slice(),
        );
        let failure = exchange_against_stub(head).await;
        assert_eq!(
            reason(failure.expect("an oversize body fails")),
            "gha_id_token_malformed"
        );
    }

    // ── ambient-source precedence ────────────────────────────────────────────

    // One named test per combination rather than `#[rstest] #[case(...)]` rows
    // (`rstest` is not a workspace dependency) and rather than a `for` loop,
    // which would abort at the first failure under one opaque name (TEST-04).
    // Decided over booleans, never over the process environment: `set_var` is
    // a data race across nextest's parallel tests (TEST-05).

    #[test]
    fn no_variable_set_selects_nothing() {
        // The dispatcher relies on this to fall through to the browser path.
        assert_eq!(select_source(false, false, false), None);
    }

    #[test]
    fn gitlab_alone_selects_its_direct_token() {
        assert_eq!(
            select_source(true, false, false),
            Some(AmbientSource::Direct(GITLAB_TOKEN))
        );
    }

    #[test]
    fn circleci_alone_selects_its_direct_token() {
        assert_eq!(
            select_source(false, true, false),
            Some(AmbientSource::Direct(CIRCLE_TOKEN))
        );
    }

    #[test]
    fn github_actions_alone_selects_the_token_exchange() {
        assert_eq!(select_source(false, false, true), Some(AmbientSource::GithubExchange));
    }

    #[test]
    fn gitlab_outranks_circleci() {
        assert_eq!(
            select_source(true, true, false),
            Some(AmbientSource::Direct(GITLAB_TOKEN))
        );
    }

    #[test]
    fn gitlab_outranks_the_github_actions_exchange() {
        // The case that matters operationally: a self-hosted GitLab executor
        // that also exports the Actions pair must not trade an already-issued
        // token for a credential-carrying dial to whatever
        // `ACTIONS_ID_TOKEN_REQUEST_URL` names.
        assert_eq!(
            select_source(true, false, true),
            Some(AmbientSource::Direct(GITLAB_TOKEN))
        );
    }

    #[test]
    fn circleci_outranks_the_github_actions_exchange() {
        assert_eq!(
            select_source(false, true, true),
            Some(AmbientSource::Direct(CIRCLE_TOKEN))
        );
    }

    #[test]
    fn every_direct_source_outranks_the_exchange_when_all_three_are_set() {
        assert_eq!(
            select_source(true, true, true),
            Some(AmbientSource::Direct(GITLAB_TOKEN))
        );
    }
}
