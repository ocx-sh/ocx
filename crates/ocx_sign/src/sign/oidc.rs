// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! OIDC token acquisition: override token → ambient providers → browser PKCE.

use async_trait::async_trait;
use zeroize::Zeroizing;

use super::error::SignErrorKind;

/// An acquired OIDC identity token, zeroized on drop and redacted in `Debug`.
///
/// Never log the token at any level.
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
        f.debug_struct("OidcToken").field("raw", &"<redacted>").finish()
    }
}

/// An OIDC token source.
#[async_trait]
pub trait TokenProvider: Send + Sync {
    /// Acquire an OIDC token for the given Fulcio audience.
    async fn acquire(&self, audience: &str) -> Result<OidcToken, SignErrorKind>;
}

/// Ambient-detection provider; `None` from [`detect`](Self::detect) means "not applicable", not a failure.
pub trait AmbientProvider: Send + Sync {
    fn detect(trusted_hosts: &[String]) -> Option<Box<dyn TokenProvider>>
    where
        Self: Sized;
}

/// Composite dispatching token provider: override → ambient chain → browser.
///
/// `no_tty=true` disables the browser fallback, so headless CI gets a typed error instead of hanging.
pub struct DispatchingTokenProvider {
    /// Precedence-resolved override token (file → stdin → env); short-circuits detection.
    pub override_token: Option<Zeroizing<String>>,
    pub no_tty: bool,
    /// Hosts allowed onto otherwise-forbidden ranges; ambient token endpoints obey the same SSRF floor.
    pub trusted_hosts: Vec<String>,
}

impl DispatchingTokenProvider {
    pub fn new(override_token: Option<Zeroizing<String>>, no_tty: bool, trusted_hosts: Vec<String>) -> Self {
        Self {
            override_token,
            no_tty,
            trusted_hosts,
        }
    }

    /// Whether a signing identity is visible without acquiring one.
    ///
    /// Detection only: no token exchange, network call or browser prompt.
    /// `true` commits the run to signing; a later acquisition failure must be a hard error, never an unsigned downgrade.
    pub fn has_signing_material(&self) -> bool {
        self.override_token.is_some() || self.detect_ambient().is_some()
    }

    /// Shared by [`Self::has_signing_material`] and `acquire`, or they disagree about whether an identity exists.
    fn detect_ambient(&self) -> Option<Box<dyn TokenProvider>> {
        super::oidc_ambient_inline::InlineAmbientProvider::detect(&self.trusted_hosts)
            .or_else(|| super::oidc_ambient::AmbientIdProvider::detect(&self.trusted_hosts))
    }
}

#[async_trait]
impl TokenProvider for DispatchingTokenProvider {
    async fn acquire(&self, audience: &str) -> Result<OidcToken, SignErrorKind> {
        if let Some(token) = &self.override_token {
            return Ok(OidcToken::new(token.as_str().to_owned()));
        }

        // A detected provider's error is the answer: falling through would turn a transient fetch (75) into a
        // `no_ambient_no_tty` refusal (77), and the browser fallback never yields a token.
        if let Some(provider) = self.detect_ambient() {
            return provider.acquire(audience).await;
        }

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
    use crate::sign::oidc_ambient::AmbientIdProvider;
    use crate::sign::oidc_ambient_inline::InlineAmbientProvider;

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
        // Held so a sibling test's ambient overrides cannot land between the two reads.
        let _env = ocx_env::overrides::lock();
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

    /// A detected ambient provider's transient failure must reach the caller as a retry (exit 75), not a refusal.
    #[tokio::test]
    async fn a_transient_ambient_failure_surfaces_through_the_dispatcher() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind the token stub");
        let addr = listener.local_addr().expect("the token stub has an address");
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut scratch = [0_u8; 4096];
                let _ = socket.read(&mut scratch).await;
                let _ = socket
                    .write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .await;
            }
        });
        let env = ocx_env::overrides::lock();
        env.remove(ocx_env::SIGSTORE_ID_TOKEN.declaration());
        env.remove(ocx_env::CIRCLE_OIDC_TOKEN_V2.declaration());
        env.set(
            &ocx_env::ACTIONS_ID_TOKEN_REQUEST_URL,
            format!("http://{addr}/token?api-version=1"),
        );
        env.set(ocx_env::ACTIONS_ID_TOKEN_REQUEST_TOKEN.declaration(), "bearer");

        let provider = DispatchingTokenProvider::new(None, true, Vec::new());
        assert!(provider.has_signing_material(), "the Actions pair is set");
        let failure = provider
            .acquire("sigstore")
            .await
            .expect_err("the token endpoint answers 503");
        assert!(
            matches!(failure, SignErrorKind::OidcTokenUnavailable),
            "got: {failure:?}"
        );
        assert_eq!(
            ocx_exit::ClassifyExitCode::classify(&failure),
            Some(ocx_exit::ExitCode::TempFail)
        );
    }
}
