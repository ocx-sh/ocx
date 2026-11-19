// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Test-only builders the export and client tests share.

use std::io;
use std::net::SocketAddr;
use std::sync::mpsc;
use std::time::Duration;

use secrecy::SecretString;
use url::Url;

use crate::destination::{Destination, DestinationFamily};

/// The value of the `authorization` header [`destination`] carries, for leak checks.
pub(crate) const SECRET_HEADER: &str = "hunter2-bearer-token";

/// A `[telemetry]` destination with two headers, one of them a credential.
pub(crate) fn destination(endpoint: &str, timeout: Duration) -> Destination {
    Destination {
        endpoint: Url::parse(endpoint).expect("literal URL"),
        headers: vec![
            ("authorization".to_owned(), SecretString::from(SECRET_HEADER)),
            ("x-team".to_owned(), SecretString::from("blue")),
        ],
        timeout,
        family: DestinationFamily::OcxConfig,
    }
}

/// A name lookup that never answers until the returned sender is dropped, the way a stuck
/// `getaddrinfo` does. Keep the sender alive for the length of the case.
pub(crate) fn stalled_lookup() -> (
    mpsc::Sender<()>,
    impl FnOnce(String, u16) -> io::Result<Vec<SocketAddr>> + Send + 'static,
) {
    let (release, hold) = mpsc::channel::<()>();
    (release, move |_, _| {
        let _ = hold.recv();
        Err(io::Error::other("released"))
    })
}
