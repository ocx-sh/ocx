// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Inline env-inspection ambient OIDC provider.
//!
//! ~80-line fallback that survives `ambient-id` archival / drift. Inspects
//! environment variables directly for:
//!
//! - **GitHub Actions** — `ACTIONS_ID_TOKEN_REQUEST_URL` + `ACTIONS_ID_TOKEN_REQUEST_TOKEN`
//! - **GitLab CI** — `SIGSTORE_ID_TOKEN` (when `id_tokens:` block configured)
//! - **CircleCI** — `CIRCLE_OIDC_TOKEN_V2`
//! - **Buildkite** — agent API
//! - **GCP** — metadata server at `169.254.169.254`
//!
//! Runs AFTER [`super::oidc_ambient::AmbientIdProvider`] in the dispatch
//! chain, providing hedged coverage when the primary crate returns `None` but
//! the env is clearly CI.
//!
//! Phase 1 stub — bodies use `unimplemented!()`.

use async_trait::async_trait;

use super::error::SignErrorKind;
use super::oidc::{AmbientProvider, OidcToken, TokenProvider};

/// Inline env-inspection ambient token provider.
pub struct InlineAmbientProvider;

impl AmbientProvider for InlineAmbientProvider {
    fn detect() -> Option<Box<dyn TokenProvider>> {
        unimplemented!(
            "InlineAmbientProvider::detect — Phase 5 inspects CI env vars and \
             returns Some(Box::new(Self)) when a known pattern matches"
        )
    }
}

#[async_trait]
impl TokenProvider for InlineAmbientProvider {
    async fn acquire(&self, _audience: &str) -> Result<OidcToken, SignErrorKind> {
        unimplemented!("InlineAmbientProvider::acquire — Phase 5 fetches via HTTP or reads env directly")
    }
}
