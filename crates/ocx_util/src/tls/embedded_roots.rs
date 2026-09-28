// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Shared TLS root-seeding for hand-rolled `reqwest::Client` builders.

/// Seeds a [`reqwest::ClientBuilder`] with the bundled Mozilla CA roots.
///
/// Without seeded roots reqwest reads the OS trust store, which panics on a host with an empty one.
pub fn seed_embedded_roots(builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    let mut builder = builder;
    for root in webpki_root_certs::TLS_SERVER_ROOT_CERTS {
        if let Ok(certificate) = reqwest::Certificate::from_der(root.as_ref()) {
            builder = builder.add_root_certificate(certificate);
        }
    }
    builder
}
