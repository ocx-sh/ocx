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
        unimplemented!(
            "AmbientIdProvider::detect — Phase 5 delegates to `ambient_id::detect()` \
             and returns Some(Box::new(Self)) when a CI provider matches"
        )
    }
}

#[async_trait]
impl TokenProvider for AmbientIdProvider {
    async fn acquire(&self, _audience: &str) -> Result<OidcToken, SignErrorKind> {
        unimplemented!("AmbientIdProvider::acquire — Phase 5 delegates to ambient-id")
    }
}
