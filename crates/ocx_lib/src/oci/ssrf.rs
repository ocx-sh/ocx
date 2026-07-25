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
//! `trusted_hosts` (configured per `[registries."<ns>"]`) is the explicit escape
//! hatch: a listed host or CIDR skips validation, so a private corporate registry
//! reaches its own private index without disabling the guard globally. Host
//! *allowlisting* (which hosts may appear in roots at all) stays index-side
//! governance — this module only enforces the SSRF floor.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;

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
            // error (78), not a transient fault.
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
/// (`::`), multicast (`ff00::/8`), documentation (`2001:db8::/32`). An
/// IPv4-mapped IPv6 address (`::ffff:a.b.c.d`) is unwrapped and judged by its
/// embedded IPv4 address, so the mapping cannot smuggle a forbidden target
/// past the guard.
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
/// (`[::1]:5000`) yields the host `"[::1]"`, which parses as neither an IP
/// literal nor a DNS name, so [`resolve_and_validate`] refuses it with
/// [`SsrfError::Resolution`]. That fails **closed**. Any future bracket
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
pub struct GuardedResolver {
    trusted: Arc<Vec<String>>,
}

impl GuardedResolver {
    /// Builds a resolver that exempts the hosts / CIDRs in `trusted`.
    pub fn new(trusted: Arc<Vec<String>>) -> Self {
        Self { trusted }
    }
}

impl reqwest::dns::Resolve for GuardedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let trusted = self.trusted.clone();
        Box::pin(async move {
            // reqwest overrides the port from the request URL after resolution, so
            // any placeholder here is discarded; only the validated IPs matter.
            let host = name.as_str().to_string();
            let addresses = resolve_and_validate(&host, 0, &trusted).await?;
            let addresses: reqwest::dns::Addrs = Box::new(addresses.into_iter());
            Ok(addresses)
        })
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
        // neither a parseable IP literal nor a resolvable DNS name, so
        // `resolve_and_validate` refuses it (`SsrfError::Resolution`) instead of
        // dialing loopback — it fails CLOSED. Asserted locally (no DNS) via the
        // two properties that make it fail closed.
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
        let resolver = GuardedResolver::new(Arc::new(Vec::new()));
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
}
