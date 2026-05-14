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
use zeroize::Zeroizing;

use super::error::SignErrorKind;

/// An acquired OIDC identity token.
///
/// The token is held as a `Zeroizing<String>` so the memory is wiped on drop,
/// reducing the window for accidental exposure via memory dumps or swap.
/// Callers must never log the token at any level (see `tracing` negative-log
/// tests in Phase 3). The field is not `Debug`-printed to avoid accidental
/// leak in error traces.
pub struct OidcToken {
    raw: Zeroizing<String>,
}

impl OidcToken {
    /// Wrap a raw token string.
    pub fn new(raw: String) -> Self {
        Self {
            raw: Zeroizing::new(raw),
        }
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
///
/// `override_token` (if `Some`) short-circuits ambient + browser detection —
/// the value is returned directly from `acquire`. The token is held under
/// `Zeroizing` so the underlying bytes are wiped on drop, reducing the
/// exposure window against memory dumps or swap (CWE-316).
///
/// `no_tty=true` disables the interactive browser-OAuth fallback so headless
/// CI runs surface a typed error instead of hanging waiting for user input.
pub struct DispatchingTokenProvider {
    /// Precedence-resolved override token (file → stdin → env), or `None`
    /// when the CLI did not supply any of those sources.
    pub override_token: Option<Zeroizing<String>>,
    /// When `true`, suppress the browser OAuth fallback — required for CI.
    pub no_tty: bool,
}

impl DispatchingTokenProvider {
    /// Construct a dispatcher with the precedence-resolved override token
    /// (or `None`) and the `--no-tty` policy bit.
    pub fn new(override_token: Option<Zeroizing<String>>, no_tty: bool) -> Self {
        Self { override_token, no_tty }
    }
}

#[async_trait]
impl TokenProvider for DispatchingTokenProvider {
    async fn acquire(&self, _audience: &str) -> Result<OidcToken, SignErrorKind> {
        unimplemented!("DispatchingTokenProvider::acquire — Phase 5 implements ADR S1-C state machine")
    }
}

/// `--identity-token-file` source — reads the OIDC token from a file with
/// owner-only permission gating (`mode & 0o077 == 0` on Unix).
///
/// Phase 1 stub — Phase 5c wires the file read + `IdentityTokenFilePermissive`
/// gate into [`TokenProvider::acquire`] so the dispatcher composes uniformly
/// over file / stdin / env / ambient / browser sources.
pub struct FileTokenProvider {
    /// Path to the token file (validated lazily on `acquire`).
    pub path: std::path::PathBuf,
}

impl FileTokenProvider {
    /// Construct a file-backed token provider. Path is validated when
    /// [`TokenProvider::acquire`] is called, not at construction time.
    pub fn new(path: impl Into<std::path::PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

#[async_trait]
impl TokenProvider for FileTokenProvider {
    async fn acquire(&self, _audience: &str) -> Result<OidcToken, SignErrorKind> {
        unimplemented!("FileTokenProvider::acquire — Phase 5c reads + validates the token file")
    }
}

/// `--identity-token-stdin` source — reads a newline-terminated OIDC token
/// from the process's standard input. Phase 1 stub.
pub struct StdinTokenProvider;

impl StdinTokenProvider {
    /// Construct a stdin-backed token provider.
    pub fn new() -> Self {
        Self
    }
}

impl Default for StdinTokenProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl TokenProvider for StdinTokenProvider {
    async fn acquire(&self, _audience: &str) -> Result<OidcToken, SignErrorKind> {
        unimplemented!("StdinTokenProvider::acquire — Phase 5c reads + trims the stdin token")
    }
}

/// Environment-variable source — reads `OCX_IDENTITY_TOKEN` directly via
/// `std::env::var` (the value is intentionally not propagated through
/// `OcxConfigView`; see the credential exemption in `subsystem-cli.md`).
///
/// Phase 1 stub.
pub struct EnvTokenProvider {
    /// Env-var key the token is read from (defaults to `OCX_IDENTITY_TOKEN`).
    pub key: &'static str,
}

impl EnvTokenProvider {
    /// Construct an env-backed token provider.
    pub fn new(key: &'static str) -> Self {
        Self { key }
    }
}

impl Default for EnvTokenProvider {
    fn default() -> Self {
        Self::new("OCX_IDENTITY_TOKEN")
    }
}

#[async_trait]
impl TokenProvider for EnvTokenProvider {
    async fn acquire(&self, _audience: &str) -> Result<OidcToken, SignErrorKind> {
        unimplemented!("EnvTokenProvider::acquire — Phase 5c reads + validates the env-supplied token")
    }
}
