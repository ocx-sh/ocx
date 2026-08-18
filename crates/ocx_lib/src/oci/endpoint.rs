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
//! reaches an HTTP client. The function returns a [`UrlRejection`] on failure;
//! each caller wraps it into their own `InvalidEndpointUrl` variant so
//! exit-code classification stays local to the sign or verify subsystem.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use url::{Host, Url};

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

/// Shared HTTP client for Sigstore trust-services calls.
///
/// `reqwest::Client::new()` carries no default timeout, so a stalled Fulcio or
/// Rekor endpoint hangs verify forever — and via the policy-gated auto-verify
/// hook, hangs every covered install, turning the fail-closed gate into
/// fail-hung. A single process-wide client with bounded connect + request
/// timeouts closes that, and its internal connection pool is reused across the
/// sign and verify call sites instead of rebuilt per request.
///
/// Lives at `oci::endpoint` (a peer of `oci::sign`/`oci::verify`) so both
/// pipelines share one HTTP seam without verify depending on sign.
pub fn sigstore_http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        // Bundled roots, exactly as `forge::github` and `oci::index::ocx_index`
        // do it: reqwest's rustls path falls back to the OS trust store and
        // panics where that store is empty (minimal container, CI runner with
        // no ca-certificates). Without this, `ocx install` keeps working — the
        // `oci_client` transport seeds its own roots in the fork — while
        // `ocx package verify` panics on the same host, and auto-verify carries
        // that panic into every covered install.
        let bounded = crate::utility::tls::seed_embedded_roots(reqwest::Client::builder())
            .connect_timeout(SIGSTORE_CONNECT_TIMEOUT)
            .timeout(SIGSTORE_REQUEST_TIMEOUT)
            .redirect(refuse_redirects())
            .dns_resolver(Arc::new(PinnedResolver))
            .build();
        match bounded {
            Ok(client) => client,
            // Only a TLS-backend init failure reaches here, and it fails every
            // HTTPS request that follows anyway. The fallback keeps the
            // redirect refusal rather than trading it away for a bare client:
            // a destination the SSRF guard never saw is never dialed, even on
            // a degraded build.
            Err(_) => crate::utility::tls::seed_embedded_roots(reqwest::Client::builder())
                .redirect(refuse_redirects())
                .dns_resolver(Arc::new(PinnedResolver))
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
/// One deployment is refused as a side effect: with an HTTP proxy configured by
/// hostname, the connector dials the *proxy*, which no guard ever named, so
/// every Sigstore call fails here. That is fail-closed rather than silent, but
/// it is a regression for proxied environments — tracked as
/// <https://github.com/ocx-sh/ocx/issues/323>, and the refusal message says so
/// rather than blaming a host the operator never configured.
struct PinnedResolver;

impl reqwest::dns::Resolve for PinnedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_ascii_lowercase();
        // Clone out before the async block: no lock is held across an await.
        let pinned = sigstore_pins()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&host)
            .cloned();
        Box::pin(async move {
            match pinned {
                Some(addresses) => Ok(Box::new(addresses.into_iter()) as reqwest::dns::Addrs),
                None => Err(format!(
                    "sigstore host {host} was never approved by the SSRF guard; only a guarded \
                     endpoint is dialed. If {host} is an HTTP proxy, signing and verification \
                     cannot run through it — see https://github.com/ocx-sh/ocx/issues/323"
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
/// # Errors
///
/// [`SsrfError::ForbiddenTarget`](crate::oci::ssrf::SsrfError::ForbiddenTarget)
/// when the endpoint resolves into a forbidden range with no `trusted_hosts`
/// entry, and
/// [`SsrfError::Resolution`](crate::oci::ssrf::SsrfError::Resolution) when it
/// does not resolve at all -- failing closed either way.
pub async fn resolve_sigstore_url(url: &Url, trusted: &[String]) -> Result<(), crate::oci::ssrf::SsrfError> {
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
    let approved = crate::oci::ssrf::resolve_and_validate(&host, port, trusted).await?;
    pin_sigstore_host(&host, approved);
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
/// and the post-parse userinfo branch reconstructs a sanitized URL with
/// `username=""`, `password=None` before formatting.
#[derive(Debug, thiserror::Error)]
#[error("{reason}")]
pub struct UrlRejection {
    /// Short description of why the URL was rejected.
    pub reason: String,
}

impl From<crate::oci::ssrf::SsrfError> for UrlRejection {
    /// Carry an SSRF verdict through the existing `InvalidEndpointUrl` channel.
    ///
    /// A refused endpoint is a refused endpoint whichever layer caught it, and
    /// routing it here keeps the CLI contract fixed: same error variant, same
    /// `error.detail` flag attribution, same exit code. `SsrfError`'s own
    /// `Display` names the host and the address it resolved to -- both from the
    /// caller's own URL, so there is nothing to redact.
    fn from(error: crate::oci::ssrf::SsrfError) -> Self {
        Self::new(error.to_string())
    }
}

impl UrlRejection {
    fn new(reason: impl Into<String>) -> Self {
        Self { reason: reason.into() }
    }
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
        // Strip the userinfo component before echoing the URL back, so that
        // any password supplied on the command line does not leak into logs
        // or the structured JSON error envelope (CWE-209).
        let mut sanitized = url.clone();
        let _ = sanitized.set_username("");
        let _ = sanitized.set_password(None);
        return Err(UrlRejection::new(format!(
            "URL must not embed credentials (sanitized: `{sanitized}`)"
        )));
    }
    let scheme = url.scheme();
    match (scheme, is_loopback_host(&url)) {
        ("https", _) => Ok(url),
        ("http", true) => Ok(url),
        ("http", false) => Err(UrlRejection::new(format!(
            "URL must use HTTPS (got `{raw}`); HTTP only accepted for loopback hosts"
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
        let error = sigstore_http_client()
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
        let resolved: Vec<SocketAddr> = PinnedResolver
            .resolve(name)
            .await
            .expect("pinned host resolves")
            .collect();
        assert_eq!(resolved, approved);
    }

    /// A loopback endpoint the operator typed is pinned by the guard, so the
    /// local-stack carve-out survives the resolver.
    #[tokio::test]
    async fn guarding_a_loopback_endpoint_pins_it_for_the_client() {
        let url = validate_sigstore_url("http://localhost:5555", "--rekor-url").expect("loopback URL is admitted");
        resolve_sigstore_url(&url, &[])
            .await
            .expect("loopback is the operator's opt-in");

        use reqwest::dns::Resolve as _;
        use std::str::FromStr as _;
        let name = reqwest::dns::Name::from_str("localhost").expect("test name");
        let resolved: Vec<SocketAddr> = PinnedResolver
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
            resolve_sigstore_url(&url, &[])
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
        resolve_sigstore_url(&url, &[]).await.expect("a public address passes");
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
        // The credential check at lines 79-88 must fire BEFORE the {raw} echo at line 101.
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

        let error = sigstore_http_client()
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

        let response = sigstore_http_client()
            .get(format!("http://{addr}/api/v1/log/entries"))
            .send()
            .await
            .expect("the flooding endpoint answers");
        assert!(
            read_body_capped(response).await.is_none(),
            "an unbounded trust-service body was read into memory instead of being refused"
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

        let response = sigstore_http_client()
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
}
