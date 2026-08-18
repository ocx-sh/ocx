// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Inline env-inspection ambient OIDC provider.
//!
//! Fallback that survives `ambient-id` archival / drift by inspecting CI
//! environment variables directly:
//!
//! - **GitHub Actions** — `ACTIONS_ID_TOKEN_REQUEST_URL` + `ACTIONS_ID_TOKEN_REQUEST_TOKEN`
//!   (fetches an audience-scoped id-token from the runner's token endpoint)
//! - **GitLab CI** — `SIGSTORE_ID_TOKEN` (set via an `id_tokens:` block)
//! - **CircleCI** — `CIRCLE_OIDC_TOKEN_V2`
//!
//! Other platforms return `None` so the dispatcher falls through to the
//! browser path (or a typed pre-check failure under `--no-tty`).

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
    /// Hosts the operator allowed onto otherwise-forbidden ranges, so a
    /// self-hosted runner whose token endpoint lives on a private address is
    /// configured through the same `trusted_hosts` list as Fulcio, Rekor and
    /// the registry — not through a carve-out unique to this dial.
    trusted_hosts: Vec<String>,
}

fn env_present(key: &str) -> bool {
    std::env::var_os(key).is_some_and(|v| !v.is_empty())
}

/// Which ambient token source a set of set-and-non-empty variables selects.
///
/// Carries the *variable name*, never the token: this type is `Debug`, and a
/// variant holding the credential would print it into any trace that touched
/// it (API-02).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AmbientSource {
    /// The CI hands the token over under this variable — no round trip, and no
    /// endpoint to guard.
    Direct(&'static str),
    /// GitHub Actions: exchange the request token at the runner's endpoint.
    GithubExchange,
}

/// Pick the ambient source, given which variables are set and non-empty.
///
/// **The order is the contract, not an implementation detail.** A runner can
/// legitimately set more than one — a GitLab job with `SIGSTORE_ID_TOKEN`
/// running on a self-hosted executor that also exports the Actions pair, or a
/// composite CI image carrying leftovers from another platform. Direct tokens
/// come first because they need no network round trip and no SSRF guard, so
/// preferring them removes a dial rather than adding one; between the two
/// direct sources the order is arbitrary but fixed, because a reorder silently
/// changes which identity signs on a runner that sets both.
///
/// Split out of [`InlineAmbientProvider::acquire`] so the precedence is
/// decidable from values (ARCH-12): asserting it through `acquire` would mean
/// mutating the process environment, which races every parallel test (TEST-05).
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

/// The three presence facts [`select_source`] decides on, read from the
/// process environment.
///
/// One reader for both [`AmbientProvider::detect`] and
/// [`TokenProvider::acquire`]: they previously applied the emptiness rule
/// differently — `detect` required a non-empty `ACTIONS_ID_TOKEN_REQUEST_URL`
/// while `acquire` accepted a set-but-empty one — so a runner exporting the
/// variable empty was detected as absent yet acquired as present.
fn ambient_env() -> (bool, bool, bool) {
    (
        env_present(GITLAB_TOKEN),
        env_present(CIRCLE_TOKEN),
        env_present(GHA_URL) && env_present(GHA_TOKEN),
    )
}

/// Append the audience and apply the SSRF floor before anything dials.
///
/// Same guard as every other Sigstore dial (CWE-918), and it belongs here most:
/// this request carries a bearer credential, and its target comes from the
/// process environment rather than from a flag — so it is the dial most easily
/// pointed somewhere else and the least likely to be noticed. `trusted_hosts`
/// is the escape hatch a self-hosted runner uses, identical to Fulcio and
/// Rekor; loopback is admitted by the shared carve-out when the URL says so
/// literally.
///
/// Split out from [`InlineAmbientProvider::acquire`] so it is reachable from a
/// test without mutating the process environment, which is a data race across
/// parallel tests.
async fn guarded_request_url(url: &str, audience: &str, trusted_hosts: &[String]) -> Result<String, SignErrorKind> {
    let request_url = format!("{url}&audience={audience}");
    let parsed = url::Url::parse(&request_url).map_err(|_| SignErrorKind::OidcPreCheckFailed {
        reason: "gha_id_token_url_unparseable".to_string(),
    })?;
    crate::oci::endpoint::resolve_sigstore_url(&parsed, trusted_hosts)
        .await
        .map_err(|_| SignErrorKind::OidcPreCheckFailed {
            reason: "gha_id_token_url_forbidden".to_string(),
        })?;
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
            // The variable was non-empty a moment ago and nothing in this
            // process mutates the environment, so a failed re-read is the
            // no-token case rather than a distinct failure worth its own
            // variant.
            Some(AmbientSource::Direct(key)) => return Ok(OidcToken::new(std::env::var(key).map_err(|_| no_token())?)),
            Some(AmbientSource::GithubExchange) => {}
        }

        // GitHub Actions: exchange the request token for an audience-scoped id-token.
        {
            let (url, bearer) = (
                std::env::var(GHA_URL).map_err(|_| no_token())?,
                std::env::var(GHA_TOKEN).map_err(|_| no_token())?,
            );
            let bearer = Zeroizing::new(bearer);
            let request_url = guarded_request_url(&url, audience, &self.trusted_hosts).await?;
            let response = crate::oci::endpoint::sigstore_http_client()
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
            // Capped: a JWT is kilobytes; the request-URL comes from the
            // runner environment, which a compromised job controls.
            let raw = crate::oci::endpoint::read_body_capped(response).await.ok_or_else(|| {
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
        let err = guarded_request_url("http://169.254.169.254/token?api-version=1", "sigstore", &[])
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
        let url = "http://10.1.2.3/token?api-version=1";
        let err = guarded_request_url(url, "sigstore", &[])
            .await
            .expect_err("a private address is forbidden by default");
        assert_eq!(reason(err), "gha_id_token_url_forbidden");
        guarded_request_url(url, "sigstore", &["10.1.2.3".to_string()])
            .await
            .expect("an operator-trusted host is admitted");
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
