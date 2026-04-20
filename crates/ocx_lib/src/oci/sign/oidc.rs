// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! OIDC token acquisition — dispatch state machine.
//!
//! Per ADR S1-C, OCX dispatches OIDC token acquisition in this order:
//!
//! 1. **Override token** (resolved by the CLI layer from `--identity-token-file`,
//!    `--identity-token-stdin`, or `OCX_IDENTITY_TOKEN` per C-S1-4 precedence)
//! 2. **Ambient providers** — primary [`oidc_ambient::AmbientIdProvider`] wraps
//!    the `ambient-id` crate; fallback [`oidc_ambient_inline::InlineAmbientProvider`]
//!    inspects CI env vars directly
//! 3. **Browser PKCE** — interactive laptop path; skipped when `no_tty=true`
//!
//! The [`DispatchingTokenProvider`] owns this state machine and returns a
//! typed [`SignErrorKind`] on failure (e.g., `OidcPreCheckFailed` with a
//! remediation hint when an ambient provider recognizes the CI platform but
//! required scopes/permissions are missing).
//!
//! Phase 1 stub — bodies use `unimplemented!()`.
//!
//! [`oidc_ambient::AmbientIdProvider`]: super::oidc_ambient::AmbientIdProvider
//! [`oidc_ambient_inline::InlineAmbientProvider`]: super::oidc_ambient_inline::InlineAmbientProvider

use async_trait::async_trait;

use super::error::SignErrorKind;

/// An acquired OIDC identity token.
///
/// The token is held as a plain `String` in-memory; callers must never log
/// it at any level (see `tracing` negative-log tests in Phase 3). The field
/// is not `Debug`-printed to avoid accidental leak in error traces.
pub struct OidcToken {
    raw: String,
}

impl OidcToken {
    /// Wrap a raw token string.
    pub fn new(raw: String) -> Self {
        Self { raw }
    }

    /// Return the raw JWT string.
    pub fn as_str(&self) -> &str {
        &self.raw
    }
}

impl std::fmt::Debug for OidcToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never expose the token bytes in Debug output.
        f.debug_struct("OidcToken").field("raw", &"<redacted>").finish()
    }
}

/// Abstract OIDC token provider.
///
/// Implementations: [`super::oidc_ambient::AmbientIdProvider`],
/// [`super::oidc_ambient_inline::InlineAmbientProvider`],
/// [`super::oidc_browser::BrowserOauthProvider`],
/// [`DispatchingTokenProvider`] (composite).
#[async_trait]
pub trait TokenProvider: Send + Sync {
    /// Acquire an OIDC token for the given Fulcio audience.
    async fn acquire(&self, audience: &str) -> Result<OidcToken, SignErrorKind>;
}

/// Ambient-detection provider.
///
/// Distinct from [`TokenProvider`] because ambient providers may report
/// "not applicable here" (returning `None` from [`detect`](Self::detect))
/// without being considered a failure — the dispatcher then falls through to
/// the next option in the chain.
pub trait AmbientProvider: Send + Sync {
    /// Returns an active token provider if this ambient environment applies,
    /// otherwise `None`.
    fn detect() -> Option<Box<dyn TokenProvider>>
    where
        Self: Sized;
}

/// Composite dispatching token provider.
///
/// Implements the ADR S1-C state machine: override → ambient chain → browser.
/// Constructed once per `ocx package sign` invocation by the CLI layer after
/// resolving the override token per C-S1-4 precedence.
pub struct DispatchingTokenProvider {
    #[allow(dead_code)]
    override_token: Option<String>,
    #[allow(dead_code)]
    no_tty: bool,
}

impl DispatchingTokenProvider {
    /// Construct a dispatcher with an optional resolved override token and TTY policy.
    ///
    /// `override_token` is the **resolved** token from one of
    /// `--identity-token-file`, `--identity-token-stdin`, or
    /// `OCX_IDENTITY_TOKEN` (per C-S1-4 precedence). The CLI layer resolves
    /// before constructing the provider so the provider never sees raw argv.
    ///
    /// `no_tty=true` skips the interactive browser path even when no ambient
    /// provider matches — used in CI to fail fast with
    /// [`SignErrorKind::OidcPreCheckFailed`].
    pub fn new(override_token: Option<String>, no_tty: bool) -> Self {
        Self { override_token, no_tty }
    }
}

#[async_trait]
impl TokenProvider for DispatchingTokenProvider {
    async fn acquire(&self, _audience: &str) -> Result<OidcToken, SignErrorKind> {
        unimplemented!("DispatchingTokenProvider::acquire — Phase 5 implements ADR S1-C state machine")
    }
}
