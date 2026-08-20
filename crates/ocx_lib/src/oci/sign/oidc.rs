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
    fn detect(trusted_hosts: &[String]) -> Option<Box<dyn TokenProvider>>
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
    /// Hosts the operator allowed onto otherwise-forbidden address ranges,
    /// resolved from config for the package's registry. An ambient provider
    /// that dials a token endpoint applies the same SSRF floor as every other
    /// Sigstore dial, so a self-hosted runner on a private network is
    /// configured once, in one place, rather than per dial site.
    pub trusted_hosts: Vec<String>,
}

impl DispatchingTokenProvider {
    /// Construct a dispatcher with the precedence-resolved override token
    /// (or `None`) and the `--no-tty` policy bit.
    pub fn new(override_token: Option<Zeroizing<String>>, no_tty: bool, trusted_hosts: Vec<String>) -> Self {
        Self {
            override_token,
            no_tty,
            trusted_hosts,
        }
    }

    /// Whether a signing identity is visible without acquiring one.
    ///
    /// Detection only — never a token exchange, never a network call, and
    /// deliberately not the browser flow, which is a *prompt* for identity
    /// rather than evidence of one. `ocx package attest` and
    /// `ocx package push --sbom` use this to choose between a signed attach and
    /// an unsigned one, so it must answer without side effects and without
    /// committing to either.
    ///
    /// The polarity is one-way on purpose: `true` here means the run signs, and
    /// a later acquisition failure is a hard error. Falling back to an unsigned
    /// attach at that point would silently downgrade a CI job configured for
    /// OIDC — the artifact would publish, look attached, and carry no identity.
    pub fn has_signing_material(&self) -> bool {
        self.override_token.is_some() || self.detect_ambient().is_some()
    }

    /// The ambient chain: the inline env-inspection provider, then the
    /// `ambient-id` wrapper. Shared by [`Self::has_signing_material`] and
    /// [`TokenProvider::acquire`] so the two cannot drift into disagreeing
    /// about whether an environment has an identity.
    fn detect_ambient(&self) -> Option<Box<dyn TokenProvider>> {
        super::oidc_ambient_inline::InlineAmbientProvider::detect(&self.trusted_hosts)
            .or_else(|| super::oidc_ambient::AmbientIdProvider::detect(&self.trusted_hosts))
    }
}

#[async_trait]
impl TokenProvider for DispatchingTokenProvider {
    /// ADR S1-C dispatch: override token → ambient providers → browser PKCE,
    /// with `--no-tty` short-circuiting the browser to a typed pre-check
    /// failure (exit 77) so headless CI never hangs.
    async fn acquire(&self, audience: &str) -> Result<OidcToken, SignErrorKind> {
        // 1. Explicit override (file / stdin / env), resolved by the CLI.
        if let Some(token) = &self.override_token {
            return Ok(OidcToken::new(token.as_str().to_owned()));
        }

        // 2. Ambient providers (CI). The inline env-inspection provider is the
        //    active path; the `ambient-id` wrapper is a documented v2 seam that
        //    currently reports "not applicable".
        let ambient = self.detect_ambient();
        if let Some(provider) = ambient
            && let Ok(token) = provider.acquire(audience).await
        {
            return Ok(token);
        }

        // 3. Browser PKCE — suppressed by --no-tty.
        if self.no_tty {
            return Err(SignErrorKind::OidcPreCheckFailed {
                reason: "no_ambient_no_tty".to_string(),
            });
        }
        super::oidc_browser::BrowserOauthProvider::new().acquire(audience).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci::sign::oidc_ambient::AmbientIdProvider;
    use crate::oci::sign::oidc_ambient_inline::InlineAmbientProvider;

    /// An override token IS the signing material, and it short-circuits every
    /// ambient read — so this row is deterministic wherever it runs.
    ///
    /// `--no-tty` is a browser-suppression policy, not evidence of identity, so
    /// it must not move the answer in either direction.
    #[test]
    fn an_override_token_is_signing_material_whatever_the_tty_policy_says() {
        for no_tty in [true, false] {
            let provider = DispatchingTokenProvider::new(Some(Zeroizing::new("jwt".into())), no_tty, Vec::new());
            assert!(
                provider.has_signing_material(),
                "an override token is an identity; no_tty={no_tty}",
            );
        }
    }

    /// Without an override the answer is exactly what ambient detection says,
    /// and nothing else — in particular not the browser flow, which is a prompt
    /// *for* an identity rather than evidence of one.
    ///
    /// Environment-dependent by construction: detection reads CI variables, and
    /// this asserts against the two providers directly rather than against a
    /// literal, so it states the contract without assuming whether the runner
    /// is a CI configured for OIDC. Off such a runner — where this is normally
    /// written and run — the expected value is `false`, so a
    /// `has_signing_material` hardwired to `true` reds here.
    #[test]
    fn without_an_override_the_answer_is_ambient_detection_alone() {
        let ambient = InlineAmbientProvider::detect(&[])
            .or_else(|| AmbientIdProvider::detect(&[]))
            .is_some();
        for no_tty in [true, false] {
            let provider = DispatchingTokenProvider::new(None, no_tty, Vec::new());
            assert_eq!(
                provider.has_signing_material(),
                ambient,
                "the browser flow must not count as signing material; no_tty={no_tty}",
            );
        }
    }
}
