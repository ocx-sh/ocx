// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Default-on SSRF guard for remote-controlled registry hosts: before OCX
//! dereferences an index root's `repository` pointer, the host must not resolve
//! to a private, loopback, link-local or metadata address.
//!
//! The connect pins the validated IPs, never the hostname, or DNS rebinding
//! reopens the window between check and dial.

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, LazyLock};

use hyper_util::client::proxy::matcher::{Intercept, Matcher};
use url::{Host, Url};

/// Whether `host` carries a plain-HTTP allowance: byte-exact on `host[:port]`.
///
/// Must match `oci_client::ClientProtocol::HttpsExcept`, or the fork and ocx
/// disagree on which hosts dial plain HTTP.
#[must_use]
pub fn allows_plain_http(hosts: &[String], host: &str) -> bool {
    hosts.iter().any(|allowed| allowed == host)
}

/// A physical host was refused, or could not be resolved, by the SSRF guard.
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
pub enum SsrfError {
    #[error("host {host} resolves to a forbidden address {ip}; add it to trusted_hosts to allow")]
    // The Sigstore wrapper exits 64 for this (`From<SsrfError> for UrlRejection` in `endpoint.rs`); change both together.
    #[exit(
        ConfigError,
        slug = "ssrf_forbidden_target",
        summary = "A registry host resolves to an address outside the trusted set"
    )]
    ForbiddenTarget { host: String, ip: IpAddr },

    #[error("failed to resolve host {host}")]
    // A DNS failure at connect time is already 75; no portable split of NXDOMAIN from a flaky resolver.
    #[exit(
        TempFail,
        slug = "host_resolution_failed",
        summary = "A registry host could not be resolved"
    )]
    Resolution {
        host: String,
        #[source]
        source: std::io::Error,
    },
}

/// True for addresses OCX refuses to reach from a remote-controlled host.
///
/// Every IPv6 form embedding an IPv4 target is judged by that address, or an
/// encoding smuggles a forbidden target past the guard.
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

/// `100.64.0.0/10` (RFC 6598); hand-rolled while `Ipv4Addr::is_shared` is unstable.
fn is_shared_cgnat(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    octets[0] == 100 && (octets[1] & 0xc0) == 0x40
}

fn is_benchmarking(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    octets[0] == 198 && (octets[1] & 0xfe) == 18
}

fn is_reserved(ip: Ipv4Addr) -> bool {
    ip.octets()[0] >= 240
}

fn is_forbidden_v6(ip: Ipv6Addr) -> bool {
    // Judged as IPv4, or `::ffff:127.0.0.1` bypasses the IPv4 range checks.
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

/// `::a.b.c.d` and NAT64 `64:ff9b::a.b.c.d` (RFC 6052) carry an IPv4 target.
///
/// Judged last, never via `to_ipv4` at the top: `::1` is inside `::/96`, and
/// unwrapping it first reads it as the allowed `0.0.0.1`.
fn embeds_forbidden_v4(ip: Ipv6Addr) -> bool {
    let [prefix @ .., high, low] = ip.segments();
    if prefix != [0, 0, 0, 0, 0, 0] && prefix != [0x64, 0xff9b, 0, 0, 0, 0] {
        return false;
    }
    is_forbidden_v4(Ipv4Addr::from((u32::from(high) << 16) | u32::from(low)))
}

/// `2001:db8::/32` (RFC 3849); hand-rolled while `Ipv6Addr::is_documentation` is unstable.
fn is_documentation_v6(ip: Ipv6Addr) -> bool {
    let segments = ip.segments();
    segments[0] == 0x2001 && segments[1] == 0x0db8
}

/// Whether `host` exactly matches a `trusted_hosts` entry, or is an IP literal inside a trusted CIDR.
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

/// Splits a registry authority into `(host, port)`, defaulting to `443`.
///
/// A bracketed IPv6 authority stays `"[::1]"`, which both guard routes refuse;
/// any bracket stripping lands here, or the two routes judge different hosts.
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

/// Resolves `host`, refusing every forbidden address unless `host` is trusted.
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

/// A [`reqwest::dns::Resolve`] hook running [`resolve_and_validate`] at connect time.
///
/// Exempts only the process's own proxy host: a corporate proxy is RFC1918, so
/// the floor would refuse every call; widening it past that one host reopens SSRF.
pub struct GuardedResolver {
    trusted: Arc<Vec<String>>,
    rules: Arc<ProxyRules>,
    pinned: Option<Arc<PinnedName>>,
}

/// A name the caller already resolved, and its answer.
struct PinnedName {
    name: String,
    addresses: Vec<SocketAddr>,
}

impl GuardedResolver {
    pub fn new(trusted: Arc<Vec<String>>, rules: Arc<ProxyRules>) -> Self {
        Self {
            trusted,
            rules,
            pinned: None,
        }
    }

    /// Answers `name` with `addresses`, without a lookup and without the range floor.
    ///
    /// For the operator-configured proxy a caller resolved itself, within its own deadline. The name
    /// is matched ASCII case-insensitively: hyper asks with the proxy URL's spelling, and a
    /// case-sensitive match would send a mixed-case proxy host to a lookup on the blocking pool.
    ///
    /// `name` must come from operator configuration (the proxy the environment names), never from
    /// remote data: the pinned answer bypasses the address floor.
    #[must_use]
    pub fn with_pinned(mut self, name: &str, addresses: Vec<SocketAddr>) -> Self {
        self.pinned = Some(Arc::new(PinnedName {
            name: name.to_owned(),
            addresses,
        }));
        self
    }
}

impl reqwest::dns::Resolve for GuardedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let trusted = self.trusted.clone();
        let rules = self.rules.clone();
        let pinned = self.pinned.clone();
        Box::pin(async move {
            // Port 0: reqwest replaces it with the request URL's after resolution.
            let host = name.as_str();
            let addresses: Vec<SocketAddr> =
                if let Some(pinned) = pinned.filter(|pinned| pinned.name.eq_ignore_ascii_case(host)) {
                    pinned.addresses.clone()
                } else if rules.is_proxy_host(host) {
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

/// The scheme OCX will dial a registry over; it picks `HTTP_PROXY` vs `HTTPS_PROXY`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialScheme {
    Http,
    Https,
}

impl DialScheme {
    /// `Http` iff [`allows_plain_http`] admits `registry`.
    #[must_use]
    pub fn for_registry(insecure_hosts: &[String], registry: &str) -> Self {
        if allows_plain_http(insecure_hosts, registry) {
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
    Direct,
    Proxied,
}

/// The outcome of [`guard_destination`]; `Direct` carries the validated addresses to pin.
#[derive(Debug)]
pub enum DialRoute {
    Direct(Vec<SocketAddr>),
    Proxied,
}

/// The system/env HTTP proxy matcher reqwest itself consults, plus the proxy hosts it can pick.
pub struct ProxyRules {
    matcher: Matcher,
    /// Lowercase host names, at most one per scheme.
    proxy_hosts: BTreeSet<String>,
}

/// Never dialed: a name no `NO_PROXY` rule lists, to ask which proxy each scheme picks.
const PROXY_PROBES: [&str; 2] = ["http://ocx-proxy-probe/", "https://ocx-proxy-probe/"];

impl ProxyRules {
    #[must_use]
    pub fn from_system() -> Self {
        Self::new(Matcher::from_system())
    }

    /// Build from an explicit matcher instead of the ambient environment.
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

    /// Whether `host:port` is dialed through a proxy; an unparseable host is
    /// `Direct`, which keeps the full floor on it (fail closed).
    #[must_use]
    pub fn dial_route(&self, scheme: DialScheme, host: &str, port: u16) -> Route {
        normalised_destination(scheme, host, port).map_or(Route::Direct, |destination| self.route_for(&destination))
    }

    /// Takes the normalised destination, or the matcher and the literal check judge different hosts.
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

    /// Whether `name` is one of this process's configured proxy hosts (case-insensitive).
    #[must_use]
    pub fn is_proxy_host(&self, name: &str) -> bool {
        self.proxy_hosts.iter().any(|proxy| proxy.eq_ignore_ascii_case(name))
    }

    /// The host and port of the proxy a dial to `host:port` goes through, `None` when no proxy
    /// intercepts it.
    ///
    /// Asks the matcher about the destination itself, so a `NO_PROXY` rule decides here exactly as it
    /// does for the dial. The port is the one the proxy URL names, else the default hyper-util dials
    /// for its scheme. A caller that resolves the proxy itself pins this name
    /// ([`GuardedResolver::with_pinned`]) instead of leaving the lookup to the resolver.
    #[must_use]
    pub fn proxy_endpoint(&self, scheme: DialScheme, host: &str, port: u16) -> Option<(String, u16)> {
        let destination = normalised_destination(scheme, host, port)?
            .as_str()
            .parse::<http::Uri>()
            .ok()?;
        let intercept = self.matcher.intercept(&destination)?;
        let uri = intercept.uri();
        let port = uri.port_u16().unwrap_or(match uri.scheme_str() {
            Some("https") => 443,
            _ => 80,
        });
        Some((uri.host()?.to_ascii_lowercase(), port))
    }

    /// The proxy a dial to `host:port` goes through, with the credentials split off its URL, `None` when
    /// no proxy intercepts it.
    ///
    /// For a client that dials only that one destination: it builds its own proxy from this answer and
    /// turns reqwest's OS proxy read off, so the route this process judged is the route the client takes.
    #[must_use]
    pub fn proxy_choice(&self, scheme: DialScheme, host: &str, port: u16) -> Option<ProxyChoice> {
        let destination = normalised_destination(scheme, host, port)?
            .as_str()
            .parse::<http::Uri>()
            .ok()?;
        self.matcher.intercept(&destination).map(ProxyChoice)
    }
}

/// The proxy [`ProxyRules::proxy_choice`] picked. No `Debug`: the credentials are secrets.
pub struct ProxyChoice(Intercept);

impl ProxyChoice {
    /// The proxy URL, without credentials.
    #[must_use]
    pub fn uri(&self) -> String {
        self.0.uri().to_string()
    }

    /// The `Proxy-Authorization` value of a basic-auth proxy.
    #[must_use]
    pub fn basic_auth(&self) -> Option<http::HeaderValue> {
        self.0.basic_auth().cloned()
    }

    /// The user and password of a proxy whose scheme sends them itself, such as SOCKS.
    #[must_use]
    pub fn raw_auth(&self) -> Option<(&str, &str)> {
        self.0.raw_auth()
    }
}

#[cfg(any(test, feature = "__testing"))]
impl ProxyRules {
    /// No proxy is configured: every dial takes [`Route::Direct`].
    pub fn direct() -> Arc<Self> {
        Arc::new(Self::new(Matcher::builder().build()))
    }

    /// `proxy` intercepts every scheme, the way `ALL_PROXY` does.
    pub fn proxied_everywhere(proxy: &str) -> Arc<Self> {
        Arc::new(Self::new(Matcher::builder().all(proxy).build()))
    }
}

/// The destination as reqwest will parse it: judging the raw spelling lets
/// `0x7f000001` past the checks while the transport dials loopback.
fn normalised_destination(scheme: DialScheme, host: &str, port: u16) -> Option<Url> {
    let scheme = match scheme {
        DialScheme::Http => "http",
        DialScheme::Https => "https",
    };
    let destination = if host.parse::<Ipv6Addr>().is_ok() {
        format!("{scheme}://[{host}]:{port}/")
    } else {
        format!("{scheme}://{host}:{port}/")
    };
    Url::parse(&destination).ok()
}

/// Process-wide [`ProxyRules`]: the one proxy-env reader, or the guard and the dial disagree on the route.
#[must_use]
pub fn proxy_rules() -> Arc<ProxyRules> {
    static RULES: LazyLock<Arc<ProxyRules>> = LazyLock::new(|| Arc::new(ProxyRules::from_system()));
    RULES.clone()
}

/// `localhost` and `*.localhost` are loopback by definition (RFC 6761 §6.3).
fn is_loopback_name(name: &str) -> bool {
    let name = name.strip_suffix('.').unwrap_or(name).to_ascii_lowercase();
    name == "localhost" || name.ends_with(".localhost")
}

/// The route-aware SSRF floor: [`resolve_and_validate`] on a direct route.
///
/// A proxied route does no DNS (it would judge the wrong address on a
/// proxy-only-DNS network) and refuses only a forbidden literal or loopback name.
///
/// # Errors
///
/// [`SsrfError::ForbiddenTarget`] for a forbidden address; [`SsrfError::Resolution`]
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
                // Without this, a proxy on the caller's own machine forwards
                // `CONNECT localhost:5999` from index data to the caller's loopback.
                Some(Host::Domain(name)) if is_loopback_name(name) => Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
                // Residual: an internal name (`intranet.corp`) is admitted; only the proxy's egress can refuse it.
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

/// What the dial-site SSRF floor needs to judge a rewritten target in one logical namespace.
pub struct DialPolicy<'a> {
    pub insecure_hosts: &'a [String],
    /// Keyed on the **logical** registry, never the physical one (ocx-sh/ocx#455).
    pub trusted_hosts: &'a [String],
    pub rules: &'a ProxyRules,
}

/// A rewritten physical target was refused at the dial site.
///
/// The pipelines fold this wording into `ForbiddenRegistryTarget` verbatim
/// (`physical_dial_refusal_renders_the_index_wording`).
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
#[error(
    "the physical host of {namespace}/… was refused; list it (bare host, no port) under [registries.\"{namespace}\"].trusted_hosts"
)]
// The verdict is the wrapped `SsrfError`'s, slug included.
#[exit(delegate = source)]
pub struct PhysicalDialRefused {
    pub namespace: String,
    #[source]
    pub source: SsrfError,
}

/// SSRF floor for a **rewritten** physical target, applied just before the first dial.
///
/// Fails closed on a lookup failure, unlike the resolve-time half: an answer
/// appearing only between check and dial is the rebinding attack.
///
/// # Errors
///
/// [`PhysicalDialRefused`] when the physical host resolves into a forbidden
/// range without a `trusted_hosts` entry, or — on a direct dial only — cannot
/// be resolved at all.
pub async fn guard_physical_dial(
    policy: &DialPolicy<'_>,
    logical: &crate::PackageRef,
    physical: &crate::OciIdentifier,
) -> Result<(), PhysicalDialRefused> {
    // Not a rewrite: guarding it would refuse every private registry.
    if physical.registry() == logical.registry() {
        return Ok(());
    }
    let (host, port) = split_host_port(physical.registry());
    guard_destination(
        DialScheme::for_registry(policy.insecure_hosts, physical.registry()),
        host,
        port,
        policy.trusted_hosts,
        policy.rules,
    )
    .await
    .map_err(|source| PhysicalDialRefused {
        namespace: logical.registry().to_string(),
        source,
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    /// The dial refusal renders exactly the wording `crate::index::Error::Ssrf`
    /// carried before ADR 1.9 moved it, compared against the literal rather
    /// than against the variant.
    ///
    /// Load-bearing three times over. The sign, verify and attest pipelines
    /// fold this string into `ForbiddenRegistryTarget { reason }`, which is
    /// what an operator reads and what the acceptance suite asserts; the
    /// variant is `#[error(transparent)]` over this type, so comparing the two
    /// would compare the wording with itself; and the entry it names is the
    /// one an operator has to add, keyed on the LOGICAL namespace (ocx#455).
    #[test]
    fn physical_dial_refusal_renders_the_index_wording() {
        let refused = PhysicalDialRefused {
            namespace: "ocx.sh".to_owned(),
            source: SsrfError::ForbiddenTarget {
                host: "127.0.0.1".to_owned(),
                ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
            },
        };
        assert_eq!(
            refused.to_string(),
            "the physical host of ocx.sh/… was refused; list it (bare host, no port) under \
             [registries.\"ocx.sh\"].trusted_hosts"
        );
    }

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

    #[test]
    fn the_proxy_endpoint_is_the_host_and_port_each_scheme_picks() {
        let rules = ProxyRules::new(
            Matcher::builder()
                .http("http://Proxy.CORP:3128")
                .https("http://user:pw@secure-proxy.corp")
                .build(),
        );
        assert_eq!(
            rules.proxy_endpoint(DialScheme::Http, "collector.example", 4318),
            Some(("proxy.corp".to_owned(), 3128))
        );
        assert_eq!(
            rules.proxy_endpoint(DialScheme::Https, "collector.example", 4318),
            Some(("secure-proxy.corp".to_owned(), 80)),
            "a proxy URL without a port takes its own scheme's default"
        );
        let direct = ProxyRules::direct();
        assert_eq!(direct.proxy_endpoint(DialScheme::Http, "collector.example", 4318), None);
        let http_only = ProxyRules::new(Matcher::builder().http("http://proxy.corp:3128").build());
        assert_eq!(
            http_only.proxy_endpoint(DialScheme::Https, "collector.example", 4318),
            None
        );
        let disabled = ProxyRules::new(Matcher::builder().all(PROXY).no("*").build());
        assert_eq!(
            disabled.proxy_endpoint(DialScheme::Http, "collector.example", 4318),
            None
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

    // A caller that resolved the proxy itself pins the answer; hyper asks with the proxy URL's own
    // spelling, so the match ignores ASCII case and no lookup runs (`.invalid` cannot resolve).
    #[tokio::test]
    async fn guarded_resolver_answers_a_pinned_proxy_name_in_any_case_without_a_lookup() {
        use reqwest::dns::Resolve;
        let answer: SocketAddr = "10.0.0.2:3128".parse().expect("literal");
        let resolver = GuardedResolver::new(
            Arc::new(Vec::new()),
            ProxyRules::proxied_everywhere("http://Proxy.Example.invalid:3128"),
        )
        .with_pinned("proxy.example.invalid", vec![answer]);
        for spelling in [
            "proxy.example.invalid",
            "Proxy.Example.Invalid",
            "PROXY.EXAMPLE.INVALID",
        ] {
            let name: reqwest::dns::Name = spelling.parse().expect("valid dns name");
            let addresses: Vec<SocketAddr> = resolver
                .resolve(name)
                .await
                .unwrap_or_else(|error| panic!("{spelling} must take the pinned answer, got {error}"))
                .collect();
            assert_eq!(addresses, [answer], "{spelling}");
        }
        // Control: the pin covers its own name only.
        let other: reqwest::dns::Name = "other.invalid".parse().expect("valid dns name");
        assert!(resolver.resolve(other).await.is_err(), "another name is not pinned");
    }

    // The proxy is the one the matcher picks for the real destination, so a `NO_PROXY` rule that
    // names the destination leaves it direct, and one that names anything else does not.
    #[test]
    fn proxy_endpoint_follows_the_destination_not_a_stand_in_name() {
        let rules = ProxyRules::new(Matcher::builder().all(PROXY).no("collector.example").build());
        assert_eq!(rules.proxy_endpoint(DialScheme::Https, "collector.example", 4318), None);
        assert_eq!(
            rules.proxy_endpoint(DialScheme::Https, "other.example", 4318),
            Some(("proxy.corp".to_owned(), 3128))
        );
        let probe_only = ProxyRules::new(Matcher::builder().all(PROXY).no("ocx-proxy-probe").build());
        assert_eq!(
            probe_only.proxy_endpoint(DialScheme::Http, "collector.example", 4318),
            Some(("proxy.corp".to_owned(), 3128)),
            "a rule naming only the stand-in host must not hide the proxy"
        );
    }

    #[test]
    fn proxy_choice_follows_the_route_the_rules_judge() {
        let rules = ProxyRules::new(
            Matcher::builder()
                .all("http://user:pa55@proxy.corp:3128")
                .no("collector.example")
                .build(),
        );
        assert!(
            rules
                .proxy_choice(DialScheme::Https, "collector.example", 4318)
                .is_none()
        );
        let choice = rules
            .proxy_choice(DialScheme::Https, "other.example", 4318)
            .expect("a proxy intercepts it");
        assert_eq!(choice.uri(), "http://proxy.corp:3128/");
        assert!(choice.basic_auth().is_some(), "the credentials are split off the URL");
        assert!(
            ProxyRules::direct()
                .proxy_choice(DialScheme::Http, "collector.example", 4318)
                .is_none()
        );
    }

    // A proxy URL without a port is dialled on its own scheme's default by hyper-util: 80 for every
    // scheme but https, which is 443. A socks URL is no exception.
    #[test]
    fn proxy_endpoint_without_a_port_takes_the_default_hyper_util_dials() {
        for (proxy, expected) in [
            ("http://proxy.corp", 80),
            ("https://proxy.corp", 443),
            ("socks5://proxy.corp", 80),
            ("socks5h://proxy.corp", 80),
            ("socks5h://proxy.corp:9050", 9050),
        ] {
            let rules = ProxyRules::new(Matcher::builder().all(proxy).build());
            assert_eq!(
                rules.proxy_endpoint(DialScheme::Https, "collector.example", 4318),
                Some(("proxy.corp".to_owned(), expected)),
                "{proxy}"
            );
        }
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
