// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Inline env-inspection ambient OIDC provider: GitHub Actions (token exchange),
//! GitLab CI (`SIGSTORE_ID_TOKEN`), CircleCI (`CIRCLE_OIDC_TOKEN_V2`).

use async_trait::async_trait;
use zeroize::Zeroizing;

use super::error::SignErrorKind;
use super::oidc::{AmbientProvider, OidcToken, TokenProvider};

const GHA_URL: &str = "ACTIONS_ID_TOKEN_REQUEST_URL";
const GHA_TOKEN: &str = "ACTIONS_ID_TOKEN_REQUEST_TOKEN";
const GITLAB_TOKEN: &str = "SIGSTORE_ID_TOKEN";
const CIRCLE_TOKEN: &str = "CIRCLE_OIDC_TOKEN_V2";

/// Inline env-inspection ambient token provider.
pub struct InlineAmbientProvider {
    /// The shared `trusted_hosts` list; this dial gets no carve-out of its own.
    trusted_hosts: Vec<String>,
}

fn env_present(key: &str) -> bool {
    std::env::var_os(key).is_some_and(|v| !v.is_empty())
}

/// Which ambient token source the environment selects.
///
/// Carries the variable name, never the token: this type is `Debug`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AmbientSource {
    /// The token sits in this variable; no round trip, no endpoint to guard.
    Direct(&'static str),
    GithubExchange,
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
        env_present(GITLAB_TOKEN),
        env_present(CIRCLE_TOKEN),
        env_present(GHA_URL) && env_present(GHA_TOKEN),
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
    let parsed = ocx_oci::endpoint::validate_sigstore_url(&request_url, GHA_URL)
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
            Some(AmbientSource::Direct(key)) => return Ok(OidcToken::new(std::env::var(key).map_err(|_| no_token())?)),
            Some(AmbientSource::GithubExchange) => {}
        }

        {
            let (url, bearer) = (
                std::env::var(GHA_URL).map_err(|_| no_token())?,
                std::env::var(GHA_TOKEN).map_err(|_| no_token())?,
            );
            let bearer = Zeroizing::new(bearer);
            let request_url = guarded_request_url(&url, audience, &self.trusted_hosts).await?;
            let response = ocx_oci::endpoint::sigstore_http_client()
                .get(&request_url)
                .header("Authorization", format!("Bearer {}", bearer.as_str()))
                .send()
                .await
                .map_err(|_| SignErrorKind::OidcPreCheckFailed {
                    reason: "gha_id_token_request_failed".to_string(),
                })?;
            if !response.status().is_success() {
                return Err(SignErrorKind::OidcPreCheckFailed {
                    reason: "gha_id_token_request_rejected".to_string(),
                });
            }
            #[derive(serde::Deserialize)]
            struct IdTokenResponse {
                value: String,
            }
            // Capped: the endpoint comes from the runner environment, which a compromised job controls.
            let raw = ocx_oci::endpoint::read_body_capped(response).await.ok_or_else(|| {
                SignErrorKind::OidcPreCheckFailed {
                    reason: "gha_id_token_malformed".to_string(),
                }
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
