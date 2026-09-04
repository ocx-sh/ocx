// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Default-on SSRF guard for remote-controlled registry hosts.
//!
//! An index root's `repository` pointer (`oci://host/path`) arrives in
//! remote-controlled data: a mirrored or compromised index can name any host it
//! likes. Before OCX dereferences that pointer into a physical registry fetch,
//! the host must not resolve to a private, loopback, link-local, or metadata
//! address — the classic Server-Side Request Forgery target set (ocx#218). Public
//! hosts never trip the guard, so the open-source path stays zero-config.
//!
//! The check binds to the **resolved IPs at connect time** — a hostname-string
//! check alone loses to DNS rebinding. Two layers realise resolve -> validate ->
//! pin:
//!
//! 1. [`resolve_and_validate`] is a pre-flight the read path calls **before** the
//!    first physical registry request (ordering parity with the index bot). It
//!    resolves the host, validates every address, and fails fast with no transport
//!    call when any address is forbidden.
//! 2. [`GuardedResolver`] is a [`reqwest::dns::Resolve`] hook injected into the
//!    physical-fetch client. reqwest connects only to the addresses the resolver
//!    returns, so the same validation runs again at connect time — closing the
//!    resolve -> connect rebinding window.
//!
//! Both layers are **route-aware**. Under a configured HTTP proxy the process
//! resolves and dials only the proxy — the destination is literal text in a
//! `CONNECT` line — so [`guard_destination`] performs no DNS on that route and
//! judges the destination textually instead, after normalising it exactly as
//! reqwest will (`0x7f000001` is `127.0.0.1`). An IP literal is still refused
//! here in every spelling `url` parses — including the IPv4-mapped,
//! IPv4-compatible and NAT64-embedded IPv6 forms — and so is a loopback
//! **name**, which says its own address (RFC 6761 §6.3). A host that merely
//! *resolves* to an internal address is the proxy's egress policy to refuse.
//! Symmetrically
//! [`GuardedResolver`] admits the process's own proxy host with no range
//! judgement, because a corporate proxy is operator config and RFC1918 by
//! nature (ocx#323).
//!
//! `trusted_hosts` (configured per `[registries."<ns>"]`) is the explicit escape
//! hatch: a listed host or CIDR skips validation, so a private corporate registry
//! reaches its own private index without disabling the guard globally. Host
//! *allowlisting* (which hosts may appear in roots at all) stays index-side
//! governance — this module only enforces the SSRF floor.

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, LazyLock};

use hyper_util::client::proxy::matcher::Matcher;
use url::{Host, Url};

use crate::cli::{ClassifyExitCode, ExitCode};

/// A physical host was refused, or could not be resolved, by the SSRF guard.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SsrfError {
    /// The host resolved to an address inside a forbidden range (loopback,
    /// private, link-local, metadata, or unspecified) and was not listed in
    /// `trusted_hosts`.
    #[error("host {host} resolves to a forbidden address {ip}; add it to trusted_hosts to allow")]
    ForbiddenTarget { host: String, ip: IpAddr },

    /// DNS resolution of the host failed at the transport layer.
    #[error("failed to resolve host {host}")]
    Resolution {
        host: String,
        #[source]
        source: std::io::Error,
    },
}

impl ClassifyExitCode for SsrfError {
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            // The fix is a `trusted_hosts` config entry — a configuration
            // error (78), not a transient fault. NOTE: the Sigstore side maps
            // this variant to 64 instead, in `From<SsrfError> for UrlRejection`
            // (`oci/endpoint.rs`) — change 78 here and that table too.
            Self::ForbiddenTarget { .. } => ExitCode::ConfigError,
            // A DNS lookup failure means the physical registry could not be
            // reached at all — the same "unreachable" category as any other
            // registry connectivity failure (69).
            Self::Resolution { .. } => ExitCode::Unavailable,
        })
    }
}

/// True for addresses OCX refuses to reach from a remote-controlled host: IPv4
/// loopback / RFC1918 private / link-local (169.254.0.0/16, including the
/// 169.254.169.254 cloud-metadata endpoint) / unspecified / CGNAT-shared
/// (100.64.0.0/10, e.g. Tailscale/overlay ranges) / broadcast / multicast /
/// documentation (192.0.2/24, 198.51.100/24, 203.0.113/24) / benchmarking
/// (198.18.0.0/15) / reserved (240.0.0.0/4), and the IPv6 equivalents —
/// loopback (`::1`), ULA (`fc00::/7`), link-local (`fe80::/10`), unspecified
/// (`::`), multicast (`ff00::/8`), documentation (`2001:db8::/32`). Every IPv6
/// form that embeds an IPv4 target — mapped (`::ffff:a.b.c.d`), compatible
/// (`::a.b.c.d`) and NAT64 (`64:ff9b::a.b.c.d`) — is unwrapped and judged by
/// that embedded address, so no encoding can smuggle a forbidden target past
/// the guard.
pub fn is_forbidden_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_forbidden_v4(v4),
        IpAddr::V6(v6) => is_forbidden_v6(v6),
    }
}

fn is_forbidden_v4(ip: Ipv4Addr) -> bool {
    ip.is_loopback()        // 127.0.0.0/8
        || ip.is_private()    // 10/8, 172.16/12, 192.168/16
        || ip.is_link_local() // 169.254.0.0/16 (incl. 169.254.169.254 metadata)
        || ip.is_unspecified() // 0.0.0.0
        || is_shared_cgnat(ip) // 100.64.0.0/10 (CGNAT / overlay networks)
        || ip.is_broadcast()   // 255.255.255.255
        || ip.is_multicast()   // 224.0.0.0/4
        || ip.is_documentation() // 192.0.2/24, 198.51.100/24, 203.0.113/24
        || is_benchmarking(ip) // 198.18.0.0/15
        || is_reserved(ip) // 240.0.0.0/4
}

/// `100.64.0.0/10` — carrier-grade NAT / shared address space (RFC 6598),
/// also used by overlay networks such as Tailscale. `Ipv4Addr::is_shared` is
/// unstable, so the `/10` prefix is hand-rolled: octets[1]'s top two bits
/// must be `01` (i.e. `octets[1]` in `64..=127`).
fn is_shared_cgnat(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    octets[0] == 100 && (octets[1] & 0xc0) == 0x40
}

/// `198.18.0.0/15` — benchmarking address space (RFC 2544).
fn is_benchmarking(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    octets[0] == 198 && (octets[1] & 0xfe) == 18
}

/// `240.0.0.0/4` — reserved for future use (includes the limited-broadcast
/// address, already covered by `is_broadcast`).
fn is_reserved(ip: Ipv4Addr) -> bool {
    ip.octets()[0] >= 240
}

fn is_forbidden_v6(ip: Ipv6Addr) -> bool {
    // An IPv4-mapped address embeds an IPv4 target — judge it as IPv4 so a
    // `::ffff:127.0.0.1` cannot bypass the IPv4 range checks above.
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_forbidden_v4(v4);
    }
    let first = ip.segments()[0];
    ip.is_loopback()                 // ::1
        || ip.is_unspecified()         // ::
        || ip.is_multicast()           // ff00::/8
        || is_documentation_v6(ip)      // 2001:db8::/32
        || (first & 0xfe00) == 0xfc00  // fc00::/7 unique-local
        || (first & 0xffc0) == 0xfe80 // fe80::/10 link-local
        || embeds_forbidden_v4(ip) // ::a.b.c.d, 64:ff9b::a.b.c.d
}

/// `::a.b.c.d` (IPv4-compatible, `::/96`) and `64:ff9b::a.b.c.d` (the NAT64
/// well-known prefix, RFC 6052) carry an IPv4 target in their last two
/// segments, so both reach a forbidden address without matching any predicate
/// above.
///
/// Deliberately judged **last**: `::1` and `::` are themselves inside `::/96`,
/// and unwrapping before the loopback and unspecified checks would read `::1`
/// as the allowed `0.0.0.1`. For the same reason this is a separate function
/// rather than swapping `to_ipv4_mapped` for `to_ipv4`, which unwraps the
/// compatible form at the top where precedence is wrong.
fn embeds_forbidden_v4(ip: Ipv6Addr) -> bool {
    let [prefix @ .., high, low] = ip.segments();
    if prefix != [0, 0, 0, 0, 0, 0] && prefix != [0x64, 0xff9b, 0, 0, 0, 0] {
        return false;
    }
    is_forbidden_v4(Ipv4Addr::from((u32::from(high) << 16) | u32::from(low)))
}

/// `2001:db8::/32` — documentation address space (RFC 3849).
/// `Ipv6Addr::is_documentation` is unstable, so the `/32` prefix is
/// hand-rolled from the first two segments.
fn is_documentation_v6(ip: Ipv6Addr) -> bool {
    let segments = ip.segments();
    segments[0] == 0x2001 && segments[1] == 0x0db8
}

/// Whether `host` is exempt from validation: an exact string match against a
/// `trusted_hosts` entry, or — when `host` is an IP literal — membership in a
/// trusted `CIDR` entry (e.g. `10.0.0.0/8` covers `10.1.2.3`).
pub fn host_is_trusted(host: &str, trusted: &[String]) -> bool {
    let host_ip = host.parse::<IpAddr>().ok();
    trusted.iter().any(|entry| {
        if entry == host {
            return true;
        }
        match (host_ip, parse_cidr(entry)) {
            (Some(ip), Some((network, prefix))) => cidr_contains(network, prefix, ip),
            _ => false,
        }
    })
}

/// Parses a `<addr>/<prefix>` CIDR entry. Returns `None` for a bare host, a bad
/// address, or a non-numeric / out-of-range prefix.
fn parse_cidr(entry: &str) -> Option<(IpAddr, u8)> {
    let (addr, prefix) = entry.split_once('/')?;
    let network = addr.parse::<IpAddr>().ok()?;
    let prefix = prefix.parse::<u8>().ok()?;
    let max = match network {
        IpAddr::V4(_) => 32,
        IpAddr::V6(_) => 128,
    };
    (prefix <= max).then_some((network, prefix))
}

/// Whether `ip` falls inside the `network/prefix` CIDR block. Mismatched address
/// families never match. A `/0` prefix matches everything of the same family.
fn cidr_contains(network: IpAddr, prefix: u8, ip: IpAddr) -> bool {
    match (network, ip) {
        (IpAddr::V4(network), IpAddr::V4(ip)) => {
            let mask = u32::MAX.checked_shl(u32::from(32 - prefix)).unwrap_or(0);
            (u32::from(network) & mask) == (u32::from(ip) & mask)
        }
        (IpAddr::V6(network), IpAddr::V6(ip)) => {
            let mask = u128::MAX.checked_shl(u32::from(128 - prefix)).unwrap_or(0);
            (u128::from(network) & mask) == (u128::from(ip) & mask)
        }
        _ => false,
    }
}

/// Splits a physical registry authority into `(host, port)` for
/// [`resolve_and_validate`], defaulting to `443` when no explicit port is
/// present.
///
/// Accepts the plain `host` and `host:port` forms the registry grammar admits
/// (e.g. `ghcr.io`, `localhost:5000`). Only a trailing numeric `:port` is split
/// off; anything else is treated as a bare host on the default port.
///
/// Lives beside [`resolve_and_validate`] because it is the parse half of the
/// same guard: every caller that validates a remote-controlled authority splits
/// it here first, so the two cannot drift apart (design register X3, shared
/// module).
///
/// Known gap, deliberately unchanged: a bracketed IPv6 authority
/// (`[::1]:5000`) yields the host `"[::1]"`, which `str::parse::<IpAddr>`
/// rejects and no DNS name matches. Both routes still refuse it, by different
/// means: a direct dial reaches [`resolve_and_validate`], whose lookup fails
/// with [`SsrfError::Resolution`], and a proxied dial reaches
/// [`guard_destination`], where [`normalised_destination`] reads the brackets
/// as the URL syntax they are and refuses `::1` as
/// [`SsrfError::ForbiddenTarget`]. Both fail **closed**. Any future bracket
/// stripping must land here, once, so both call sites get it together.
#[must_use]
pub fn split_host_port(registry: &str) -> (&str, u16) {
    match registry.rsplit_once(':') {
        Some((host, port)) => match port.parse::<u16>() {
            Ok(port) => (host, port),
            Err(_) => (registry, 443),
        },
        None => (registry, 443),
    }
}

/// Resolves `host` and returns its socket addresses, refusing any that fall in a
/// forbidden range unless `host` is trusted (`trusted_hosts`).
///
/// This is the pre-flight the read path runs **before** the first physical
/// registry request. A trusted host skips validation and returns its addresses
/// verbatim; otherwise every resolved address must pass [`is_forbidden_ip`], and
/// the first that does not aborts with [`SsrfError::ForbiddenTarget`] and no
/// transport call. `port` is only used to drive resolution; the returned
/// addresses carry it, but the caller's [`GuardedResolver`] pins the connection.
///
/// # Errors
///
/// [`SsrfError::Resolution`] if the host cannot be resolved;
/// [`SsrfError::ForbiddenTarget`] if a non-trusted host resolves to a forbidden
/// address.
pub async fn resolve_and_validate(host: &str, port: u16, trusted: &[String]) -> Result<Vec<SocketAddr>, SsrfError> {
    let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|source| SsrfError::Resolution {
            host: host.to_string(),
            source,
        })?
        .collect();

    if host_is_trusted(host, trusted) {
        return Ok(addresses);
    }

    for address in &addresses {
        if is_forbidden_ip(address.ip()) {
            return Err(SsrfError::ForbiddenTarget {
                host: host.to_string(),
                ip: address.ip(),
            });
        }
    }
    Ok(addresses)
}

/// A [`reqwest::dns::Resolve`] hook that runs [`resolve_and_validate`] at connect
/// time, so reqwest connects only to SSRF-validated addresses (resolve ->
/// validate -> pin). Injected into the physical-fetch client via the vendored
/// `oci_client` fork's `ClientConfig::dns_resolver` seam.
///
/// **One name is exempt: the process's own proxy** ([`ProxyRules::is_proxy_host`]).
/// Under a hostname-configured proxy every request resolves the proxy through
/// this hook, and a corporate proxy is RFC1918 by nature, so keeping the floor
/// on it refuses every Sigstore and registry call (ocx#323). It is admitted
/// with a plain lookup and no range judgement — the same trust tier as
/// `trusted_hosts`, since both are operator configuration rather than
/// remote-controlled data.
///
/// Residual, deliberate: whatever that proxy then reaches is bounded by the
/// proxy's own egress policy, not by this guard — except for the destinations
/// [`guard_destination`] already refused by name or literal before the dial.
/// The exemption covers exactly one operator-named host; every other name
/// still faces the full floor.
pub struct GuardedResolver {
    trusted: Arc<Vec<String>>,
    rules: Arc<ProxyRules>,
}

impl GuardedResolver {
    /// Builds a resolver that exempts the hosts / CIDRs in `trusted`.
    pub fn new(trusted: Arc<Vec<String>>, rules: Arc<ProxyRules>) -> Self {
        Self { trusted, rules }
    }
}

impl reqwest::dns::Resolve for GuardedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let trusted = self.trusted.clone();
        let rules = self.rules.clone();
        Box::pin(async move {
            // reqwest overrides the port from the request URL after resolution, so
            // any placeholder here is discarded; only the validated IPs matter.
            let host = name.as_str();
            let addresses: Vec<SocketAddr> = if rules.is_proxy_host(host) {
                // The process's own proxy: admitted with no range judgement,
                // for the reason on `GuardedResolver`.
                tokio::net::lookup_host((host, 0))
                    .await
                    .map_err(|source| SsrfError::Resolution {
                        host: host.to_string(),
                        source,
                    })?
                    .collect()
            } else {
                resolve_and_validate(host, 0, &trusted).await?
            };
            let addresses: reqwest::dns::Addrs = Box::new(addresses.into_iter());
            Ok(addresses)
        })
    }
}

/// The scheme OCX will dial a registry over.
///
/// Distinct from `Route`: the dial scheme decides which proxy environment
/// variable applies (`HTTP_PROXY` vs `HTTPS_PROXY`), not whether a proxy
/// intercepts the dial at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialScheme {
    Http,
    Https,
}

impl DialScheme {
    /// `Http` iff [`crate::allows_plain_http`] admits `registry` — the
    /// `host[:port]` authority exactly as `OCX_INSECURE_REGISTRIES` spells
    /// it; `Https` otherwise. The ONE site that decides the dial scheme —
    /// every guard site calls this instead of re-deriving the predicate.
    #[must_use]
    pub fn for_registry(insecure_hosts: &[String], registry: &str) -> Self {
        if crate::allows_plain_http(insecure_hosts, registry) {
            Self::Http
        } else {
            Self::Https
        }
    }
}

/// Whether a dial to a guarded destination is intercepted by a configured
/// HTTP proxy, or reaches the destination directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// No proxy intercepts: the process itself resolves and dials the host.
    Direct,
    /// A configured proxy dials the destination; the process resolves only
    /// the proxy.
    Proxied,
}

/// The outcome of [`guard_destination`]: what the SSRF floor decided about a
/// dial, and — on [`Route::Direct`] — the validated addresses to pin.
#[derive(Debug)]
pub enum DialRoute {
    /// No proxy intercepts: the floor-checked addresses the process will
    /// connect to.
    Direct(Vec<SocketAddr>),
    /// A configured proxy dials the destination; the process resolves only
    /// the proxy, so there is nothing here to pin.
    Proxied,
}

/// The system/env HTTP proxy matcher — the same env/system reader reqwest
/// itself consults under its `system-proxy` feature — plus the (at most two)
/// proxy authorities it can ever pick.
///
/// Built once per process via [`proxy_rules`]. [`Self::new`] is the test seam:
/// an explicit `Matcher` (`Matcher::builder()...build()`) instead of reading
/// the ambient environment.
pub struct ProxyRules {
    matcher: Matcher,
    /// The host names (ascii-lowercase, no port, no userinfo) of the proxies
    /// `matcher` can ever pick — at most one per scheme. Enumerated once in
    /// [`Self::new`] by intercepting a synthetic probe destination per scheme.
    proxy_hosts: BTreeSet<String>,
}

/// Synthetic destinations used only to ask `matcher` which proxy authority it
/// would pick for each scheme. Never dialed, never resolved; the name is
/// deliberately one no `NO_PROXY` rule would list.
const PROXY_PROBES: [&str; 2] = ["http://ocx-proxy-probe/", "https://ocx-proxy-probe/"];

impl ProxyRules {
    /// Builds the rules from the process environment / OS proxy settings.
    #[must_use]
    pub fn from_system() -> Self {
        Self::new(Matcher::from_system())
    }

    /// Test seam: build from an explicit matcher instead of reading the
    /// ambient environment.
    ///
    /// Enumerates the proxy-host set by intercepting [`PROXY_PROBES`]. A
    /// matcher with `NO_PROXY=*` intercepts neither probe, so the set is empty
    /// and no name is ever admitted as a proxy host.
    #[must_use]
    pub fn new(matcher: Matcher) -> Self {
        let proxy_hosts = PROXY_PROBES
            .into_iter()
            .filter_map(|probe| {
                let intercept = matcher.intercept(&http::Uri::from_static(probe))?;
                Some(intercept.uri().host()?.to_ascii_lowercase())
            })
            .collect();
        Self { matcher, proxy_hosts }
    }

    /// Whether `host:port` under `scheme` is dialed through a configured
    /// proxy or directly.
    ///
    /// A host that cannot be spelled as a URL degrades to [`Route::Direct`],
    /// which keeps the full SSRF floor on it — the guard's fail-closed
    /// direction.
    #[must_use]
    pub fn dial_route(&self, scheme: DialScheme, host: &str, port: u16) -> Route {
        normalised_destination(scheme, host, port).map_or(Route::Direct, |destination| self.route_for(&destination))
    }

    /// [`Self::dial_route`] over an already-normalised destination, so the
    /// matcher and [`guard_destination`]'s literal check judge the same host.
    fn route_for(&self, destination: &Url) -> Route {
        let Ok(destination) = destination.as_str().parse::<http::Uri>() else {
            return Route::Direct;
        };
        if self.matcher.intercept(&destination).is_some() {
            Route::Proxied
        } else {
            Route::Direct
        }
    }

    /// Whether `name` is one of this process's configured proxy hosts. DNS
    /// names are case-insensitive, so the comparison folds case on both sides.
    #[must_use]
    pub fn is_proxy_host(&self, name: &str) -> bool {
        self.proxy_hosts.iter().any(|proxy| proxy.eq_ignore_ascii_case(name))
    }
}

/// A test seam for the two shapes every guard-site test needs. Kept beside
/// [`ProxyRules`] so `Matcher` stays confined to this module.
#[cfg(test)]
impl ProxyRules {
    /// No proxy is configured: every dial takes [`Route::Direct`].
    pub(crate) fn direct() -> Arc<Self> {
        Arc::new(Self::new(Matcher::builder().build()))
    }

    /// `proxy` intercepts every scheme, the way `ALL_PROXY` does.
    pub(crate) fn proxied_everywhere(proxy: &str) -> Arc<Self> {
        Arc::new(Self::new(Matcher::builder().all(proxy).build()))
    }
}

/// The destination as **reqwest** will parse it, not as the root spelled it.
///
/// reqwest builds its request URL with `url::Url`, whose WHATWG host parser
/// normalises every alternate IPv4 spelling — `0x7f000001`, `2130706433`,
/// `127.1` and `0177.0.0.1` all become `127.0.0.1`, and a bare or bracketed
/// IPv6 literal becomes the canonical bracketed form. Judging the raw string
/// instead would let `oci://0x7f000001:5000/repo` past both the proxy match
/// and `str::parse::<IpAddr>`, while the transport dialed loopback.
///
/// `None` for anything that is not a URL host at all; every caller reads that
/// as [`Route::Direct`], which keeps the resolving floor on it.
fn normalised_destination(scheme: DialScheme, host: &str, port: u16) -> Option<Url> {
    let scheme = match scheme {
        DialScheme::Http => "http",
        DialScheme::Https => "https",
    };
    // A URL spells an IPv6 literal bracketed (RFC 3986 §3.2.2); an authority
    // that already carries its brackets is passed through untouched.
    let destination = if host.parse::<Ipv6Addr>().is_ok() {
        format!("{scheme}://[{host}]:{port}/")
    } else {
        format!("{scheme}://{host}:{port}/")
    };
    Url::parse(&destination).ok()
}

/// Process-wide [`ProxyRules`], read once from the environment. The single
/// proxy-env reader shared by [`ssrf`](self) and
/// [`endpoint`](crate::oci::endpoint).
#[must_use]
pub fn proxy_rules() -> Arc<ProxyRules> {
    static RULES: LazyLock<Arc<ProxyRules>> = LazyLock::new(|| Arc::new(ProxyRules::from_system()));
    RULES.clone()
}

/// `localhost` and every `*.localhost` name are loopback BY DEFINITION
/// (RFC 6761 §6.3) — a resolver may not answer them with anything else — so
/// refusing them by name needs no DNS and cannot be wrong. One trailing dot
/// (the FQDN root) is tolerated; `localhost.example` is an ordinary name and
/// is not matched.
fn is_loopback_name(name: &str) -> bool {
    let name = name.strip_suffix('.').unwrap_or(name).to_ascii_lowercase();
    name == "localhost" || name.ends_with(".localhost")
}

/// The SSRF floor, route-aware — the production replacement for
/// [`resolve_and_validate`] once every guard site threads a [`ProxyRules`]
/// through.
///
/// On [`Route::Direct`] this is [`resolve_and_validate`] verbatim: resolve,
/// validate, fail closed on both an unresolvable host and a forbidden address.
///
/// On [`Route::Proxied`] the proxy resolves and dials the destination, so the
/// process performs **no DNS** — a lookup here would fail on a proxy-only-DNS
/// network (ocx#407) and would judge addresses nothing ever connects to. A
/// trusted host is admitted first; a forbidden IP **literal** is then refused
/// textually, judged after [`normalised_destination`] has folded it into the
/// address reqwest will actually dial.
///
/// A loopback **name** ([`is_loopback_name`]) is refused there too, reported as
/// `127.0.0.1` because RFC 6761 §6.3 says that is what it names. An IP literal
/// is refused on either route, in every spelling `url` parses — including the
/// IPv4-mapped, IPv4-compatible and NAT64-embedded IPv6 forms.
///
/// Residual, deliberate and now narrower: a host whose **name** does not say it
/// is loopback but which resolves to an internal address through the proxy
/// (`intranet.corp`, a rebinding name) is still admitted. Only the proxy's own
/// egress policy can refuse that one — from here the destination is literal
/// text in a `CONNECT` line, and ocx has no address to judge.
///
/// # Errors
///
/// [`SsrfError::ForbiddenTarget`] for a forbidden address (resolved on a direct
/// route, textual on a proxied one — the `ip` field carries the normalised
/// address, `host` the spelling the caller passed); [`SsrfError::Resolution`]
/// when a direct route's host cannot be resolved.
pub async fn guard_destination(
    scheme: DialScheme,
    host: &str,
    port: u16,
    trusted: &[String],
    rules: &ProxyRules,
) -> Result<DialRoute, SsrfError> {
    let destination = normalised_destination(scheme, host, port);
    let route = destination
        .as_ref()
        .map_or(Route::Direct, |destination| rules.route_for(destination));
    match route {
        Route::Direct => Ok(DialRoute::Direct(resolve_and_validate(host, port, trusted).await?)),
        Route::Proxied => {
            if host_is_trusted(host, trusted) {
                return Ok(DialRoute::Proxied);
            }
            let literal = match destination.as_ref().and_then(Url::host) {
                Some(Host::Ipv4(address)) => Some(IpAddr::V4(address)),
                Some(Host::Ipv6(address)) => Some(IpAddr::V6(address)),
                // A loopback NAME carries its address in the name, so it is
                // judged without a lookup. Load-bearing when the proxy is on
                // the caller's own machine (cntlm/mitmproxy on 127.0.0.1),
                // where the residual below — "the proxy's egress policy
                // refuses it" — is not a control at all: `CONNECT
                // localhost:5999` would reach the caller's own loopback from
                // remote-controlled index data.
                Some(Host::Domain(name)) if is_loopback_name(name) => Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
                Some(Host::Domain(_)) | None => None,
            };
            if let Some(ip) = literal
                && is_forbidden_ip(ip)
            {
                return Err(SsrfError::ForbiddenTarget {
                    host: host.to_string(),
                    ip,
                });
            }
            Ok(DialRoute::Proxied)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── is_forbidden_ip truth table (X1) ─────────────────────────────────────

    fn ip(value: &str) -> IpAddr {
        value.parse().expect("test literal is a valid IP")
    }

    #[test]
    fn is_forbidden_ip_flags_private_loopback_linklocal_metadata_unspecified() {
        for forbidden in [
            "127.0.0.1",       // loopback
            "::1",             // v6 loopback
            "10.0.0.1",        // RFC1918
            "192.168.1.1",     // RFC1918
            "172.16.0.1",      // RFC1918
            "169.254.169.254", // link-local metadata endpoint
            "fe80::1",         // v6 link-local
            "fc00::1",         // v6 unique-local (ULA fc00::/7)
            "0.0.0.0",         // unspecified
            "100.64.0.1",      // CGNAT / shared (100.64.0.0/10)
            "100.127.255.255", // CGNAT / shared, top of range
            "198.18.0.1",      // benchmarking (198.18.0.0/15)
            "224.0.0.1",       // multicast (224.0.0.0/4)
            "255.255.255.255", // broadcast
            "240.0.0.1",       // reserved (240.0.0.0/4)
            "192.0.2.1",       // documentation (192.0.2.0/24)
            "ff00::1",         // v6 multicast
            "2001:db8::1",     // v6 documentation (2001:db8::/32)
        ] {
            assert!(is_forbidden_ip(ip(forbidden)), "{forbidden} must be forbidden");
        }
    }

    #[test]
    fn is_forbidden_ip_allows_public_addresses() {
        for public in [
            "8.8.8.8",
            "140.82.121.3",
            "100.63.255.255", // just below the CGNAT range
            "100.128.0.1",    // just above the CGNAT range
        ] {
            assert!(!is_forbidden_ip(ip(public)), "{public} must be allowed");
        }
    }

    #[test]
    fn is_forbidden_ip_unwraps_ipv4_compatible_and_nat64_ipv6() {
        // `::a.b.c.d` (IPv4-compatible) and `64:ff9b::a.b.c.d` (NAT64) both
        // reach an IPv4 target while matching no plain v6 predicate.
        assert!(is_forbidden_ip(ip("::127.0.0.1")), "IPv4-compatible loopback");
        assert!(is_forbidden_ip(ip("64:ff9b::10.0.0.1")), "NAT64-embedded RFC1918");
        // A public embedded address stays allowed — the unwrap judges the
        // address, it does not blanket-refuse the prefix.
        assert!(
            !is_forbidden_ip(ip("64:ff9b::8.8.8.8")),
            "NAT64-embedded public address"
        );
        // Precedence: `::1` is inside `::/96`, and unwrapping it as an IPv4
        // would read it as the allowed `0.0.0.1`. The loopback predicate runs
        // first, so it stays forbidden.
        assert!(
            is_forbidden_ip(ip("::1")),
            "loopback keeps precedence over the embedded unwrap"
        );
    }

    #[test]
    fn is_forbidden_ip_unwraps_ipv4_mapped_ipv6() {
        // `::ffff:127.0.0.1` embeds a loopback IPv4 and must not slip through.
        assert!(is_forbidden_ip(ip("::ffff:127.0.0.1")));
        // A mapped public address stays allowed.
        assert!(!is_forbidden_ip(ip("::ffff:8.8.8.8")));
    }

    // ── host_is_trusted: exact + CIDR (X2) ───────────────────────────────────

    #[test]
    fn host_is_trusted_matches_exact_host() {
        let trusted = vec!["registry.corp".to_string()];
        assert!(host_is_trusted("registry.corp", &trusted));
        assert!(!host_is_trusted("evil.corp", &trusted));
    }

    #[test]
    fn host_is_trusted_matches_cidr_membership() {
        let trusted = vec!["10.0.0.0/8".to_string()];
        assert!(host_is_trusted("10.1.2.3", &trusted), "10.1.2.3 is inside 10.0.0.0/8");
        assert!(!host_is_trusted("11.0.0.1", &trusted), "11.0.0.1 is outside 10.0.0.0/8");
        // A non-IP host never matches a CIDR entry.
        assert!(!host_is_trusted("registry.corp", &trusted));
    }

    #[test]
    fn host_is_trusted_matches_exact_ip_literal() {
        let trusted = vec!["127.0.0.1".to_string()];
        assert!(host_is_trusted("127.0.0.1", &trusted));
    }

    // ── split_host_port (the shared parse half of the guard, X3) ─────────────

    #[test]
    fn split_host_port_defaults_to_443_and_honours_an_explicit_port() {
        assert_eq!(split_host_port("ghcr.io"), ("ghcr.io", 443));
        assert_eq!(split_host_port("localhost:5000"), ("localhost", 5000));
        // A non-numeric or out-of-range `:suffix` is not a port — the whole
        // value stays the host on the default port.
        assert_eq!(
            split_host_port("registry.corp:notaport"),
            ("registry.corp:notaport", 443)
        );
        assert_eq!(split_host_port("registry.corp:99999"), ("registry.corp:99999", 443));
    }

    #[test]
    fn bracketed_ipv6_authority_keeps_its_brackets_and_so_fails_closed() {
        // Documented gap: brackets are NOT stripped. The retained `[::1]` is
        // neither a parseable IP literal nor a resolvable DNS name, so a DIRECT
        // dial refuses it in `resolve_and_validate` (`SsrfError::Resolution`)
        // instead of dialing loopback — it fails CLOSED. Asserted locally (no
        // DNS) via the two properties that make it fail closed. A PROXIED dial
        // never reaches that lookup; the route-accurate half is the paired
        // `a_forbidden_ip_literal_is_refused_on_a_proxied_route` case below,
        // which refuses `[::1]` as a forbidden target instead.
        let (host, port) = split_host_port("[::1]:5000");
        assert_eq!((host, port), ("[::1]", 5000));
        assert!(
            host.parse::<IpAddr>().is_err(),
            "a bracketed host must not parse as an allowed IP literal"
        );
        assert!(
            !host_is_trusted(host, &["::1".to_string()]),
            "a bracketed host must not match a bare-IP trusted_hosts entry either"
        );
    }

    // ── resolve_and_validate (X1/X2) ─────────────────────────────────────────
    //
    // All cases are network-free: IP literals resolve without DNS, and
    // `localhost` resolves to loopback locally.

    #[tokio::test]
    async fn resolve_and_validate_refuses_forbidden_host() {
        let error = resolve_and_validate("127.0.0.1", 443, &[])
            .await
            .expect_err("loopback must be refused");
        assert!(matches!(error, SsrfError::ForbiddenTarget { .. }));
    }

    #[tokio::test]
    async fn resolve_and_validate_refuses_localhost_resolving_to_loopback() {
        let error = resolve_and_validate("localhost", 443, &[])
            .await
            .expect_err("localhost resolves to loopback and must be refused");
        assert!(matches!(error, SsrfError::ForbiddenTarget { .. }));
    }

    #[tokio::test]
    async fn resolve_and_validate_allows_trusted_forbidden_host() {
        let trusted = vec!["127.0.0.1".to_string()];
        let addresses = resolve_and_validate("127.0.0.1", 443, &trusted)
            .await
            .expect("a trusted host skips validation");
        assert!(addresses.iter().any(|a| a.ip() == ip("127.0.0.1")));
    }

    #[tokio::test]
    async fn resolve_and_validate_allows_public_ip_literal() {
        let addresses = resolve_and_validate("8.8.8.8", 443, &[])
            .await
            .expect("a public address passes");
        assert!(addresses.iter().any(|a| a.ip() == ip("8.8.8.8")));
    }

    #[tokio::test]
    async fn guarded_resolver_refuses_forbidden_host_at_connect() {
        use reqwest::dns::Resolve;
        let resolver = GuardedResolver::new(Arc::new(Vec::new()), ProxyRules::direct());
        let name: reqwest::dns::Name = "localhost".parse().expect("valid dns name");
        let result = resolver.resolve(name).await;
        assert!(result.is_err(), "the resolver must reject a loopback host");
    }

    // ── ClassifyExitCode (C13) ────────────────────────────────────────────

    #[test]
    fn forbidden_target_classifies_to_config_error() {
        let error = SsrfError::ForbiddenTarget {
            host: "169.254.169.254".to_string(),
            ip: ip("169.254.169.254"),
        };
        assert_eq!(error.classify(), Some(ExitCode::ConfigError));
    }

    #[test]
    fn resolution_failure_classifies_to_unavailable() {
        let error = SsrfError::Resolution {
            host: "registry.invalid".to_string(),
            source: std::io::Error::other("dns lookup failed"),
        };
        assert_eq!(error.classify(), Some(ExitCode::Unavailable));
    }

    // ── route-aware guard: ProxyRules / guard_destination (plan rows 1–8) ────
    //
    // Every case injects an explicit `Matcher` — no environment variable is
    // read or written (2024-edition `set_var` is unsafe and racy under a
    // multi-threaded test runner).

    /// A proxy that is never dialed: the tests assert on the guard's verdict,
    /// never on a connection.
    const PROXY: &str = "http://proxy.corp:3128";
    /// `.invalid` is reserved by RFC 6761 and never resolves, so an `Ok`
    /// verdict for it proves no DNS lookup gated the decision.
    const PHANTOM: &str = "no-such-registry.invalid";

    // Plan row 1: proxied + unresolvable name → `Proxied`, no lookup performed.
    #[tokio::test]
    async fn a_proxied_destination_is_admitted_without_resolving_it() {
        let rules = ProxyRules::proxied_everywhere(PROXY);
        let route = guard_destination(DialScheme::Https, PHANTOM, 5000, &[], &rules)
            .await
            .expect("a proxy dials the destination, so the process never resolves it");
        assert!(
            matches!(route, DialRoute::Proxied),
            "a proxied dial has no local addresses to pin"
        );
    }

    // Plan row 2: proxied + forbidden IP literal → refused textually, no trust.
    //
    // The alternate IPv4 spellings are the security case: reqwest parses the
    // authority with `url::Url`, which folds every one of them to `127.0.0.1`,
    // so a guard judging the raw string would admit them and the transport
    // would then dial loopback.
    #[tokio::test]
    async fn a_forbidden_ip_literal_is_refused_on_a_proxied_route() {
        let rules = ProxyRules::proxied_everywhere(PROXY);
        for literal in [
            "127.0.0.1",
            "::1",
            "10.0.0.1",
            "0x7f000001",
            "2130706433",
            "127.1",
            "0177.0.0.1",
            "[::1]",
            "[fc00::1]",
            // IPv6 forms that embed an IPv4 target: compatible (`::/96`) and
            // NAT64 (`64:ff9b::/96`). Neither matches a plain v6 predicate.
            "[::127.0.0.1]",
            "[64:ff9b::10.0.0.1]",
        ] {
            let error = guard_destination(DialScheme::Https, literal, 5000, &[], &rules)
                .await
                .expect_err("a forbidden literal is refused whichever route dials it");
            assert!(
                matches!(error, SsrfError::ForbiddenTarget { .. }),
                "{literal} must be refused as a forbidden target, got {error}"
            );
        }
    }

    // Paired positive for the embedded-IPv4 cases above: the unwrap judges the
    // embedded address rather than refusing the whole prefix.
    #[tokio::test]
    async fn a_nat64_address_embedding_a_public_target_is_admitted_on_a_proxied_route() {
        let rules = ProxyRules::proxied_everywhere(PROXY);
        let route = guard_destination(DialScheme::Https, "[64:ff9b::8.8.8.8]", 5000, &[], &rules)
            .await
            .expect("a public embedded address is not a forbidden target");
        assert!(matches!(route, DialRoute::Proxied));
    }

    // A loopback NAME is refused on the proxied route: the proxy may be on the
    // caller's own machine, so "the proxy refuses it" is not a control there.
    #[tokio::test]
    async fn a_loopback_name_is_refused_on_a_proxied_route() {
        let rules = ProxyRules::proxied_everywhere(PROXY);
        for name in ["localhost", "LOCALHOST.", "api.localhost"] {
            let error = guard_destination(DialScheme::Https, name, 5999, &[], &rules)
                .await
                .expect_err("a loopback name is loopback by definition (RFC 6761)");
            assert!(
                matches!(error, SsrfError::ForbiddenTarget { .. }),
                "{name} must be refused as a forbidden target, got {error}"
            );
        }
    }

    // Paired positive: the rule matches the `localhost` suffix, not the label.
    #[tokio::test]
    async fn an_ordinary_name_starting_with_localhost_is_not_refused_by_the_name_rule() {
        let rules = ProxyRules::proxied_everywhere(PROXY);
        let route = guard_destination(DialScheme::Https, "localhost.example", 5999, &[], &rules)
            .await
            .expect("localhost.example is an ordinary name, not a loopback name");
        assert!(matches!(route, DialRoute::Proxied));
    }

    // Trust still wins over the name rule, as it does over the literal one.
    #[tokio::test]
    async fn a_trusted_loopback_name_is_admitted_on_a_proxied_route() {
        let rules = ProxyRules::proxied_everywhere(PROXY);
        let trusted = vec!["localhost".to_string()];
        let route = guard_destination(DialScheme::Https, "localhost", 5999, &trusted, &rules)
            .await
            .expect("a trusted host skips the loopback-name refusal");
        assert!(matches!(route, DialRoute::Proxied));
    }

    // Plan row 2 (ordering): `trusted_hosts` is consulted BEFORE the literal
    // check, so an operator-listed loopback registry still reaches its proxy.
    #[tokio::test]
    async fn a_trusted_host_is_admitted_on_a_proxied_route_before_the_literal_check() {
        let rules = ProxyRules::proxied_everywhere(PROXY);
        let trusted = vec!["127.0.0.1".to_string()];
        let route = guard_destination(DialScheme::Https, "127.0.0.1", 5000, &trusted, &rules)
            .await
            .expect("a trusted host skips the forbidden-literal refusal");
        assert!(matches!(route, DialRoute::Proxied));
    }

    // The normalisation must reach the proxy match too, not only the literal
    // check: `NO_PROXY=127.0.0.1` excludes `0x7f000001`, which then takes the
    // direct route and meets the full resolving floor.
    #[tokio::test]
    async fn a_no_proxy_entry_matches_an_alternate_spelling_of_the_same_address() {
        let rules = ProxyRules::new(Matcher::builder().all(PROXY).no("127.0.0.1").build());
        assert_eq!(
            rules.dial_route(DialScheme::Https, "0x7f000001", 5000),
            Route::Direct,
            "the NO_PROXY entry is matched against the normalised host"
        );

        // Control: the same spelling without the NO_PROXY entry IS proxied, so
        // the verdict above is the rule matching and not a parse failure.
        assert_eq!(
            ProxyRules::proxied_everywhere(PROXY).dial_route(DialScheme::Https, "0x7f000001", 5000),
            Route::Proxied,
            "control: only the NO_PROXY entry makes this spelling direct"
        );

        // Either failure is the floor holding: glibc's `getaddrinfo` accepts
        // the hex form and refuses the loopback it names, a stricter resolver
        // rejects the spelling outright. The route assertion and its control
        // above are what prove the normalisation; this only proves the direct
        // arm fails closed.
        let error = guard_destination(DialScheme::Https, "0x7f000001", 5000, &[], &rules)
            .await
            .expect_err("the direct route resolves the destination itself");
        assert!(
            matches!(error, SsrfError::ForbiddenTarget { .. } | SsrfError::Resolution { .. }),
            "the direct floor must fail closed on this spelling, got {error}"
        );
    }

    // The `Direct` arm carries the floor-checked addresses the caller pins.
    #[tokio::test]
    async fn a_direct_destination_carries_its_validated_addresses() {
        let route = guard_destination(DialScheme::Https, "8.8.8.8", 443, &[], &ProxyRules::direct())
            .await
            .expect("a public address passes the floor");
        let DialRoute::Direct(addresses) = route else {
            panic!("no proxy is configured, so the route must be direct");
        };
        assert!(
            addresses.contains(&SocketAddr::from(([8, 8, 8, 8], 443))),
            "the validated addresses are what the caller pins, got {addresses:?}"
        );
    }

    // Plan row 3: a `NO_PROXY` match takes the direct route, which still fails
    // closed on an unresolvable host.
    #[tokio::test]
    async fn a_no_proxy_match_takes_the_direct_route_and_still_fails_closed() {
        let rules = ProxyRules::new(Matcher::builder().all(PROXY).no(PHANTOM).build());
        assert_eq!(
            rules.dial_route(DialScheme::Https, PHANTOM, 5000),
            Route::Direct,
            "a NO_PROXY entry excludes the host from the proxy"
        );

        // Control: the same host without the NO_PROXY entry IS proxied, so the
        // verdict above is the rule matching and not a blanket `Direct`.
        let without_no_proxy = ProxyRules::proxied_everywhere(PROXY);
        assert_eq!(
            without_no_proxy.dial_route(DialScheme::Https, PHANTOM, 5000),
            Route::Proxied,
            "control: only the NO_PROXY entry makes this host direct"
        );

        let error = guard_destination(DialScheme::Https, PHANTOM, 5000, &[], &rules)
            .await
            .expect_err("the direct route resolves the destination itself");
        assert!(
            matches!(error, SsrfError::Resolution { .. }),
            "an unresolvable direct destination fails closed, got {error}"
        );
    }

    // Plan row 4: the dial scheme picks the proxy variable — an HTTP-only
    // proxy leaves HTTPS dials direct.
    #[test]
    fn an_http_only_proxy_intercepts_http_dials_and_leaves_https_direct() {
        let rules = ProxyRules::new(Matcher::builder().http(PROXY).build());
        assert_eq!(rules.dial_route(DialScheme::Http, "registry.corp", 80), Route::Proxied);
        assert_eq!(
            rules.dial_route(DialScheme::Https, "registry.corp", 443),
            Route::Direct,
            "an HTTP-only proxy must not claim an HTTPS dial"
        );
    }

    // Plan row 4 (mirror): an HTTPS-only proxy leaves HTTP dials direct.
    #[test]
    fn an_https_only_proxy_intercepts_https_dials_and_leaves_http_direct() {
        let rules = ProxyRules::new(Matcher::builder().https(PROXY).build());
        assert_eq!(
            rules.dial_route(DialScheme::Https, "registry.corp", 443),
            Route::Proxied
        );
        assert_eq!(
            rules.dial_route(DialScheme::Http, "registry.corp", 80),
            Route::Direct,
            "an HTTPS-only proxy must not claim an HTTP dial"
        );
    }

    // Plan row 5: an `ALL_PROXY`-style fallback covers both schemes.
    #[test]
    fn an_all_proxy_intercepts_both_schemes() {
        let rules = ProxyRules::proxied_everywhere(PROXY);
        assert_eq!(rules.dial_route(DialScheme::Http, "registry.corp", 80), Route::Proxied);
        assert_eq!(
            rules.dial_route(DialScheme::Https, "registry.corp", 443),
            Route::Proxied
        );
    }

    // Plan row 6: the proxy-host set holds the configured authorities' host
    // names, ascii-lowercased, with port and userinfo stripped.
    #[test]
    fn the_proxy_host_set_holds_the_configured_authority_host_names() {
        let rules = ProxyRules::new(
            Matcher::builder()
                .http("http://Proxy.CORP:3128")
                .https("http://user:pw@secure-proxy.corp:8080")
                .build(),
        );
        assert!(rules.is_proxy_host("proxy.corp"), "the HTTP proxy host is known");
        assert!(
            rules.is_proxy_host("secure-proxy.corp"),
            "the HTTPS proxy host is known"
        );
        assert!(
            rules.is_proxy_host("PrOxY.CoRp"),
            "DNS names are case-insensitive, so the query is folded like the set"
        );
        assert!(
            !rules.is_proxy_host("proxy.corp:3128"),
            "the set holds host names, not authorities"
        );
        assert!(!rules.is_proxy_host("user"), "proxy credentials are not host names");
        assert!(
            !rules.is_proxy_host("registry.corp"),
            "a destination is not a proxy host"
        );
    }

    // Plan row 6: `NO_PROXY=*` disables every proxy, so there is no proxy host
    // to admit.
    #[test]
    fn a_no_proxy_wildcard_leaves_no_proxy_host_to_admit() {
        let disabled = ProxyRules::new(Matcher::builder().all(PROXY).no("*").build());
        assert!(
            !disabled.is_proxy_host("proxy.corp"),
            "NO_PROXY=* means no dial is ever proxied"
        );

        // Control: the same proxy without the wildcard IS admitted, so the
        // verdict above is the wildcard and not a blanket `false`.
        let enabled = ProxyRules::proxied_everywhere(PROXY);
        assert!(
            enabled.is_proxy_host("proxy.corp"),
            "control: only NO_PROXY=* empties the proxy-host set"
        );
    }

    // Plan row 7: the resolver hook admits its own proxy host with a plain
    // lookup — the proxy is operator config and RFC1918 by nature (ocx#323).
    #[tokio::test]
    async fn guarded_resolver_admits_its_own_proxy_host_despite_a_forbidden_address() {
        use reqwest::dns::Resolve;
        let resolver = GuardedResolver::new(
            Arc::new(Vec::new()),
            ProxyRules::proxied_everywhere("http://localhost:3128"),
        );
        let name: reqwest::dns::Name = "localhost".parse().expect("valid dns name");
        let addresses = resolver
            .resolve(name)
            .await
            .expect("the configured proxy host is admitted without a range judgement");
        assert!(
            addresses.into_iter().any(|address| address.ip().is_loopback()),
            "the proxy host resolves to loopback and is still admitted"
        );
    }

    // Plan row 8: every other name keeps the SSRF floor — paired positive of
    // the test above, and of `guarded_resolver_refuses_forbidden_host_at_connect`.
    #[tokio::test]
    async fn guarded_resolver_refuses_a_loopback_name_that_is_not_the_proxy() {
        use reqwest::dns::Resolve;
        let resolver = GuardedResolver::new(Arc::new(Vec::new()), ProxyRules::proxied_everywhere(PROXY));
        let name: reqwest::dns::Name = "localhost".parse().expect("valid dns name");
        let error = resolver
            .resolve(name)
            .await
            .err()
            .expect("a non-proxy loopback name is still refused");
        assert!(
            error.to_string().contains("resolves to a forbidden address"),
            "the refusal keeps the SSRF guard's wording, got {error}"
        );
    }

    // Design: `DialScheme::for_registry` is the ONE site deciding the dial
    // scheme — `Http` iff the authority is listed in `OCX_INSECURE_REGISTRIES`.
    #[test]
    fn for_registry_picks_http_only_for_a_listed_insecure_registry() {
        let insecure = vec!["localhost:5000".to_string()];
        assert_eq!(DialScheme::for_registry(&insecure, "localhost:5000"), DialScheme::Http);
        assert_eq!(
            DialScheme::for_registry(&insecure, "localhost"),
            DialScheme::Https,
            "the entry is the authority, port included"
        );
        assert_eq!(DialScheme::for_registry(&insecure, "ghcr.io"), DialScheme::Https);
        assert_eq!(
            DialScheme::for_registry(&[], "localhost:5000"),
            DialScheme::Https,
            "an empty insecure list keeps every dial on https"
        );
    }
}
