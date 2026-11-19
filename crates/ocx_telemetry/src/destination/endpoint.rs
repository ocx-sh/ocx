// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Endpoint validation shared by every destination source.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use url::{Host, Url};

/// How the logs path joins the endpoint's own path.
#[derive(Debug, Clone, Copy)]
pub(super) enum EndpointPath {
    /// The endpoint already names the logs resource.
    AsGiven,
    /// `/v1/logs` follows the endpoint's path, with exactly one `/` between them.
    AppendLogs,
}

/// The endpoint as a URL, or `None` when it cannot be used: not `http`/`https`, no host,
/// user information, or an IP-literal host that [`is_refused_address`] names.
pub(super) fn parse(raw: &str, path: EndpointPath) -> Option<Url> {
    let mut url = Url::parse(raw).ok()?;
    if !matches!(url.scheme(), "http" | "https") || !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    match url.host()? {
        Host::Domain("") => return None,
        Host::Domain(_) => {}
        Host::Ipv4(address) if is_refused_address(IpAddr::V4(address)) => return None,
        Host::Ipv6(address) if is_refused_address(IpAddr::V6(address)) => return None,
        Host::Ipv4(_) | Host::Ipv6(_) => {}
    }
    if matches!(path, EndpointPath::AppendLogs) {
        let joined = format!("{}/v1/logs", url.path().trim_end_matches('/'));
        url.set_path(&joined);
        url.set_fragment(None);
    }
    Some(url)
}

/// The addresses an endpoint may never name: `169.254.0.0/16`, `fe80::/10` and the two cloud metadata
/// addresses outside them, AWS's `fd00:ec2::254` and Alibaba's `100.100.100.200`. Only those two, not
/// their unique-local and CGNAT ranges, which are legitimate collector homes. Judged through every
/// IPv6 form that embeds an IPv4 target, or `[::169.254.169.254]` walks past the IPv4 check.
pub(crate) fn is_refused_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => is_refused_v4(address),
        IpAddr::V6(address) => {
            embedded_v4(address).iter().any(|v4| is_refused_v4(*v4))
                || is_unicast_link_local(address)
                || address == AWS_METADATA_V6
        }
    }
}

const AWS_METADATA_V6: Ipv6Addr = Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254);
const ALIBABA_METADATA_V4: Ipv4Addr = Ipv4Addr::new(100, 100, 100, 200);

fn is_refused_v4(address: Ipv4Addr) -> bool {
    address.is_link_local() || address == ALIBABA_METADATA_V4
}

/// The IPv4 addresses an IPv6 literal may carry: IPv4-mapped, IPv4-translated `::ffff:0:0:0/96`,
/// IPv4-compatible `::/96`, NAT64 `64:ff9b::/96` and local-use NAT64 `64:ff9b:1::/48`. `::` and `::1`
/// read as `0.0.0.0` and `0.0.0.1`, neither refused.
///
/// The local-use block yields one reading per RFC 6052 layout an operator may pick inside it: `/48`,
/// `/56`, `/64` and `/96` (the trailing 32 bits). Any of them could hide the target.
fn embedded_v4(address: Ipv6Addr) -> Vec<Ipv4Addr> {
    if let Some(mapped) = address.to_ipv4_mapped() {
        return vec![mapped];
    }
    let segments = address.segments();
    let [prefix @ .., high, low] = segments;
    let trailing = Ipv4Addr::from((u32::from(high) << 16) | u32::from(low));
    if prefix == [0; 6] || prefix == [0x64, 0xff9b, 0, 0, 0, 0] || prefix == [0, 0, 0, 0, 0xffff, 0] {
        return vec![trailing];
    }
    if segments[..3] == [0x64, 0xff9b, 1] {
        let octets = address.octets();
        return vec![
            Ipv4Addr::new(octets[6], octets[7], octets[9], octets[10]),
            Ipv4Addr::new(octets[7], octets[9], octets[10], octets[11]),
            Ipv4Addr::new(octets[9], octets[10], octets[11], octets[12]),
            trailing,
        ];
    }
    Vec::new()
}

fn is_unicast_link_local(address: Ipv6Addr) -> bool {
    address.segments()[0] & 0xffc0 == 0xfe80
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logs_path_joins_with_exactly_one_slash() {
        for (raw, expected) in [
            ("https://c.example:4318", "https://c.example:4318/v1/logs"),
            ("https://c.example:4318/", "https://c.example:4318/v1/logs"),
            ("https://c.example/otlp", "https://c.example/otlp/v1/logs"),
            ("https://c.example/otlp/", "https://c.example/otlp/v1/logs"),
        ] {
            let url = parse(raw, EndpointPath::AppendLogs).unwrap_or_else(|| panic!("{raw} is usable"));
            assert_eq!(url.as_str(), expected, "{raw}");
        }
    }

    #[test]
    fn a_logs_endpoint_is_used_as_written() {
        let url = parse("http://logs.example:4318/custom", EndpointPath::AsGiven).expect("usable");
        assert_eq!(url.as_str(), "http://logs.example:4318/custom");
    }

    #[test]
    fn unusable_endpoints_are_refused() {
        for raw in [
            "",
            "collector.example",
            "ftp://c.example",
            "file:///tmp/x",
            "https://user@c.example",
            "https://user:pw@c.example",
            "https://",
        ] {
            assert!(parse(raw, EndpointPath::AppendLogs).is_none(), "{raw:?}");
        }
    }

    #[test]
    fn link_local_literals_are_refused_and_their_neighbours_are_not() {
        for raw in [
            "http://169.254.169.254/latest",
            "http://169.254.0.1",
            "http://[fe80::1]:4318",
            "http://[febf::1]",
            "http://[::ffff:169.254.169.254]",
            "http://[::169.254.169.254]/",
            "http://[64:ff9b::169.254.169.254]/",
            "http://[::ffff:0:169.254.169.254]/",
            "http://[64:ff9b:1::169.254.169.254]/",
            "http://[64:ff9b:1:a9fe:a9:fe00::]/",
            "http://[64:ff9b:1:a9:fe:a9fe::]/",
            "http://[64:ff9b:1:0:a9:fea9:fe00:0]/",
        ] {
            assert!(parse(raw, EndpointPath::AppendLogs).is_none(), "{raw}");
        }
        for raw in [
            "http://169.253.0.1",
            "http://169.255.0.1",
            "http://10.0.0.5:4318",
            "http://[fec0::1]",
            "http://[::1]:4318",
            "http://[::]:4318",
            "http://[::10.0.0.5]/",
            "http://[64:ff9b::10.0.0.5]/",
            "http://[64:ff9b:1::10.0.0.5]/",
            "http://[::ffff:0:10.0.0.5]/",
            "http://localhost:4318",
        ] {
            assert!(parse(raw, EndpointPath::AppendLogs).is_some(), "{raw}");
        }
    }

    #[test]
    fn exactly_the_two_cloud_metadata_addresses_outside_link_local_are_refused() {
        for raw in [
            "http://100.100.100.200/",
            "http://[fd00:ec2::254]/",
            "http://[::ffff:100.100.100.200]/",
            "http://[64:ff9b::100.100.100.200]/",
            // Local-use NAT64 under each RFC 6052 layout; every octet of the target must decode right, so
            // a misplaced octet index reads a different address and reds.
            "http://[64:ff9b:1::100.100.100.200]/",
            "http://[64:ff9b:1:6464:64:c800::]/",
            "http://[64:ff9b:1:64:64:64c8::]/",
            "http://[64:ff9b:1:0:64:6464:c800:0]/",
        ] {
            assert!(parse(raw, EndpointPath::AppendLogs).is_none(), "{raw}");
        }
        // Unique-local and CGNAT ranges are legitimate collector homes.
        for raw in [
            "http://100.100.100.201/",
            "http://100.64.0.1/",
            "http://[fd00:ec2::255]/",
            "http://[fd00:ec2::1]/",
            "http://[fd00::254]/",
        ] {
            assert!(parse(raw, EndpointPath::AppendLogs).is_some(), "{raw}");
        }
    }
}
