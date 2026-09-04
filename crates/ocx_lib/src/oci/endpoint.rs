// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! SSRF-hardened URL validation for Sigstore endpoints.
//!
//! User-supplied `--fulcio-url` / `--rekor-url` flags become HTTP client
//! targets; unrestricted input would enable SSRF (CWE-918). Slice 1 policy
//! is HTTPS-only in production, with an explicit loopback carve-out so
//! integration tests -- and an operator running Sigstore on the same host --
//! can point at a local stack (`http://127.0.0.1:PORT/...`).
//!
//! Lives at `oci::endpoint` (a peer of `oci::sign` and `oci::verify`, per ADR
//! `adr_oci_referrers_signing_v1.md` Amendment 2) so both pipelines share one
//! validator without verify depending on sign. Any future library consumer
//! (mirror tool, SDK, Bazel rule) routes through the same guard before it
//! reaches an HTTP client. The function returns a [`UrlRejection`] on failure,
//! which each caller wraps into their own `InvalidEndpointUrl` variant to
//! attach the originating flag name. The exit verdict itself originates here
//! and is carried on the rejection: a rejected URL is a usage error (64), an
//! endpoint that does not resolve is unavailable (69), and the sign and verify
//! wraps read that answer rather than each minting one.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use url::Host;

/// Re-exported so a caller can name what [`validate_sigstore_url`] returns
/// without taking a direct `url` dependency of its own.
///
/// `ocx_cli` deliberately does not depend on `url`: a validated endpoint is a
/// `let` binding threaded from here to the options struct, never a named type
/// in a CLI signature. That works right up to the first CLI helper that
/// *returns* one, which is what this re-export is for.
pub use url::Url;

/// Default public Rekor transparency-log endpoint.
///
/// Shared by `ocx package sign` / `ocx package verify` (as the `--rekor-url`
/// clap default) and the policy-gated auto-verify hook, so the one public-Rekor
/// literal lives in a single place. Overridable per-invocation via `--rekor-url`.
pub const DEFAULT_REKOR_URL: &str = "https://rekor.sigstore.dev";

/// Connect timeout for Sigstore trust-services HTTP calls (Fulcio, Rekor,
/// ambient OIDC token exchange).
const SIGSTORE_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Overall request timeout for Sigstore trust-services HTTP calls.
const SIGSTORE_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Idle bound on a Sigstore trust-service response, mapped to
/// [`reqwest::ClientBuilder::read_timeout`].
///
/// `SIGSTORE_REQUEST_TIMEOUT` is armed once at dispatch and bounds the whole
/// call; it says nothing about a connection that goes quiet. reqwest resets
/// this one per response-body frame, so a peer that stops answering fails in
/// seconds instead of waiting out the full request budget. Fulcio, Rekor and an
/// OIDC token endpoint each answer in a frame or two, so fifteen seconds of
/// silence means the peer is gone -- and it stays comfortably under the 30 s
/// ceiling, so this never truncates an honest call the request timeout allows.
const SIGSTORE_READ_TIMEOUT: Duration = Duration::from_secs(15);

/// Idle connections reqwest keeps per Sigstore host.
///
/// The default is `usize::MAX`. A sign or verify run talks to at most Fulcio,
/// Rekor and one OIDC endpoint, one request at a time each, so two spare
/// sockets per host is already slack -- and this client is process-wide and
/// long-lived, which is exactly where an unbounded idle pool accumulates.
const SIGSTORE_MAX_IDLE_PER_HOST: usize = 2;

/// The one builder every Sigstore client is configured from.
///
/// Extracted so the timeout wiring has a seam a test can build against with a
/// short `read_timeout`: nothing on `reqwest::Client` exposes its configured
/// timeouts, so proving the bound exists means exercising it, and exercising
/// the shipped 15 s value is not a unit test.
fn sigstore_client_builder(read_timeout: Duration, rules: Arc<crate::oci::ssrf::ProxyRules>) -> reqwest::ClientBuilder {
    // Bundled roots, exactly as `forge::github` and `oci::index::ocx_index`
    // do it: reqwest's rustls path falls back to the OS trust store and
    // panics where that store is empty (minimal container, CI runner with
    // no ca-certificates). Without this, `ocx install` keeps working -- the
    // `oci_client` transport seeds its own roots in the fork -- while
    // `ocx package verify` panics on the same host, and auto-verify carries
    // that panic into every covered install.
    crate::utility::tls::seed_embedded_roots(reqwest::Client::builder())
        .connect_timeout(SIGSTORE_CONNECT_TIMEOUT)
        .timeout(SIGSTORE_REQUEST_TIMEOUT)
        .read_timeout(read_timeout)
        .pool_max_idle_per_host(SIGSTORE_MAX_IDLE_PER_HOST)
        .redirect(refuse_redirects())
        .dns_resolver(Arc::new(PinnedResolver { rules }))
}

/// Shared HTTP client for Sigstore trust-services calls.
///
/// `reqwest::Client::new()` carries no default timeout, so a stalled Fulcio or
/// Rekor endpoint hangs verify forever — and via the policy-gated auto-verify
/// hook, hangs every covered install, turning the fail-closed gate into
/// fail-hung. A single process-wide client with bounded connect, request and
/// per-frame read timeouts closes that, and its internal connection pool -- cap
/// on idle sockets included, since the default is `usize::MAX` -- is reused
/// across the sign and verify call sites instead of rebuilt per request.
///
/// Lives at `oci::endpoint` (a peer of `oci::sign`/`oci::verify`) so both
/// pipelines share one HTTP seam without verify depending on sign.
pub fn sigstore_http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        match sigstore_client_builder(SIGSTORE_READ_TIMEOUT, crate::oci::ssrf::proxy_rules()).build() {
            Ok(client) => client,
            // Only a TLS-backend init failure reaches here, and it fails every
            // HTTPS request that follows anyway. Retry the same fully-bounded
            // builder rather than a hand-rolled subset, so no path can hand
            // out a client missing the timeouts, the redirect refusal or the
            // pinned resolver. The bare-client terminal is unreachable in
            // practice: the retry fails identically, and `Client::new()`
            // panics under the same TLS-init failure.
            Err(_) => sigstore_client_builder(SIGSTORE_READ_TIMEOUT, crate::oci::ssrf::proxy_rules())
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
        }
    })
}

/// Refuse every HTTP redirect on the Sigstore client.
///
/// [`resolve_sigstore_url`] validates the endpoint the caller asked for. It
/// cannot validate where a *response* points: reqwest's default policy follows
/// up to ten redirects, so a `307` from a hostile — or merely compromised —
/// Fulcio would re-issue the certificate POST, OIDC token in the body, at
/// whatever host the `Location` names, including `169.254.169.254` and any
/// private service (CWE-918, and a credential replay on top). The Rekor upload
/// and the ambient-token exchange have the same second-dial shape.
///
/// Refusing outright rather than re-validating per hop is the smaller
/// mechanism, and it costs nothing real: neither the public Sigstore
/// deployment nor a self-hosted Fulcio/Rekor redirects its API endpoints. An
/// operator who fronts one with a redirecting proxy points `--fulcio-url` /
/// `--rekor-url` at the final host instead, and the error says so.
fn refuse_redirects() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        attempt.error(
            "sigstore endpoint redirected; only the endpoint that passed the SSRF guard is dialed, so \
             redirects are refused — point the URL at the final host",
        )
    })
}

/// Ceiling on a Sigstore trust-service response body.
///
/// Fulcio returns a certificate chain, Rekor a log entry, an ambient provider a
/// JWT — all kilobytes. Nothing in those protocols bounds the length, and
/// `reqwest` imposes no limit of its own, so a compromised or hostile endpoint
/// (a self-hosted stack, or a `--fulcio-url` an attacker chose) answers a
/// two-kilobyte request with as many gigabytes as the process will hold.
/// One megabyte is three orders of magnitude above every honest response.
pub(crate) const MAX_SIGSTORE_RESPONSE_BYTES: u64 = 1024 * 1024;

/// Read a Sigstore trust-service response body, refusing one above the cap.
///
/// Returns `None` for a transport failure and for an over-cap body alike: no
/// caller distinguishes them, and every one already has a single error variant
/// for "this endpoint did not answer usefully". The declared `Content-Length`
/// short-circuits the honest case; the running total is what bounds a body that
/// declares nothing, or lies.
pub(crate) async fn read_body_capped(response: reqwest::Response) -> Option<Vec<u8>> {
    use futures::StreamExt as _;

    if let Some(declared) = response.content_length()
        && declared > MAX_SIGSTORE_RESPONSE_BYTES
    {
        return None;
    }
    // Sized from the hint only after the cap has already refused an over-declared
    // body, so a hostile Content-Length cannot drive the allocation.
    let hint = response.content_length().unwrap_or(0).min(MAX_SIGSTORE_RESPONSE_BYTES);
    let mut body = Vec::with_capacity(hint as usize);
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.ok()?;
        if body.len() as u64 + chunk.len() as u64 > MAX_SIGSTORE_RESPONSE_BYTES {
            return None;
        }
        body.extend_from_slice(&chunk);
    }
    Some(body)
}

/// Addresses the SSRF guard approved, keyed by the hostname it approved them for.
///
/// Written by [`resolve_sigstore_url`], read by [`PinnedResolver`]. Process-wide
/// because [`sigstore_http_client`] is: one client, one pin table, and the guard
/// runs before any dial on every path that reaches it.
static SIGSTORE_PINS: OnceLock<Mutex<HashMap<String, Vec<SocketAddr>>>> = OnceLock::new();

fn sigstore_pins() -> &'static Mutex<HashMap<String, Vec<SocketAddr>>> {
    SIGSTORE_PINS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Record the addresses the guard approved for `host`.
///
/// Last writer wins: two endpoints on one host agree by construction (the guard
/// re-resolved and re-validated), and a genuine DNS change between two guarded
/// endpoints is a fresh verdict, not a stale one to preserve.
fn pin_sigstore_host(host: &str, addresses: Vec<SocketAddr>) {
    if addresses.is_empty() {
        return;
    }
    // poison-policy: recover. The map holds addresses a validator already
    // approved; a panic elsewhere cannot make an approved address unapproved,
    // and refusing to record it would fail every subsequent dial closed for a
    // reason unrelated to the endpoint.
    let mut pins = sigstore_pins().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    pins.insert(host.to_ascii_lowercase(), addresses);
}

/// DNS resolver for [`sigstore_http_client`] that replays the SSRF guard's verdict.
///
/// [`resolve_sigstore_url`] resolves and validates the endpoint; without this,
/// reqwest then resolves the same name *again* for itself when it connects, and
/// a name answering a public address to the guard and a private one to reqwest
/// walks straight past the floor (CWE-918, DNS rebinding). That window matters
/// most on the Fulcio POST, which carries the OIDC bearer token in its body.
///
/// The hook returns the pinned addresses and never resolves anything, so it is
/// not a second validator that could disagree with the first — it is the same
/// verdict, applied at the dial. That is also why the earlier objection here
/// (that a process-wide resolver cannot tell an opted-in loopback endpoint from
/// a rebind) does not apply: the loopback decision is made once, by the guard,
/// on the URL string the operator typed, and this hook only replays it.
///
/// **Fails closed on an unpinned host.** Every dial site on this client is
/// preceded by a guard call, so an unpinned name means either a new call site
/// that skipped the guard or a redirect/rebind — both refusals, not fallbacks.
///
/// One name is admitted without a pin: this process's own HTTP proxy. Under a
/// proxy the connector dials the proxy, and the Sigstore endpoint travels as
/// literal text in the `CONNECT` line, so the name this hook is asked for is
/// the proxy — a host no guard was ever given, and refusing it fails every
/// Fulcio, Rekor and OIDC call on such a network (ocx#323). Admitting it is a
/// stated design property, not a hole: the proxy is operator configuration,
/// the same trust tier as `trusted_hosts`, and RFC1918 by nature, so a range
/// judgement on it would refuse every corporate deployment. Every other
/// unpinned name is still refused.
///
/// Residual, deliberate: this hook is handed a name with no route context —
/// reqwest asks it what to dial, not why — so "is this the proxy?" is
/// approximated by membership of the configured proxy-host set, which is
/// scheme-agnostic. A host that is both the proxy and a Sigstore endpoint
/// therefore relies on the pin being consulted first, below.
struct PinnedResolver {
    /// This process's proxy configuration, consulted only to recognise the
    /// proxy's own hostname.
    rules: Arc<crate::oci::ssrf::ProxyRules>,
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
            // Pin first, proxy admission only on a miss. One host can be both
            // the configured proxy and a guard-cleared endpoint -- the proxy
            // set is scheme-agnostic, so an `HTTP_PROXY`-only proxy still
            // matches an `https` endpoint the guard routed direct and pinned.
            // Admitting that name as a proxy would throw away the guard's own
            // verdict and re-resolve it unjudged, which is the rebinding
            // window this type exists to close. A proxied route pins nothing,
            // so the ocx#323 admission below still fires whenever it matters.
            match pinned {
                Some(addresses) => Ok(Box::new(addresses.into_iter()) as reqwest::dns::Addrs),
                None if rules.is_proxy_host(&host) => {
                    // Plain lookup, no range judgement: see the type doc
                    // above. Port 0 because reqwest overrides it from the
                    // request URL after resolution, as `GuardedResolver` does.
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
/// Shared by [`validate_sigstore_url`] (which admits `http` only here) and
/// [`resolve_sigstore_url`] (which treats it as the operator's explicit opt-in
/// to a local stack). One function, so the two checks cannot drift into
/// admitting a URL at the boundary that the dial guard then refuses.
fn is_loopback_host(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(h)) => h == "localhost",
        Some(Host::Ipv4(addr)) => addr.is_loopback(),
        Some(Host::Ipv6(addr)) => addr.is_loopback(),
        None => false,
    }
}

/// Re-check a validated Sigstore endpoint against where it actually resolves.
///
/// [`validate_sigstore_url`] is a *string*-level check: it reads the scheme and
/// the literal host. That admits `https://169.254.169.254/` and any DNS name
/// that resolves into a private or link-local range, so on its own it does not
/// close CWE-918 — a hostile `ocx.toml` or managed-config tier setting
/// `rekor-url` to the cloud metadata endpoint would be dialed, and the response
/// surfaces in an error message. This is the same floor
/// [`Index::guard_physical_dial`](crate::oci::Index::guard_physical_dial)
/// applies to a rewritten registry target, against the same
/// [`resolve_and_validate`](crate::oci::ssrf::resolve_and_validate) predicate
/// and the same `trusted_hosts` escape hatch, so an operator running Sigstore
/// on a private address configures it exactly once and identically.
///
/// The loopback carve-out is opt-in by construction: an `http://localhost:5555`
/// endpoint resolves into a forbidden range, so it is admitted only because the
/// *string* already said loopback — which is the operator typing it. A name
/// that merely resolves to loopback gets no such pass, which is what closes DNS
/// rebinding onto a local stack.
///
/// The verdict is then *pinned*: the approved addresses are recorded for the
/// host and [`PinnedResolver`] replays them when [`sigstore_http_client`]
/// connects, so the HTTP client never resolves the name a second time. That
/// closes the resolve-then-connect rebinding window this guard would otherwise
/// leave open -- unlike the registry guard, where the pull runs on a shared
/// client with no resolver hook.
///
/// All of the above describes a *direct* route. Where the process's proxy
/// configuration intercepts the endpoint, the proxy resolves and dials it and
/// this process never does, so
/// [`guard_destination`](crate::oci::ssrf::guard_destination) performs no
/// lookup, refuses a forbidden IP literal textually, and approves no addresses
/// -- nothing is pinned, and [`PinnedResolver`] is asked for the proxy's name
/// rather than the endpoint's.
///
/// # Errors
///
/// [`SsrfError::ForbiddenTarget`](crate::oci::ssrf::SsrfError::ForbiddenTarget)
/// when the endpoint resolves into a forbidden range with no `trusted_hosts`
/// entry (or, on a proxied route, is a forbidden IP literal), and
/// [`SsrfError::Resolution`](crate::oci::ssrf::SsrfError::Resolution) when a
/// directly-routed endpoint does not resolve at all -- failing closed either
/// way.
pub async fn resolve_sigstore_url(url: &Url, trusted: &[String]) -> Result<(), crate::oci::ssrf::SsrfError> {
    resolve_sigstore_url_with_rules(url, trusted, &crate::oci::ssrf::proxy_rules()).await
}

/// [`resolve_sigstore_url`] with the proxy rules injected instead of read from
/// the process environment.
///
/// The seam exists because the route decides the whole verdict: on a proxied
/// route the process never resolves the endpoint, so there is nothing to look
/// up and nothing to pin. Proving that needs rules a test can choose, and the
/// alternative -- mutating `HTTPS_PROXY` around the call -- is `unsafe` in
/// edition 2024 and racy under a shared-process test runner.
///
/// Private, so the public shape stays a two-argument call for the sign, verify
/// and auto-verify callers outside this module.
async fn resolve_sigstore_url_with_rules(
    url: &Url,
    trusted: &[String],
    rules: &crate::oci::ssrf::ProxyRules,
) -> Result<(), crate::oci::ssrf::SsrfError> {
    // `host_str()` re-brackets an IPv6 literal (`[::1]`), and a bracketed host
    // is neither a parseable address nor a resolvable name -- it would fail
    // closed on every IPv6 endpoint. Take the already-parsed host instead.
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
    // The scheme decides which proxy the destination would be routed through
    // (`HTTPS_PROXY` vs `HTTP_PROXY`), so it is read from the URL rather than
    // assumed. `validate_sigstore_url` admits only `https` and a loopback
    // `http`, so no third scheme reaches here; treating one as `Https` anyway
    // is the fail-closed direction, since an http-only proxy then leaves the
    // full direct-route floor in place.
    let scheme = if url.scheme() == "http" {
        crate::oci::ssrf::DialScheme::Http
    } else {
        crate::oci::ssrf::DialScheme::Https
    };
    match crate::oci::ssrf::guard_destination(scheme, &host, port, trusted, rules).await? {
        crate::oci::ssrf::DialRoute::Direct(approved) => pin_sigstore_host(&host, approved),
        // Nothing to pin: on a proxied route the process resolves only the
        // proxy, so the guard approved no addresses for this host. Recording
        // one here would hand [`PinnedResolver`] a verdict no guard made.
        crate::oci::ssrf::DialRoute::Proxied => {}
    }
    Ok(())
}

/// Reason why a user-supplied Sigstore endpoint URL was rejected.
///
/// Returned by [`validate_sigstore_url`] on failure. Callers wrap this into
/// their own `InvalidEndpointUrl` error variant (`SignErrorKind` or
/// `VerifyErrorKind`) with the originating flag name attached.
///
/// The `reason` string is safe to surface in CLI stderr and JSON envelopes:
/// it is constructed entirely from the structural classification of the URL
/// (empty string, bad scheme, etc.) and never echoes credential-bearing raw
/// input (CWE-209 mitigation). The parse-failure branch deliberately omits
/// the raw input — an unparseable URL may still contain `user:pass@`
/// substrings whose userinfo cannot be reliably stripped before parsing —
/// and every branch that echoes a parsed URL routes it through
/// [`scrub_for_echo`] first, which clears userinfo, query and fragment.
#[derive(Debug, thiserror::Error)]
#[error("{reason}")]
pub struct UrlRejection {
    /// Short description of why the URL was rejected.
    pub reason: String,
    /// The exit code a bare rejection (one that reached the exit boundary
    /// without a sign- or verify-side wrap) classifies to.
    /// [`ExitCode::UsageError`](crate::cli::ExitCode::UsageError) for every
    /// string-level rejection; [`Self`]'s `From<SsrfError>` impl raises it to
    /// [`Unavailable`](crate::cli::ExitCode::Unavailable) for an endpoint that
    /// does not resolve.
    exit: crate::cli::ExitCode,
}

impl From<crate::oci::ssrf::SsrfError> for UrlRejection {
    /// Carry an SSRF verdict through the existing `InvalidEndpointUrl` channel.
    ///
    /// A refused endpoint is a refused endpoint whichever layer caught it, and
    /// routing it here keeps the CLI contract fixed: same error variant, same
    /// `error.detail` flag attribution, and — for every verdict but an
    /// unresolvable host — the same exit code. `SsrfError`'s own
    /// `Display` names the host and the address it resolved to -- both from the
    /// caller's own URL, so there is nothing to redact.
    fn from(error: crate::oci::ssrf::SsrfError) -> Self {
        // Deliberately not `error.classify()`. That is the *registry* guard's
        // table, where a forbidden target is a configuration error (78); on
        // the Sigstore side a rejected endpoint URL is documented as 64, and
        // that is the contract both `SignErrorKind::InvalidEndpointUrl` and
        // its verify twin report. Only the resolution failure moves, to the
        // 69 the registry guard now gives the identical condition.
        let exit = match error {
            crate::oci::ssrf::SsrfError::Resolution { .. } => crate::cli::ExitCode::Unavailable,
            // Spelled out rather than wildcarded: `SsrfError` is
            // `#[non_exhaustive]` only to other crates, so within this one an
            // added variant lands here as a compile error instead of a silent
            // 64.
            crate::oci::ssrf::SsrfError::ForbiddenTarget { .. } => crate::cli::ExitCode::UsageError,
        };
        Self {
            reason: error.to_string(),
            exit,
        }
    }
}

impl UrlRejection {
    /// Builds a bare rejection classifying to
    /// [`ExitCode::UsageError`](crate::cli::ExitCode::UsageError).
    ///
    /// `pub` (rather than `oci::endpoint`-private) so a caller outside this
    /// module can construct a rejection without going through a struct
    /// literal — the private `exit` field means that literal no longer
    /// compiles outside this module.
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            exit: crate::cli::ExitCode::UsageError,
        }
    }

    /// The exit code this rejection classifies to, independent of whether a
    /// sign- or verify-side wrap ever asks
    /// [`ClassifyExitCode::classify`](crate::cli::ClassifyExitCode::classify).
    #[must_use]
    pub fn exit_code(&self) -> crate::cli::ExitCode {
        self.exit
    }
}

impl crate::cli::ClassifyExitCode for UrlRejection {
    /// Exit 64 (`UsageError`) for a rejected URL, the same code
    /// [`SignErrorKind::InvalidEndpointUrl`](crate::oci::sign::SignErrorKind::InvalidEndpointUrl)
    /// and its verify twin already give — and 69 (`Unavailable`) for the one
    /// rejection that is not the operator's fault, an endpoint whose host does
    /// not resolve. Both answers are carried on the rejection itself, so a
    /// bare classification and a wrapped one cannot disagree.
    ///
    /// This impl is what a *bare* rejection classifies to — one that reached
    /// the exit boundary without a sign- or verify-side wrap, because it came
    /// from `[trust.sigstore]` rather than from a flag and there was no
    /// identifier to attach it to. Matching 64 rather than minting a config
    /// code keeps one answer for one condition: the same bad URL exits the
    /// same way whichever tier supplied it.
    fn classify(&self) -> Option<crate::cli::ExitCode> {
        Some(self.exit)
    }
}

/// Strip every part of an operator-supplied URL that can carry a secret,
/// before it is echoed back inside a rejection message.
///
/// Userinfo is the CWE-209 case the credentials branch already handled. Query
/// and fragment are the same hazard one component over: a `[trust.sigstore]`
/// entry is operator config, and `http://fulcio.corp/?token=hush` puts a
/// bearer token in a string that a rejection prints to stderr and into the
/// JSON error envelope (ERR-17). Scheme, host, port and path survive, so the
/// operator can still tell which entry was refused.
fn scrub_for_echo(url: &Url) -> Url {
    let mut scrubbed = url.clone();
    // Both setters fail only on a cannot-be-a-base URL, which cannot reach a
    // rejection that wants to name a host; nothing is lost by ignoring them.
    let _ = scrubbed.set_username("");
    let _ = scrubbed.set_password(None);
    scrubbed.set_query(None);
    scrubbed.set_fragment(None);
    scrubbed
}

/// Validate a user-supplied Sigstore endpoint URL.
///
/// Accepts:
/// - Any `https://` URL (production Fulcio/Rekor endpoints).
/// - `http://` on loopback hosts (`127.0.0.0/8`, `::1`, `localhost`) for
///   integration-test fixtures.
///
/// Rejects:
/// - `http://` on non-loopback hosts (SSRF risk, CWE-918).
/// - Any scheme other than `http` or `https` (`file://`, `ftp://`, etc.).
/// - URLs embedding credentials (`https://user:pass@host/`) — Sigstore
///   endpoints never require userinfo; presence indicates URL confusion
///   or credential-stuffing attempts.
/// - Empty or unparseable strings.
///
/// Scheme comparison is case-insensitive by virtue of `url::Url::parse`
/// normalizing the scheme to lowercase during parsing, so `HTTPS://...`
/// is accepted identically to `https://...`.
///
/// # Errors
///
/// Returns a [`UrlRejection`] describing the violation. Callers wrap it into
/// their own `InvalidEndpointUrl` variant, citing the flag name so the error
/// envelope's `error.detail` is programmatically dispatchable.
pub fn validate_sigstore_url(raw: &str, _flag_name: &str) -> Result<Url, UrlRejection> {
    // Do not echo `raw` in the parse-failure message: an unparseable input may
    // still contain a `user:password@host` substring (the parser rejects the
    // URL for unrelated reasons — bad port, invalid host, etc.), and embedding
    // it here would leak the credential into stderr or the JSON envelope
    // before the post-parse userinfo scrubber below can run (CWE-209).
    let url = Url::parse(raw).map_err(|e| UrlRejection::new(format!("malformed URL: {e}")))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UrlRejection::new(format!(
            "URL must not embed credentials (sanitized: `{}`)",
            scrub_for_echo(&url)
        )));
    }
    let scheme = url.scheme();
    match (scheme, is_loopback_host(&url)) {
        ("https", _) => Ok(url),
        ("http", true) => Ok(url),
        ("http", false) => Err(UrlRejection::new(format!(
            "URL must use HTTPS (sanitized: `{}`); HTTP only accepted for loopback hosts",
            scrub_for_echo(&url)
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
            Arc::new(crate::oci::ssrf::ProxyRules::new(
                hyper_util::client::proxy::matcher::Matcher::builder().build(),
            )),
        )
        .no_proxy()
        .build()
        .expect("the shared builder produces a client")
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
        let rules = Arc::new(crate::oci::ssrf::ProxyRules::new(
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

        let rules = Arc::new(crate::oci::ssrf::ProxyRules::new(
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
            &crate::oci::ssrf::ProxyRules::new(hyper_util::client::proxy::matcher::Matcher::builder().build()),
        )
        .await
        .expect("loopback is the operator's opt-in");

        use reqwest::dns::Resolve as _;
        use std::str::FromStr as _;
        let name = reqwest::dns::Name::from_str("localhost").expect("test name");
        let rules = Arc::new(crate::oci::ssrf::ProxyRules::new(
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
        assert!(matches!(error, crate::oci::ssrf::SsrfError::ForbiddenTarget { .. }));
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
                &crate::oci::ssrf::ProxyRules::new(hyper_util::client::proxy::matcher::Matcher::builder().build()),
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
        assert!(matches!(error, crate::oci::ssrf::SsrfError::ForbiddenTarget { .. }));

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
            &crate::oci::ssrf::ProxyRules::new(hyper_util::client::proxy::matcher::Matcher::builder().build()),
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
            read_body_capped(response).await.is_none(),
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
            Arc::new(crate::oci::ssrf::ProxyRules::new(
                hyper_util::client::proxy::matcher::Matcher::builder().build(),
            )),
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
            Some(&b"{\"logIndex\":1}"[..]),
            "an under-cap body must be returned unchanged"
        );
    }

    // ── The proxy route: the process dials the proxy, not the endpoint ─────

    /// An operator whose only egress is an HTTP proxy named by *hostname* can
    /// sign and verify (ocx-sh/ocx#323).
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
        let rules = Arc::new(crate::oci::ssrf::ProxyRules::new(
            hyper_util::client::proxy::matcher::Matcher::builder()
                .all(proxy_url.clone())
                .build(),
        ));
        let client = sigstore_client_builder(Duration::from_secs(5), rules)
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

        let rules = Arc::new(crate::oci::ssrf::ProxyRules::new(
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
        let rules = crate::oci::ssrf::ProxyRules::new(
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

    /// A Sigstore endpoint that does not resolve is unavailable (69), not a
    /// usage error (64).
    ///
    /// 64 tells the operator their `--fulcio-url` is malformed. A name that
    /// fails to resolve is the same class of failure as any unreachable
    /// service -- the flag was fine, the network was not -- and the registry
    /// guard now says 69 for it too, so the two answers agree. The other two
    /// rows are the paired positives: a refused address and a bad scheme are
    /// genuine usage errors and keep 64.
    ///
    /// Discriminates: build the rejection from a fixed `UsageError` and the
    /// resolution row reds.
    #[test]
    fn a_url_rejection_carrying_a_resolution_failure_classifies_as_unavailable() {
        use crate::cli::{ClassifyExitCode as _, ExitCode};

        let unresolvable = UrlRejection::from(crate::oci::ssrf::SsrfError::Resolution {
            host: "fulcio.invalid".to_string(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "failed to lookup address information"),
        });
        assert_eq!(
            unresolvable.exit_code(),
            ExitCode::Unavailable,
            "an endpoint that does not resolve is unavailable, not a usage error"
        );
        assert_eq!(
            unresolvable.classify(),
            Some(ExitCode::Unavailable),
            "a bare rejection classifies to the verdict it carries"
        );

        let forbidden = UrlRejection::from(crate::oci::ssrf::SsrfError::ForbiddenTarget {
            host: "fulcio.corp.test".to_string(),
            ip: std::net::IpAddr::V4(std::net::Ipv4Addr::new(10, 1, 2, 3)),
        });
        assert_eq!(
            forbidden.exit_code(),
            ExitCode::UsageError,
            "a refused endpoint stays the 64 the sign and verify wraps already report"
        );

        let bad_scheme = unwrap_err(validate_sigstore_url("ftp://example.com/bundle", "--rekor-url"));
        assert_eq!(
            bad_scheme.exit_code(),
            ExitCode::UsageError,
            "a string-level rejection is a usage error"
        );
    }
}
