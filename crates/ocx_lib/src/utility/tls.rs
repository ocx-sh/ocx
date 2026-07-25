// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Shared TLS root-seeding for hand-rolled `reqwest::Client` builders.

/// Seeds a [`reqwest::ClientBuilder`] with the bundled Mozilla CA roots.
///
/// Without an explicit root set, reqwest's rustls path falls back to the OS
/// trust store, which **panics** on a host with an empty store (minimal
/// container / CI runner). Seeding any roots flips reqwest onto the
/// extra-roots verifier branch that never touches the system store. Shared by
/// every hand-rolled `reqwest::Client` in this crate that needs bundled roots
/// (`forge::github`, `oci::index::ocx_index`) — the `oci_client` transport
/// gets the same set independently via the fork's `ClientConfig::default()`.
///
/// A root whose DER bytes fail to parse is skipped rather than failing the
/// whole build — `webpki_root_certs::TLS_SERVER_ROOT_CERTS` is a fixed,
/// vendored set, so a per-entry parse failure here would indicate upstream
/// bundle corruption, not caller input.
pub fn seed_embedded_roots(builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    let mut builder = builder;
    for root in webpki_root_certs::TLS_SERVER_ROOT_CERTS {
        if let Ok(certificate) = reqwest::Certificate::from_der(root.as_ref()) {
            builder = builder.add_root_certificate(certificate);
        }
    }
    builder
}
