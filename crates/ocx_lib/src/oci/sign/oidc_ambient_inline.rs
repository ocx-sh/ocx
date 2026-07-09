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
pub struct InlineAmbientProvider;

fn env_present(key: &str) -> bool {
    std::env::var_os(key).is_some_and(|v| !v.is_empty())
}

impl AmbientProvider for InlineAmbientProvider {
    fn detect() -> Option<Box<dyn TokenProvider>> {
        let gha = env_present(GHA_URL) && env_present(GHA_TOKEN);
        if gha || env_present(GITLAB_TOKEN) || env_present(CIRCLE_TOKEN) {
            Some(Box::new(Self))
        } else {
            None
        }
    }
}

#[async_trait]
impl TokenProvider for InlineAmbientProvider {
    async fn acquire(&self, audience: &str) -> Result<OidcToken, SignErrorKind> {
        // Direct-token CIs first (no HTTP round-trip).
        if let Ok(token) = std::env::var(GITLAB_TOKEN)
            && !token.is_empty()
        {
            return Ok(OidcToken::new(token));
        }
        if let Ok(token) = std::env::var(CIRCLE_TOKEN)
            && !token.is_empty()
        {
            return Ok(OidcToken::new(token));
        }

        // GitHub Actions: exchange the request token for an audience-scoped id-token.
        if let (Ok(url), Ok(bearer)) = (std::env::var(GHA_URL), std::env::var(GHA_TOKEN)) {
            let bearer = Zeroizing::new(bearer);
            let request_url = format!("{url}&audience={audience}");
            let response = reqwest::Client::new()
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
            let body: IdTokenResponse = response.json().await.map_err(|_| SignErrorKind::OidcPreCheckFailed {
                reason: "gha_id_token_malformed".to_string(),
            })?;
            return Ok(OidcToken::new(body.value));
        }

        Err(SignErrorKind::OidcPreCheckFailed {
            reason: "ambient_provider_no_token".to_string(),
        })
    }
}
