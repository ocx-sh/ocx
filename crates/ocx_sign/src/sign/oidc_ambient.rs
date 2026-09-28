// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Ambient OIDC provider seam for the `ambient-id` crate, not yet a dependency.

use async_trait::async_trait;

use super::error::SignErrorKind;
use super::oidc::{AmbientProvider, OidcToken, TokenProvider};

/// `ambient-id`-backed ambient token provider.
pub struct AmbientIdProvider;

impl AmbientProvider for AmbientIdProvider {
    fn detect(_trusted_hosts: &[String]) -> Option<Box<dyn TokenProvider>> {
        // Always `None`, so dispatch falls through to `oidc_ambient_inline`.
        None
    }
}

#[async_trait]
impl TokenProvider for AmbientIdProvider {
    async fn acquire(&self, _audience: &str) -> Result<OidcToken, SignErrorKind> {
        Err(SignErrorKind::OidcPreCheckFailed {
            reason: "ambient_id_not_wired".to_string(),
        })
    }
}
