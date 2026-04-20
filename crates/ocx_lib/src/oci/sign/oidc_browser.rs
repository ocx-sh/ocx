// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Browser OAuth (PKCE) provider — wraps `sigstore::oauth::OauthTokenProvider`.
//!
//! Last resort in the ADR S1-C state machine: used on interactive laptops
//! when no ambient provider matches and `no_tty=false`. Opens a local
//! callback server, launches the user's browser, and exchanges the
//! authorization code for an OIDC token.
//!
//! Phase 1 stub — bodies use `unimplemented!()`. The `sigstore` crate is
//! added in Phase 5.

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
        unimplemented!("BrowserOauthProvider::acquire — Phase 5 wraps sigstore::oauth::OauthTokenProvider")
    }
}
