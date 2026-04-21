// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! SSRF-hardened URL validation for Sigstore endpoints.
//!
//! User-supplied `--fulcio-url` / `--rekor-url` flags become HTTP client
//! targets; unrestricted input would enable SSRF (CWE-918). Slice 1 policy
//! is HTTPS-only in production, with an explicit loopback carve-out so
//! integration tests can point at the local `fake_sigstore` stack
//! (`http://127.0.0.1:PORT/...`).

use url::{Host, Url};

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
/// Returns an error describing the violation, citing `flag_name` so callers
/// can surface a useful message (e.g. `"invalid --fulcio-url URL ..."`).
pub fn validate_sigstore_url(raw: &str, flag_name: &str) -> anyhow::Result<Url> {
    let url = Url::parse(raw).map_err(|e| anyhow::anyhow!("invalid {flag_name} URL `{raw}`: {e}"))?;
    if !url.username().is_empty() || url.password().is_some() {
        // Strip the userinfo component before echoing the URL back, so that
        // any password supplied on the command line does not leak into logs
        // or the structured JSON error envelope (CWE-209).
        let mut sanitized = url.clone();
        let _ = sanitized.set_username("");
        let _ = sanitized.set_password(None);
        return Err(anyhow::anyhow!(
            "{flag_name} must not embed credentials (sanitized: `{sanitized}`)"
        ));
    }
    let scheme = url.scheme();
    let is_loopback = match url.host() {
        Some(Host::Domain(h)) => h == "localhost",
        Some(Host::Ipv4(addr)) => addr.is_loopback(),
        Some(Host::Ipv6(addr)) => addr.is_loopback(),
        None => false,
    };
    match (scheme, is_loopback) {
        ("https", _) => Ok(url),
        ("http", true) => Ok(url),
        ("http", false) => Err(anyhow::anyhow!(
            "{flag_name} must use HTTPS (got `{raw}`); HTTP only accepted for loopback hosts"
        )),
        (other, _) => Err(anyhow::anyhow!(
            "{flag_name} must use HTTPS or HTTP on loopback (got scheme `{other}`)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let err = validate_sigstore_url("https://user:pass@fulcio.sigstore.dev", "--fulcio-url")
            .expect_err("URLs embedding credentials must be rejected");
        let msg = err.to_string();
        assert!(msg.contains("--fulcio-url"), "error must cite flag name");
        assert!(msg.contains("credentials"), "error must explain rejection reason");
    }

    #[test]
    fn url_with_username_only_rejected() {
        let err = validate_sigstore_url("https://user@fulcio.sigstore.dev", "--fulcio-url")
            .expect_err("URLs with username alone must be rejected");
        assert!(err.to_string().contains("credentials"));
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
        let err = validate_sigstore_url("http://[::ffff:127.0.0.1]:8080", "--fulcio-url")
            .expect_err("IPv4-mapped IPv6 is not std::net loopback; must be rejected");
        assert!(err.to_string().contains("HTTPS"));
    }

    #[test]
    fn http_non_loopback_rejected() {
        let err = validate_sigstore_url("http://example.com/fulcio", "--fulcio-url")
            .expect_err("non-loopback http must be rejected");
        let msg = err.to_string();
        assert!(msg.contains("--fulcio-url"), "error must cite flag name");
        assert!(msg.contains("HTTPS"), "error must mention HTTPS requirement");
    }

    #[test]
    fn file_scheme_rejected() {
        let err = validate_sigstore_url("file:///etc/passwd", "--rekor-url").expect_err("file:// must be rejected");
        let msg = err.to_string();
        assert!(msg.contains("--rekor-url"), "error must cite flag name");
        assert!(msg.contains("file"), "error must mention the bad scheme");
    }

    #[test]
    fn ftp_scheme_rejected() {
        let err =
            validate_sigstore_url("ftp://example.com/bundle", "--rekor-url").expect_err("ftp:// must be rejected");
        let msg = err.to_string();
        assert!(msg.contains("ftp"), "error must mention the bad scheme");
    }

    #[test]
    fn malformed_url_rejected() {
        let err =
            validate_sigstore_url("not a url at all", "--fulcio-url").expect_err("malformed url must be rejected");
        let msg = err.to_string();
        assert!(msg.contains("--fulcio-url"), "error must cite flag name");
    }

    #[test]
    fn empty_url_rejected() {
        let err = validate_sigstore_url("", "--fulcio-url").expect_err("empty url must be rejected");
        let msg = err.to_string();
        assert!(msg.contains("--fulcio-url"), "error must cite flag name");
    }
}
