// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! SSRF-hardened URL validation for Sigstore endpoints: HTTPS only, plus `http` on loopback for local stacks.
//!
//! Shared by sign and verify (`adr_oci_referrers_signing_v1.md` Amendment 2): rejected is 64, unresolvable 69.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use url::Host;

/// Re-exported so callers can name what [`validate_sigstore_url`] returns without depending on `url`.
pub use url::Url;

/// Default public Rekor transparency-log endpoint.
pub const DEFAULT_REKOR_URL: &str = "https://rekor.sigstore.dev";

/// Connect timeout for Sigstore trust-services HTTP calls.
const SIGSTORE_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Overall request timeout for Sigstore trust-services HTTP calls.
const SIGSTORE_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Idle bound between Sigstore response frames, so a silent peer fails in seconds rather than after
/// `SIGSTORE_REQUEST_TIMEOUT`.
const SIGSTORE_READ_TIMEOUT: Duration = Duration::from_secs(15);

/// Idle connections kept per Sigstore host; reqwest's unbounded default accumulates in a process-wide client.
const SIGSTORE_MAX_IDLE_PER_HOST: usize = 2;

/// The one builder every Sigstore client is configured from; its arguments let a test build one without the
/// process-wide client.
fn sigstore_client_builder(
    read_timeout: Duration,
    rules: Arc<crate::ssrf::ProxyRules>,
    extra_roots: &ocx_util::tls::ExtraRoots,
) -> reqwest::ClientBuilder {
    // Bundled roots: reqwest's rustls path panics on a host with an empty OS trust store, and auto-verify
    // would carry that panic into installs.
    extra_roots
        .seed(ocx_util::tls::seed_embedded_roots(reqwest::Client::builder()))
        .connect_timeout(SIGSTORE_CONNECT_TIMEOUT)
        .timeout(SIGSTORE_REQUEST_TIMEOUT)
        .read_timeout(read_timeout)
        .pool_max_idle_per_host(SIGSTORE_MAX_IDLE_PER_HOST)
        .redirect(refuse_redirects())
        .dns_resolver(Arc::new(PinnedResolver { rules }))
}

/// Shared, process-wide HTTP client for Sigstore trust-services calls.
///
/// Every timeout bounded, or a stalled Fulcio or Rekor hangs verify and, through auto-verify, every covered install.
pub fn sigstore_http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        let roots = ocx_util::tls::sigstore_roots();
        match sigstore_client_builder(SIGSTORE_READ_TIMEOUT, crate::ssrf::proxy_rules(), roots).build() {
            Ok(client) => client,
            // Retry the same fully-bounded builder rather than degrade to a hand-rolled subset.
            Err(_) => sigstore_client_builder(SIGSTORE_READ_TIMEOUT, crate::ssrf::proxy_rules(), roots)
                .build()
                // Unreachable: `Client::new()` panics under the same TLS-init failure, so no unbounded client
                // is returned.
                .unwrap_or_else(|_| reqwest::Client::new()),
        }
    })
}

/// Refuse every HTTP redirect on the Sigstore client.
///
/// The SSRF guard checks the endpoint, not a `Location`: a hostile Fulcio's `307` would re-POST the OIDC token
/// to any host, metadata endpoints included (CWE-918).
fn refuse_redirects() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        attempt.error(
            "sigstore endpoint redirected; only the endpoint that passed the SSRF guard is dialed, so \
             redirects are refused — point the URL at the final host",
        )
    })
}

/// The text of a failed Sigstore request for a log line: the whole cause chain, the URL left out (an operator-supplied
/// endpoint may embed credentials).
///
/// A refused redirect's cause is the only place the "point the URL at the final host" remedy lives.
pub fn describe_send_failure(error: reqwest::Error) -> String {
    let error = error.without_url();
    let mut text = error.to_string();
    let mut source = std::error::Error::source(&error);
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

/// Ceiling on a Sigstore trust-service response body; neither the protocols nor `reqwest` bound one, and honest
/// answers are kilobytes.
pub const MAX_SIGSTORE_RESPONSE_BYTES: u64 = 1024 * 1024;

/// Why [`read_body_capped`] produced no body; the two answer different next actions (retry vs. a bad response).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyReadError {
    /// The body is over [`MAX_SIGSTORE_RESPONSE_BYTES`], declared or counted.
    Oversize,
    /// The stream broke before the body ended.
    Transport,
}

/// Read a Sigstore trust-service response body, refusing one above the cap.
///
/// The running total bounds a body whose `Content-Length` is absent or lies.
///
/// # Errors
///
/// [`BodyReadError::Oversize`] for an over-cap body, [`BodyReadError::Transport`] for a stream that broke mid-read.
pub async fn read_body_capped(response: reqwest::Response) -> Result<Vec<u8>, BodyReadError> {
    use futures::StreamExt as _;

    if let Some(declared) = response.content_length()
        && declared > MAX_SIGSTORE_RESPONSE_BYTES
    {
        return Err(BodyReadError::Oversize);
    }
    // Sized only after the cap refused an over-declared body, so a hostile Content-Length cannot drive the allocation.
    let hint = response.content_length().unwrap_or(0).min(MAX_SIGSTORE_RESPONSE_BYTES);
    let mut body = Vec::with_capacity(hint as usize);
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| BodyReadError::Transport)?;
        if body.len() as u64 + chunk.len() as u64 > MAX_SIGSTORE_RESPONSE_BYTES {
            return Err(BodyReadError::Oversize);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Addresses the SSRF guard approved, keyed by hostname: written by [`resolve_sigstore_url`], read by
/// [`PinnedResolver`].
static SIGSTORE_PINS: OnceLock<Mutex<HashMap<String, Vec<SocketAddr>>>> = OnceLock::new();

fn sigstore_pins() -> &'static Mutex<HashMap<String, Vec<SocketAddr>>> {
    SIGSTORE_PINS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Record the addresses the guard approved for `host`; last writer wins, as each write is a fresh verdict.
fn pin_sigstore_host(host: &str, addresses: Vec<SocketAddr>) {
    if addresses.is_empty() {
        return;
    }
    // Poison recovered: a panic elsewhere cannot unapprove an address, and refusing would fail every later dial.
    let mut pins = sigstore_pins().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    pins.insert(host.to_ascii_lowercase(), addresses);
}

/// Resolver for [`sigstore_http_client`] replaying the SSRF guard's pins, so reqwest never re-resolves a judged
/// name into a private range (CWE-918, DNS rebinding).
///
/// Fails closed on an unpinned host, except this process's own proxy, whose name is operator config.
struct PinnedResolver {
    /// Consulted only to recognise the proxy's own hostname.
    rules: Arc<crate::ssrf::ProxyRules>,
}

impl reqwest::dns::Resolve for PinnedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_ascii_lowercase();
        // Clone out before the async block: no lock is held across an await.
        let pinned = sigstore_pins()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&host)
            .cloned();
        let rules = Arc::clone(&self.rules);
        Box::pin(async move {
            // Pin first: a host that is both proxy and cleared endpoint would otherwise be re-resolved unjudged.
            match pinned {
                Some(addresses) => Ok(Box::new(addresses.into_iter()) as reqwest::dns::Addrs),
                None if rules.is_proxy_host(&host) => {
                    // Port 0: reqwest overrides it from the request URL after resolution.
                    let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
                    Ok(Box::new(addresses.into_iter()) as reqwest::dns::Addrs)
                }
                None => Err(format!(
                    "sigstore host {host} was never approved by the SSRF guard; only an endpoint \
                     the guard cleared, or this process's configured HTTP proxy, is dialed. A \
                     name arriving here is neither: it is a redirect target, or a second DNS \
                     answer for a name that was cleared (rebinding)"
                )
                .into()),
            }
        })
    }
}

/// Whether the URL names loopback in the string itself.
///
/// One function for both checks, or the boundary admits a URL the dial guard then refuses.
fn is_loopback_host(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(h)) => h == "localhost",
        Some(Host::Ipv4(addr)) => addr.is_loopback(),
        Some(Host::Ipv6(addr)) => addr.is_loopback(),
        None => false,
    }
}

/// Re-checks a validated endpoint against where it resolves, pinning the approved addresses for
/// `PinnedResolver`.
///
/// String-level [`validate_sigstore_url`] admits a name resolving into `169.254.169.254` or a private range
/// (CWE-918). Loopback is opt-in by the string only, never by resolution. Proxied, only a forbidden IP literal
/// or loopback name is refused and nothing is pinned.
///
/// # Errors
///
/// [`SsrfError::ForbiddenTarget`](crate::ssrf::SsrfError::ForbiddenTarget) for a forbidden target with no
/// `trusted_hosts` entry, [`SsrfError::Resolution`](crate::ssrf::SsrfError::Resolution) when a directly-routed
/// endpoint does not resolve.
pub async fn resolve_sigstore_url(url: &Url, trusted: &[String]) -> Result<(), crate::ssrf::SsrfError> {
    resolve_sigstore_url_with_rules(url, trusted, &crate::ssrf::proxy_rules()).await
}

/// [`resolve_sigstore_url`] with injected proxy rules, since a test cannot safely mutate `HTTPS_PROXY`.
async fn resolve_sigstore_url_with_rules(
    url: &Url,
    trusted: &[String],
    rules: &crate::ssrf::ProxyRules,
) -> Result<(), crate::ssrf::SsrfError> {
    // The parsed host, not `host_str()`, whose bracketed IPv6 would fail closed on every IPv6 endpoint.
    let host = match url.host() {
        Some(Host::Domain(domain)) => domain.to_string(),
        Some(Host::Ipv4(addr)) => addr.to_string(),
        Some(Host::Ipv6(addr)) => addr.to_string(),
        None => String::new(),
    };
    let port = url.port_or_known_default().unwrap_or(443);
    let opted_in;
    let trusted = if is_loopback_host(url) {
        opted_in = [trusted, std::slice::from_ref(&host)].concat();
        &opted_in
    } else {
        trusted
    };
    // The scheme picks the proxy; any unexpected scheme treated as `Https` still keeps the direct-route floor.
    let scheme = if url.scheme() == "http" {
        crate::ssrf::DialScheme::Http
    } else {
        crate::ssrf::DialScheme::Https
    };
    match crate::ssrf::guard_destination(scheme, &host, port, trusted, rules).await? {
        crate::ssrf::DialRoute::Direct(approved) => pin_sigstore_host(&host, approved),
        // Proxied: the guard approved no addresses, and pinning one would hand `PinnedResolver` a verdict no
        // guard made.
        crate::ssrf::DialRoute::Proxied => {}
    }
    Ok(())
}

/// Reason why a user-supplied Sigstore endpoint URL was rejected.
///
/// `reason` never carries raw input (CWE-209): a parse failure omits it, and a parsed URL is echoed only as a
/// [`RedactedUrl`](crate::RedactedUrl).
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
#[error("{reason}")]
#[exit(delegate = kind)]
pub struct UrlRejection {
    /// Short description of why the URL was rejected.
    pub reason: String,
    kind: UrlRejectionKind,
}

/// What kind of rejection a [`UrlRejection`] is; it alone decides the exit code and `error.detail` slug.
#[derive(Debug, Clone, Copy, thiserror::Error, ocx_exit::Classify)]
#[exit(family = "UrlRejection")]
enum UrlRejectionKind {
    /// The URL itself is unacceptable: malformed, wrong scheme, embedded credentials or a refused address.
    #[error("invalid endpoint URL")]
    #[exit(
        UsageError,
        slug = "invalid_endpoint_url",
        summary = "A Sigstore endpoint URL failed validation"
    )]
    Invalid,
    /// The endpoint host does not resolve: the flag was fine, the network was not.
    #[error("endpoint host does not resolve")]
    #[exit(
        Unavailable,
        slug = "endpoint_unresolvable",
        summary = "An endpoint host does not resolve"
    )]
    Unresolvable,
}

impl UrlRejection {
    /// The exit code this rejection classifies to: 64, or 69 for an endpoint that does not resolve.
    #[must_use]
    pub fn exit_code(&self) -> ocx_exit::ExitCode {
        // Neither kind defers, so `Failure` is unreachable.
        ocx_exit::ClassifyExitCode::classify(&self.kind).unwrap_or(ocx_exit::ExitCode::Failure)
    }
}

impl From<crate::ssrf::SsrfError> for UrlRejection {
    /// Carries an SSRF verdict through the `InvalidEndpointUrl` channel, keeping the CLI's variant and exit contract.
    fn from(error: crate::ssrf::SsrfError) -> Self {
        // Not `error.classify()`, the registry guard's table (forbidden = 78): a rejected Sigstore endpoint is 64.
        let kind = match error {
            crate::ssrf::SsrfError::Resolution { .. } => UrlRejectionKind::Unresolvable,
            // No wildcard, so a new variant is a compile error rather than a silent 64.
            crate::ssrf::SsrfError::ForbiddenTarget { .. } => UrlRejectionKind::Invalid,
        };
        Self {
            reason: error.to_string(),
            kind,
        }
    }
}

impl UrlRejection {
    /// Builds a bare rejection classifying to [`ExitCode::UsageError`](ocx_exit::ExitCode::UsageError).
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            kind: UrlRejectionKind::Invalid,
        }
    }
}

/// Validate a user-supplied Sigstore endpoint URL.
///
/// Accepts `https://`, and `http://` only on loopback hosts; rejects embedded credentials, other schemes and
/// unparseable input.
///
/// # Errors
///
/// A [`UrlRejection`] describing the violation, which callers wrap with the originating flag name.
pub fn validate_sigstore_url(raw: &str, _flag_name: &str) -> Result<Url, UrlRejection> {
    // Never echo `raw`: an unparseable input can still hold `user:password@` (CWE-209).
    let url = Url::parse(raw).map_err(|e| UrlRejection::new(format!("malformed URL: {e}")))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UrlRejection::new(format!(
            "URL must not embed credentials (sanitized: `{}`)",
            crate::RedactedUrl::from(url.clone())
        )));
    }
    let scheme = url.scheme();
    match (scheme, is_loopback_host(&url)) {
        ("https", _) => Ok(url),
        ("http", true) => Ok(url),
        ("http", false) => Err(UrlRejection::new(format!(
            "URL must use HTTPS (sanitized: `{}`); HTTP only accepted for loopback hosts",
            crate::RedactedUrl::from(url.clone())
        ))),
        (other, _) => Err(UrlRejection::new(format!(
            "URL must use HTTPS or HTTP on loopback (got scheme `{other}`)"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── The pin: the guard's verdict is what the client dials ──────────────

    /// The shipped client with `.no_proxy()`, so an ambient developer
    /// `HTTP_PROXY` cannot route the loopback fixtures below.
    fn hermetic_sigstore_client() -> reqwest::Client {
        sigstore_client_builder(
            SIGSTORE_READ_TIMEOUT,
            Arc::new(crate::ssrf::ProxyRules::new(
                hyper_util::client::proxy::matcher::Matcher::builder().build(),
            )),
            &ocx_util::tls::ExtraRoots::default(),
        )
        .no_proxy()
        .build()
        .expect("the shared builder produces a client")
    }

    /// A refused redirect is permanent, and the line a log carries still names the remedy.
    #[tokio::test]
    async fn a_refused_redirect_is_not_transient_and_describes_the_remedy() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let redirector = TcpListener::bind("127.0.0.1:0").await.expect("bind redirector");
        let addr = redirector.local_addr().expect("redirector address");
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = redirector.accept().await {
                let mut scratch = [0_u8; 1024];
                let _ = socket.read(&mut scratch).await;
                let _ = socket
                    .write_all(
                        b"HTTP/1.1 307 Temporary Redirect\r\nLocation: http://127.0.0.1:9/\r\nContent-Length: 0\r\n\r\n",
                    )
                    .await;
            }
        });

        let error = hermetic_sigstore_client()
            .get(format!("http://{addr}/api/v1/log/publicKey"))
            .send()
            .await
            .expect_err("a redirect is refused");
        assert!(!crate::transport_policy::is_transient_transport_error(&error));
        let text = describe_send_failure(error);
        assert!(text.contains("point the URL at the final host"), "{text}");
        assert!(!text.contains(&addr.to_string()), "the URL is left out: {text}");
    }

    /// Chain a `reqwest::Error` into one string, so an assertion sees the
    /// resolver's own message rather than reqwest's outer "error sending
    /// request" wrapper.
    fn error_chain(error: &dyn std::error::Error) -> String {
        let mut text = error.to_string();
        let mut source = error.source();
        while let Some(cause) = source {
            text.push_str(": ");
            text.push_str(&cause.to_string());
            source = cause.source();
        }
        text
    }

    /// The Sigstore builder is the pure seam — a
    /// set handed to `sigstore_client_builder` is what the built client trusts.
    /// One in-process TLS server presenting a leaf signed by a minted root, dialed
    /// by IP so the pinned resolver (names only) stays out of the picture; the
    /// seeded client gets a 200, the default-set client ends in `UnknownIssuer`.
    /// Both in one function per the Unchecked-Green rule.
    #[tokio::test]
    async fn extra_ca_sigstore_client_builder_trusts_the_seeded_root_and_only_then() {
        use ocx_test_support::pki::{TestPki, assert_untrusted_root, serve_https};

        let pki = TestPki::mint();
        let addr = serve_https(&pki).await;
        let url = format!("https://{addr}/api/v1/log/publicKey");
        let rules = || {
            Arc::new(crate::ssrf::ProxyRules::new(
                hyper_util::client::proxy::matcher::Matcher::builder().build(),
            ))
        };

        let seeded = sigstore_client_builder(
            Duration::from_secs(5),
            rules(),
            &ocx_util::tls::ExtraRoots::from_pem(pki.root_pem().as_bytes()).expect("a minted root parses"),
        )
        .no_proxy()
        .build()
        .expect("the seeded builder produces a client");
        let response = seeded
            .get(&url)
            .send()
            .await
            .expect("the seeded Sigstore client trusts the minted root");
        assert_eq!(response.status(), reqwest::StatusCode::OK);

        let error = hermetic_sigstore_client()
            .get(&url)
            .send()
            .await
            .expect_err("the default set does not know the minted root");
        let chain = error_chain(&error);
        assert_untrusted_root(&chain);
    }

    /// The SHIPPED client — `sigstore_http_client()`, the
    /// one every Fulcio/Rekor/TUF call site uses — consumes the roots
    /// `install_sigstore_roots` installed, not only the pure builder the seam
    /// test above feeds. Dialed by IP so the pinned resolver stays out of it.
    ///
    /// Process-global state twice over (`SIGSTORE_ROOTS`, the client
    /// `OnceLock`): this test installs, so under plain `cargo test` it
    /// collides with `tls`'s installer test and with any earlier
    /// caller of the shared client in the same process — nextest's one
    /// process per test is what makes it sound, as `context.rs`'s
    /// `try_init` tests already rely on. Observed-condition skip when the
    /// ambient environment proxies loopback: the shared client honours the
    /// process's proxy rules, and a proxied dial never reaches the fixture.
    ///
    /// Mutation: replace `sigstore_roots()` at the `get_or_init` with
    /// `&ExtraRoots::default()` — the dial ends in `UnknownIssuer` and this
    /// reds.
    #[tokio::test]
    async fn extra_ca_the_shared_sigstore_client_trusts_the_installed_roots() {
        use ocx_test_support::pki::{TestPki, error_chain, serve_https};

        let pki = TestPki::mint();
        let addr = serve_https(&pki).await;
        if !matches!(
            crate::ssrf::proxy_rules().dial_route(crate::ssrf::DialScheme::Https, "127.0.0.1", addr.port()),
            crate::ssrf::Route::Direct
        ) {
            eprintln!("skipped: the ambient proxy configuration routes loopback through a proxy");
            return;
        }
        ocx_util::tls::install_sigstore_roots(
            ocx_util::tls::ExtraRoots::from_pem(pki.root_pem().as_bytes()).expect("a minted root parses"),
        );
        let url = format!("https://{addr}/api/v1/log/publicKey");

        let response = sigstore_http_client()
            .get(&url)
            .send()
            .await
            .unwrap_or_else(|error| panic!("the shared client trusts the installed root: {}", error_chain(&error)));
        assert_eq!(response.status(), reqwest::StatusCode::OK);
    }

    /// A host no guard approved is never dialed.
    ///
    /// This is the DNS-rebinding regression test: the guard resolves and
    /// validates, and the shared client used to resolve the same name a second
    /// time for itself, so a name answering public to the guard and private to
    /// reqwest crossed the floor. With the pin wired, a name the guard did not
    /// approve does not resolve at all -- and a rebind is exactly an
    /// unapproved answer for an approved name.
    ///
    /// Discriminates: drop `.dns_resolver(...)` from `sigstore_http_client`
    /// and the failure becomes reqwest's own DNS error, which does not carry
    /// this text.
    ///
    /// `unguarded.invalid` must stay unpinned by every other test in this
    /// process -- the pin map is process-wide. A violation reds loudly rather
    /// than passing wrongly: a pinned host resolves, so `expect_err` panics.
    #[tokio::test]
    async fn the_shared_client_refuses_a_host_no_guard_approved() {
        let error = hermetic_sigstore_client()
            .get("http://unguarded.invalid:1/")
            .send()
            .await
            .expect_err("an unguarded host must not be dialed");
        let text = error_chain(&error);
        assert!(
            text.contains("never approved by the SSRF guard"),
            "expected the pin to refuse the dial, got: {text}"
        );
    }

    /// The pin replays the guard's addresses verbatim -- it is the same
    /// verdict applied at the dial, not a second resolution that could differ.
    #[tokio::test]
    async fn the_resolver_replays_exactly_the_addresses_the_guard_approved() {
        use reqwest::dns::Resolve as _;
        use std::str::FromStr as _;

        let approved: Vec<SocketAddr> = vec!["203.0.113.7:443".parse().expect("test address")];
        pin_sigstore_host("Pinned.Example", approved.clone());

        // Lookup is case-insensitive: the guard sees the URL's host, reqwest
        // lowercases before it asks.
        let name = reqwest::dns::Name::from_str("pinned.example").expect("test name");
        let rules = Arc::new(crate::ssrf::ProxyRules::new(
            hyper_util::client::proxy::matcher::Matcher::builder().build(),
        ));
        let resolved: Vec<SocketAddr> = PinnedResolver { rules }
            .resolve(name)
            .await
            .expect("pinned host resolves")
            .collect();
        assert_eq!(resolved, approved);
    }

    /// A pin outranks the proxy admission.
    ///
    /// The configured-proxy set is scheme-agnostic, so one name can be both
    /// this process's `HTTP_PROXY` and an `https` endpoint the guard routed
    /// direct and pinned. Consulting the pin first is what keeps the guard's
    /// verdict authoritative for that name: admitting it as a proxy instead
    /// would discard the pin and re-resolve it with no floor, which is exactly
    /// the rebinding window the pin exists to close.
    ///
    /// Discriminates: test `is_proxy_host` before the pin map and the resolver
    /// takes the lookup path, which cannot resolve a `.invalid` name and fails.
    #[tokio::test]
    async fn a_pinned_host_outranks_the_proxy_admission() {
        use reqwest::dns::Resolve as _;
        use std::str::FromStr as _;

        let approved: Vec<SocketAddr> = vec!["127.0.0.1:8443".parse().expect("test address")];
        pin_sigstore_host("pinned-and-proxy.invalid", approved.clone());

        let rules = Arc::new(crate::ssrf::ProxyRules::new(
            hyper_util::client::proxy::matcher::Matcher::builder()
                .all("http://pinned-and-proxy.invalid:3128")
                .build(),
        ));
        let name = reqwest::dns::Name::from_str("pinned-and-proxy.invalid").expect("test name");
        let resolved: Vec<SocketAddr> = PinnedResolver { rules }
            .resolve(name)
            .await
            .expect("a pinned host resolves by its pin, not by a lookup that cannot succeed")
            .collect();
        assert_eq!(
            resolved, approved,
            "the guard's own verdict must outrank the proxy admission"
        );
    }

    /// A loopback endpoint the operator typed is pinned by the guard, so the
    /// local-stack carve-out survives the resolver.
    #[tokio::test]
    async fn guarding_a_loopback_endpoint_pins_it_for_the_client() {
        let url = validate_sigstore_url("http://localhost:5555", "--rekor-url").expect("loopback URL is admitted");
        // Explicit empty rules, not the ambient environment: an `http`
        // endpoint is routed by the developer's own `HTTP_PROXY`, and a
        // proxied route pins nothing, so this would red on a proxied machine
        // for a reason that has nothing to do with the pin it asserts on.
        resolve_sigstore_url_with_rules(
            &url,
            &[],
            &crate::ssrf::ProxyRules::new(hyper_util::client::proxy::matcher::Matcher::builder().build()),
        )
        .await
        .expect("loopback is the operator's opt-in");

        use reqwest::dns::Resolve as _;
        use std::str::FromStr as _;
        let name = reqwest::dns::Name::from_str("localhost").expect("test name");
        let rules = Arc::new(crate::ssrf::ProxyRules::new(
            hyper_util::client::proxy::matcher::Matcher::builder().build(),
        ));
        let resolved: Vec<SocketAddr> = PinnedResolver { rules }
            .resolve(name)
            .await
            .expect("localhost is pinned")
            .collect();
        assert!(
            resolved.iter().all(|address| address.ip().is_loopback()),
            "the pin must carry only what the guard approved, got: {resolved:?}"
        );
    }

    // ── resolve_sigstore_url: the dial-time floor the string check cannot be ──
    //
    // Network-free: IP literals need no DNS, and `localhost` resolves locally.

    /// The whole point of the second check. `validate_sigstore_url` admits this
    /// URL -- it is `https` -- and the cloud metadata endpoint is exactly what a
    /// hostile `ocx.toml` or managed-config tier would point `rekor-url` at.
    #[tokio::test]
    async fn a_metadata_endpoint_passes_the_string_check_and_is_refused_at_dial_time() {
        let url = validate_sigstore_url("https://169.254.169.254/api/v1", "--rekor-url")
            .expect("the string check admits it -- that is the gap this closes");
        let error = resolve_sigstore_url(&url, &[])
            .await
            .expect_err("the link-local metadata endpoint must be refused");
        assert!(matches!(error, crate::ssrf::SsrfError::ForbiddenTarget { .. }));
    }

    /// A local stack is admitted because the *string* says loopback -- the
    /// operator typed it. This is the carve-out, and it is why the guard cannot
    /// be a resolver hook on the shared client.
    #[tokio::test]
    async fn an_explicitly_named_local_stack_is_admitted() {
        for raw in ["http://127.0.0.1:5555", "http://localhost:3000", "http://[::1]:3000"] {
            let url = validate_sigstore_url(raw, "--fulcio-url").expect("loopback string accepted");
            // Empty rules: the carve-out under test is the direct route's.
            resolve_sigstore_url_with_rules(
                &url,
                &[],
                &crate::ssrf::ProxyRules::new(hyper_util::client::proxy::matcher::Matcher::builder().build()),
            )
            .await
            .unwrap_or_else(|e| panic!("an opted-in local stack must be admitted: {raw}: {e}"));
        }
    }

    /// The carve-out is keyed on the *literal* host, so a host that is not
    /// spelled as loopback is judged purely by where it resolves -- which is
    /// what leaves no room for a rebind to borrow the local-stack pass. Pinned
    /// on the predicate itself, since fabricating a real rebind needs DNS.
    #[test]
    fn the_local_stack_carve_out_is_keyed_on_the_literal_host() {
        for spelled in ["http://127.0.0.1:5555", "http://localhost:3000", "http://[::1]:3000"] {
            assert!(is_loopback_host(&Url::parse(spelled).expect("url")), "{spelled}");
        }
        for not_spelled in [
            "https://rekor.sigstore.dev",
            "https://localhost.evil.test",
            "https://8.8.8.8",
        ] {
            assert!(
                !is_loopback_host(&Url::parse(not_spelled).expect("url")),
                "{not_spelled} must get no local-stack pass"
            );
        }
    }

    /// An operator running Sigstore on a private address configures it in the
    /// same `trusted_hosts` list the registry guard reads -- not a second key.
    #[tokio::test]
    async fn a_trusted_hosts_entry_admits_a_private_sigstore_deployment() {
        let url = validate_sigstore_url("https://10.1.2.3:5555", "--fulcio-url").expect("https accepted");
        let error = resolve_sigstore_url(&url, &[])
            .await
            .expect_err("RFC1918 is refused by default");
        assert!(matches!(error, crate::ssrf::SsrfError::ForbiddenTarget { .. }));

        resolve_sigstore_url(&url, &["10.0.0.0/8".to_string()])
            .await
            .expect("a CIDR trusted_hosts entry admits it");
    }

    /// A public endpoint is untouched by any of this.
    #[tokio::test]
    async fn a_public_endpoint_is_admitted() {
        let url = validate_sigstore_url("https://8.8.8.8", "--rekor-url").expect("https accepted");
        // Empty rules: the direct route is the one that has a floor to pass.
        resolve_sigstore_url_with_rules(
            &url,
            &[],
            &crate::ssrf::ProxyRules::new(hyper_util::client::proxy::matcher::Matcher::builder().build()),
        )
        .await
        .expect("a public address passes");
    }

    fn unwrap_err(result: Result<Url, UrlRejection>) -> UrlRejection {
        result.expect_err("expected validation failure")
    }

    #[test]
    fn https_production_url_accepted() {
        let url = validate_sigstore_url("https://fulcio.sigstore.dev", "--fulcio-url").expect("https accepted");
        assert_eq!(url.scheme(), "https");
    }

    #[test]
    fn https_with_path_accepted() {
        let url = validate_sigstore_url("https://rekor.sigstore.dev/api/v1", "--rekor-url")
            .expect("https with path accepted");
        assert_eq!(url.scheme(), "https");
    }

    #[test]
    fn http_loopback_ipv4_accepted() {
        let url = validate_sigstore_url("http://127.0.0.1:5432", "--fulcio-url").expect("loopback ipv4 accepted");
        assert_eq!(url.scheme(), "http");
        assert_eq!(url.host_str(), Some("127.0.0.1"));
    }

    #[test]
    fn http_loopback_ipv4_range_accepted() {
        // Entire 127.0.0.0/8 is loopback per RFC 5735 — any address in that
        // range is routed to loopback without touching the network, so the
        // SSRF carve-out must cover the full subnet, not just 127.0.0.1.
        let url = validate_sigstore_url("http://127.0.0.2:5432", "--fulcio-url")
            .expect("127.0.0.0/8 loopback range accepted");
        assert_eq!(url.host_str(), Some("127.0.0.2"));
    }

    #[test]
    fn uppercase_https_scheme_accepted() {
        // `url::Url::parse` normalizes scheme to lowercase, so HTTPS:// is
        // accepted identically to https:// — lock that behavior here.
        let url = validate_sigstore_url("HTTPS://fulcio.sigstore.dev", "--fulcio-url")
            .expect("uppercase HTTPS must be accepted after scheme normalization");
        assert_eq!(url.scheme(), "https");
    }

    #[test]
    fn url_with_userinfo_rejected() {
        let rejection = unwrap_err(validate_sigstore_url(
            "https://user:pass@fulcio.sigstore.dev",
            "--fulcio-url",
        ));
        assert!(rejection.reason.contains("credentials"));
    }

    #[test]
    fn url_with_username_only_rejected() {
        let rejection = unwrap_err(validate_sigstore_url(
            "https://user@fulcio.sigstore.dev",
            "--fulcio-url",
        ));
        assert!(rejection.reason.contains("credentials"));
    }

    #[test]
    fn http_localhost_accepted() {
        let url = validate_sigstore_url("http://localhost:5432/path", "--rekor-url").expect("localhost accepted");
        assert_eq!(url.scheme(), "http");
        assert_eq!(url.host_str(), Some("localhost"));
    }

    #[test]
    fn http_loopback_ipv6_accepted() {
        // [::1] is the IPv6 loopback; valid for test fixtures.
        let url = validate_sigstore_url("http://[::1]:9000", "--rekor-url").expect("ipv6 loopback accepted");
        assert_eq!(url.scheme(), "http");
    }

    #[test]
    fn http_ipv4_mapped_ipv6_rejected() {
        // `::ffff:127.0.0.1` routes to loopback at the OS level on Linux, but
        // `std::net::Ipv6Addr::is_loopback()` returns `false` — only `::1`
        // qualifies. Confirm that SSRF-relevant inputs using the IPv4-mapped
        // form are rejected, locking in the conservative policy.
        let rejection = unwrap_err(validate_sigstore_url("http://[::ffff:127.0.0.1]:8080", "--fulcio-url"));
        assert!(rejection.reason.contains("HTTPS"));
    }

    #[test]
    fn http_non_loopback_rejected() {
        let rejection = unwrap_err(validate_sigstore_url("http://example.com/fulcio", "--fulcio-url"));
        assert!(rejection.reason.contains("HTTPS"));
    }

    #[test]
    fn file_scheme_rejected() {
        let rejection = unwrap_err(validate_sigstore_url("file:///etc/passwd", "--rekor-url"));
        assert!(rejection.reason.contains("file"));
    }

    #[test]
    fn ftp_scheme_rejected() {
        let rejection = unwrap_err(validate_sigstore_url("ftp://example.com/bundle", "--rekor-url"));
        assert!(rejection.reason.contains("ftp"));
    }

    #[test]
    fn malformed_url_rejected() {
        let _rejection = unwrap_err(validate_sigstore_url("not a url at all", "--fulcio-url"));
        // UrlRejection is returned — just confirming it's a Err
    }

    #[test]
    fn empty_url_rejected() {
        let _rejection = unwrap_err(validate_sigstore_url("", "--fulcio-url"));
        // UrlRejection is returned — just confirming it's a Err
    }

    #[test]
    fn http_non_loopback_with_percent_encoded_credentials_caught_before_url_echo() {
        // CWE-209 regression: url::Url decodes percent-encoded userinfo, so
        // http://user%3Apass@example.com decodes to username="user:pass" (non-empty).
        // The credential check must fire BEFORE the scheme branch's URL echo.
        let rejection = validate_sigstore_url("http://user%3Apass@example.com/fulcio", "--fulcio-url").unwrap_err();
        assert!(
            rejection.reason.contains("credentials") || rejection.reason.contains("userinfo"),
            "expected credential/userinfo rejection, got: {}",
            rejection.reason
        );
        assert!(
            !rejection.reason.contains("user%3Apass"),
            "percent-encoded credentials leaked: {}",
            rejection.reason
        );
    }

    #[test]
    fn parse_error_text_must_not_echo_credentials() {
        // Regression guard for CWE-209: an unparseable URL whose raw form
        // contains `user:password@host` would previously have its credentials
        // formatted verbatim into the parse-error message because the
        // post-parse userinfo scrubber never ran. The fix omits `raw` from
        // the parse-failure branch entirely; this test locks in that
        // contract so a future "add the URL back for debuggability" change
        // re-introduces the leak only by explicitly deleting this test.
        let bad = "https://user:secret_pass@fulcio.invalid:99999/";
        let rejection = unwrap_err(validate_sigstore_url(bad, "--fulcio-url"));
        let text = format!("{rejection}");
        assert!(!text.contains("secret_pass"), "credentials leaked into error: {text}");
        assert!(!text.contains("user:"), "userinfo leaked: {text}");
    }

    #[test]
    fn rejected_url_echo_must_not_carry_query_or_fragment() {
        // ERR-17: a `[trust.sigstore]` endpoint is operator config, and an
        // operator's URL carries whatever the operator put in it — a bearer
        // token in the query string is the live case. The scheme rejection
        // echoes the URL back so the operator can see which entry was refused,
        // so that echo is scrubbed the same way the credentials branch is:
        // userinfo, query and fragment cleared, host and path kept.
        //
        // Discriminates: echo `raw` (or a scrub that stops at userinfo) and
        // `token=hush` reappears in stderr and in the JSON envelope message.
        let rejection = unwrap_err(validate_sigstore_url(
            "http://203.0.113.7/?token=hush#frag",
            "--fulcio-url",
        ));
        let text = format!("{rejection}");
        assert!(!text.contains("hush"), "query value leaked: {text}");
        assert!(!text.contains("token="), "query key leaked: {text}");
        assert!(!text.contains("#frag"), "fragment leaked: {text}");
        assert!(
            text.contains("203.0.113.7"),
            "the refused host must still be named: {text}"
        );
    }

    /// The second-dial gap. [`resolve_sigstore_url`] clears the endpoint the
    /// caller named; it cannot clear where a *response* points. Under reqwest's
    /// default policy a `307` from Fulcio re-issues the certificate POST — OIDC
    /// token in the body — at whatever host the `Location` names. Two real
    /// listeners, so the assertion is on what the second one received rather
    /// than on how the client happens to be configured: delete
    /// `.redirect(refuse_redirects())` and this reds on the reached flag.
    #[tokio::test]
    async fn the_shared_client_refuses_a_redirect_instead_of_dialing_the_new_host() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let target = TcpListener::bind("127.0.0.1:0").await.expect("bind redirect target");
        let target_addr = target.local_addr().expect("redirect target address");
        let reached = Arc::new(AtomicBool::new(false));
        let reached_by_client = Arc::clone(&reached);
        tokio::spawn(async move {
            if let Ok((mut socket, _)) = target.accept().await {
                reached_by_client.store(true, Ordering::SeqCst);
                let mut scratch = [0_u8; 1024];
                let _ = socket.read(&mut scratch).await;
                let _ = socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
            }
        });

        let redirector = TcpListener::bind("127.0.0.1:0").await.expect("bind redirector");
        let redirector_addr = redirector.local_addr().expect("redirector address");
        tokio::spawn(async move {
            if let Ok((mut socket, _)) = redirector.accept().await {
                let mut scratch = [0_u8; 1024];
                let _ = socket.read(&mut scratch).await;
                let response = format!(
                    "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://{target_addr}/api/v2/log/entries\r\n\
                     Content-Length: 0\r\n\r\n"
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });

        let error = hermetic_sigstore_client()
            .post(format!("http://{redirector_addr}/api/v1/signingCert"))
            .body(r#"{"credentials":{"oidcIdentityToken":"secret"}}"#)
            .send()
            .await
            .expect_err("a redirected sigstore call must fail rather than follow the Location header");
        assert!(error.is_redirect(), "expected a redirect refusal, got: {error}");
        assert!(
            !reached.load(Ordering::SeqCst),
            "the redirect target was dialed -- the request, and the OIDC token in its body, followed the \
             Location header past the SSRF guard"
        );
    }

    /// A trust service can answer a two-kilobyte request with as much as it
    /// likes, and neither the Fulcio nor the Rekor protocol bounds it. The
    /// body here declares no `Content-Length` and never stops, so only the
    /// running total can refuse it: swap [`read_body_capped`] back for
    /// `response.bytes()` and this test buffers until the machine gives up.
    #[tokio::test]
    async fn an_undeclared_oversize_trust_service_body_is_refused_while_it_is_read() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind flooding endpoint");
        let addr = listener.local_addr().expect("flooding endpoint address");
        tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut scratch = [0_u8; 1024];
                let _ = socket.read(&mut scratch).await;
                // Chunked, so the length is never declared up front.
                let _ = socket
                    .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
                    .await;
                // A complete, well-formed 2 MiB body: over the cap, but the
                // terminator is written, so with the cap raised the read
                // succeeds and this test reds. Without it, an unterminated
                // stream refuses for a transport reason at any cap, and the
                // test would pass whether or not a cap exists at all.
                let chunk = vec![b'x'; 64 * 1024];
                let header = format!("{:x}\r\n", chunk.len());
                for _ in 0..32 {
                    // Writes fail once the capped reader hangs up; that is the
                    // pass condition, not an error.
                    if socket.write_all(header.as_bytes()).await.is_err()
                        || socket.write_all(&chunk).await.is_err()
                        || socket.write_all(b"\r\n").await.is_err()
                    {
                        return;
                    }
                }
                let _ = socket.write_all(b"0\r\n\r\n").await;
            }
        });

        let response = hermetic_sigstore_client()
            .get(format!("http://{addr}/api/v1/log/entries"))
            .send()
            .await
            .expect("the flooding endpoint answers");
        assert!(
            read_body_capped(response).await == Err(BodyReadError::Oversize),
            "an unbounded trust-service body was read into memory instead of being refused"
        );
    }

    /// A trust service that accepts the connection and then says nothing is
    /// the shape `timeout()` alone answers badly: it is armed once at dispatch,
    /// so a peer that goes quiet holds the call for the whole 30 s budget --
    /// and on the auto-verify path, holds an install with it.
    ///
    /// Built from the shipped [`sigstore_client_builder`] with a short read
    /// timeout, because nothing on `reqwest::Client` exposes its configured
    /// timeouts and waiting out the production 15 s is not a unit test. The
    /// harness bound is what discriminates: drop `.read_timeout(...)` from the
    /// builder and the request hangs past it instead of failing.
    #[tokio::test]
    async fn a_silent_trust_service_is_abandoned_rather_than_awaited_forever() {
        use tokio::io::AsyncReadExt;
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind silent endpoint");
        let addr = listener.local_addr().expect("silent endpoint address");
        tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut scratch = [0_u8; 1024];
                let _ = socket.read(&mut scratch).await;
                // Never answers, and holds the socket open so the client sees
                // silence rather than a close.
                std::future::pending::<()>().await;
            }
        });

        let client = sigstore_client_builder(
            Duration::from_millis(300),
            Arc::new(crate::ssrf::ProxyRules::new(
                hyper_util::client::proxy::matcher::Matcher::builder().build(),
            )),
            &ocx_util::tls::ExtraRoots::default(),
        )
        .no_proxy()
        .build()
        .expect("the shared builder produces a client");
        let outcome = tokio::time::timeout(
            Duration::from_secs(5),
            client.get(format!("http://{addr}/api/v1/log/entries")).send(),
        )
        .await
        .expect("the read timeout must fire long before this bound -- an unbounded read hangs here");
        assert!(
            outcome.is_err(),
            "a silent trust service answered successfully, which the listener never does"
        );
    }

    /// A stream that dies mid-body is a transport fault, not an over-cap body: the caller retries the first and
    /// reports the second. The server promises 100 bytes, sends 10, and hangs up.
    #[tokio::test]
    async fn a_body_stream_that_breaks_mid_read_is_a_transport_error() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind truncating endpoint");
        let addr = listener.local_addr().expect("truncating endpoint address");
        tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut scratch = [0_u8; 1024];
                let _ = socket.read(&mut scratch).await;
                let _ = socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n0123456789")
                    .await;
                // Dropping the socket closes it short of the declared length.
            }
        });

        let response = hermetic_sigstore_client()
            .get(format!("http://{addr}/api/v1/log/publicKey"))
            .send()
            .await
            .expect("the endpoint answers its headers");
        assert_eq!(
            read_body_capped(response).await,
            Err(BodyReadError::Transport),
            "a mid-stream break must classify as transport, not as an oversize body"
        );
    }

    /// The other half: an honest response still comes back whole, so the cap
    /// cannot be satisfied by refusing everything.
    #[tokio::test]
    async fn an_ordinary_trust_service_body_is_returned_whole() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind endpoint");
        let addr = listener.local_addr().expect("endpoint address");
        tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut scratch = [0_u8; 1024];
                let _ = socket.read(&mut scratch).await;
                let _ = socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 14\r\n\r\n{\"logIndex\":1}")
                    .await;
            }
        });

        let response = hermetic_sigstore_client()
            .get(format!("http://{addr}/api/v1/log/publicKey"))
            .send()
            .await
            .expect("the endpoint answers");
        assert_eq!(
            read_body_capped(response).await.as_deref(),
            Ok(&b"{\"logIndex\":1}"[..]),
            "an under-cap body must be returned unchanged"
        );
    }

    // ── The proxy route: the process dials the proxy, not the endpoint ─────

    /// An operator whose only egress is an HTTP proxy named by *hostname* can
    /// sign and verify.
    ///
    /// Under a proxy the connector resolves and dials the proxy; the Sigstore
    /// endpoint is literal text in the absolute-form request line, so the guard
    /// never resolves it and never pins it. [`PinnedResolver`] is asked for the
    /// proxy's own hostname instead -- a name no guard was ever given -- and
    /// before the admission it refused that name, which failed every Fulcio,
    /// Rekor and OIDC call on such a network.
    ///
    /// Two real listeners, so the assertion is on what each one received rather
    /// than on how the client is configured: the proxy must see the endpoint
    /// spelled out in the request line, and the endpoint itself must never be
    /// dialed by this process.
    ///
    /// Discriminates: drop the proxy-host admission from [`PinnedResolver`] and
    /// the send fails with `never approved by the SSRF guard`, naming
    /// `localhost` -- the proxy the operator configured, not an endpoint.
    ///
    /// Hermetic: rules come from an explicit `Matcher`, never the ambient
    /// environment, and the client is built here rather than taken from the
    /// process-wide [`sigstore_http_client`]. One shared-state caveat remains
    /// and is why this wants a per-test process (`cargo nextest`, the project
    /// runner): the pin map is process-wide, so a sibling test that pins
    /// `localhost` would let the dial succeed by the pin rather than by the
    /// admission. That weakens the red proof under a single-process
    /// `cargo test` run; it never makes this test fail.
    #[tokio::test]
    async fn a_hostname_configured_proxy_is_dialed_instead_of_being_refused_as_unguarded() {
        use std::sync::atomic::{AtomicBool, Ordering};

        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let target = TcpListener::bind("127.0.0.1:0").await.expect("bind sigstore endpoint");
        let target_addr = target.local_addr().expect("sigstore endpoint address");
        let dialed_directly = Arc::new(AtomicBool::new(false));
        let dialed_by_client = Arc::clone(&dialed_directly);
        tokio::spawn(async move {
            if let Ok((mut socket, _)) = target.accept().await {
                dialed_by_client.store(true, Ordering::SeqCst);
                let mut scratch = [0_u8; 1024];
                let _ = socket.read(&mut scratch).await;
                let _ = socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
            }
        });

        let proxy = TcpListener::bind("127.0.0.1:0").await.expect("bind forward proxy");
        let proxy_addr = proxy.local_addr().expect("forward proxy address");
        let (request_line_tx, request_line_rx) = tokio::sync::oneshot::channel::<String>();
        tokio::spawn(async move {
            if let Ok((mut socket, _)) = proxy.accept().await {
                let mut scratch = [0_u8; 2048];
                let read = socket.read(&mut scratch).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&scratch[..read]).into_owned();
                // Reported before the response, so the client cannot return
                // from `send()` before the request line is on the channel.
                let _ = request_line_tx.send(request.lines().next().unwrap_or_default().to_string());
                let _ = socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 14\r\n\r\n{\"logIndex\":1}")
                    .await;
            }
        });

        // Hostname form on purpose: an IP-literal proxy skips the resolver hook
        // entirely, so it would prove nothing about the refusal this closes.
        let proxy_url = format!("http://localhost:{}", proxy_addr.port());
        let rules = Arc::new(crate::ssrf::ProxyRules::new(
            hyper_util::client::proxy::matcher::Matcher::builder()
                .all(proxy_url.clone())
                .build(),
        ));
        let client = sigstore_client_builder(Duration::from_secs(5), rules, &ocx_util::tls::ExtraRoots::default())
            .proxy(reqwest::Proxy::all(&proxy_url).expect("the proxy URL is well-formed"))
            .build()
            .expect("the shared builder produces a client");

        let response = client
            .get(format!("http://{target_addr}/api/v1/log/publicKey"))
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "a hostname-configured proxy must be dialed, not refused as unguarded: {}",
                    error_chain(&error)
                )
            });
        assert!(
            response.status().is_success(),
            "the proxy answered {}, which the fixture never does",
            response.status()
        );

        let request_line = tokio::time::timeout(Duration::from_secs(5), request_line_rx)
            .await
            .expect("the proxy must receive the request")
            .expect("the proxy fixture reports its request line");
        assert_eq!(
            request_line,
            format!("GET http://{target_addr}/api/v1/log/publicKey HTTP/1.1"),
            "the endpoint must travel as absolute-form text through the proxy"
        );
        assert!(
            !dialed_directly.load(Ordering::SeqCst),
            "the endpoint was dialed by this process -- the proxy configuration was bypassed"
        );
    }

    /// The same admission at the resolver, where it is decidable without a
    /// listener -- and without the process-wide pin map.
    ///
    /// The end-to-end test above needs a proxy hostname that resolves to
    /// loopback, which in practice means `localhost`, and `localhost` is a name
    /// other tests pin. This one names a proxy that resolves nowhere, so no pin
    /// can ever satisfy it: what is asserted is only that a configured proxy
    /// host is *judged* as one. An admitted name that does not resolve fails
    /// with a lookup error; the bug is failing with the guard's refusal, which
    /// blames a host the operator configured on purpose.
    ///
    /// Discriminates: drop the proxy-host admission and the verdict is the
    /// `never approved by the SSRF guard` refusal verbatim.
    #[tokio::test]
    async fn the_pinned_resolver_admits_a_configured_proxy_host_rather_than_refusing_it_as_unguarded() {
        use reqwest::dns::Resolve as _;
        use std::str::FromStr as _;

        let rules = Arc::new(crate::ssrf::ProxyRules::new(
            hyper_util::client::proxy::matcher::Matcher::builder()
                .all("http://ocx-proxy.invalid:3128")
                .build(),
        ));
        let name = reqwest::dns::Name::from_str("ocx-proxy.invalid").expect("test name");

        let resolver = PinnedResolver { rules };
        let verdict = match resolver.resolve(name).await {
            Ok(addresses) => format!("resolved to {:?}", addresses.collect::<Vec<_>>()),
            Err(error) => error.to_string(),
        };
        assert!(
            !verdict.contains("never approved by the SSRF guard"),
            "the configured proxy host was refused as unguarded: {verdict}"
        );
    }

    /// A proxied Sigstore endpoint is admitted without a lookup, and pins
    /// nothing.
    ///
    /// On a proxied route the process resolves only the proxy, so an endpoint
    /// name that this host cannot resolve is not a refusal -- it is the proxy's
    /// to resolve. Pinning is the matching half: there are no approved
    /// addresses, so nothing may be recorded for [`PinnedResolver`] to replay.
    ///
    /// Discriminates: keep the direct-route lookup on a proxied route and the
    /// call fails with `failed to resolve host`; pin unconditionally and the
    /// map carries an entry the guard never approved.
    #[tokio::test]
    async fn a_proxied_sigstore_endpoint_is_admitted_without_being_pinned() {
        let url = validate_sigstore_url("https://fulcio.proxied-only.invalid", "--fulcio-url")
            .expect("the string check admits an https endpoint");
        let rules = crate::ssrf::ProxyRules::new(
            hyper_util::client::proxy::matcher::Matcher::builder()
                .all("http://localhost:1")
                .build(),
        );

        resolve_sigstore_url_with_rules(&url, &[], &rules)
            .await
            .expect("a proxied endpoint is the proxy's to resolve, so there is no lookup to fail");

        let pinned = sigstore_pins()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains_key("fulcio.proxied-only.invalid");
        assert!(!pinned, "a proxied route approved no addresses, so it must pin none");
    }
}
