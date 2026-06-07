// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! SSRF-hardened URL validation for Sigstore endpoints.
//!
//! User-supplied `--fulcio-url` / `--rekor-url` flags become HTTP client
//! targets; unrestricted input would enable SSRF (CWE-918). Slice 1 policy
//! is HTTPS-only in production, with an explicit loopback carve-out so
//! integration tests can point at the local `fake_sigstore` stack
//! (`http://127.0.0.1:PORT/...`).
//!
//! Lives in `ocx_lib` rather than the CLI so any future library consumer
//! (mirror tool, SDK, Bazel rule) routes through the same guard before
//! constructing a [`super::pipeline::SignContext`] or a verify context.
//! The function returns a [`UrlRejection`] on failure; each caller wraps
//! it into their own `InvalidEndpointUrl` variant so exit-code
//! classification stays local to the sign or verify subsystem.

use url::{Host, Url};

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
    let is_loopback = match url.host() {
        Some(Host::Domain(h)) => h == "localhost",
        Some(Host::Ipv4(addr)) => addr.is_loopback(),
        Some(Host::Ipv6(addr)) => addr.is_loopback(),
        None => false,
    };
    match (scheme, is_loopback) {
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
}
