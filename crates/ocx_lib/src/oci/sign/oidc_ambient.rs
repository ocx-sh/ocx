// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Primary ambient OIDC provider — wraps the `ambient-id` crate.
//!
//! `ambient-id` replaces the archived `jku/ci-id` crate (archived 2026-01-27;
//! see Researcher A1). Covers GitHub Actions, GitLab CI, CircleCI, Buildkite,
//! and GCP metadata-server flows out of the box.
//!
//! Phase 1 stub — bodies use `unimplemented!()`. The `ambient-id` dependency
//! is added in Phase 5.

use async_trait::async_trait;

use super::error::SignErrorKind;
use super::oidc::{AmbientProvider, OidcToken, TokenProvider};

/// `ambient-id`-backed ambient token provider.
pub struct AmbientIdProvider;

impl AmbientProvider for AmbientIdProvider {
    fn detect() -> Option<Box<dyn TokenProvider>> {
        // v2 seam: the `ambient-id` crate is not yet a dependency (it is in
        // Fedora packaging review). The inline env-inspection provider
        // (`oidc_ambient_inline`) is the active ambient path for v1; this
        // provider reports "not applicable" so the dispatch chain falls
        // through to it.
        None
    }
}

#[async_trait]
impl TokenProvider for AmbientIdProvider {
    async fn acquire(&self, _audience: &str) -> Result<OidcToken, SignErrorKind> {
        // Unreachable in v1: `detect` always returns `None`, so the dispatcher
        // never constructs this provider. Kept as the typed v2 seam.
        Err(SignErrorKind::OidcPreCheckFailed {
            reason: "ambient_id_not_wired".to_string(),
        })
    }
}
