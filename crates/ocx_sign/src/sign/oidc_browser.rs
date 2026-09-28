// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Browser OAuth (PKCE) provider; not implemented, so `acquire` always refuses with a typed error.

use async_trait::async_trait;

use super::error::SignErrorKind;
use super::oidc::{OidcToken, TokenProvider};

/// Browser OAuth PKCE token provider.
pub struct BrowserOauthProvider;

impl BrowserOauthProvider {
    /// Construct a new browser OAuth provider.
    pub fn new() -> Self {
        Self
    }
}

impl Default for BrowserOauthProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl TokenProvider for BrowserOauthProvider {
    async fn acquire(&self, _audience: &str) -> Result<OidcToken, SignErrorKind> {
        // Refuse rather than hang: interactive PKCE cannot run headless.
        Err(SignErrorKind::OidcPreCheckFailed {
            reason: "browser_flow_unavailable".to_string(),
        })
    }
}
