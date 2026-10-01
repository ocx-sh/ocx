// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Loopback servers that fail a request in one specific way.

use std::net::SocketAddr;

/// Binds a loopback listener that accepts every connection and drops it unanswered.
///
/// A request to it fails after a successful connect, so it is neither a connect nor a timeout error.
pub async fn serve_hangup() -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind a hang-up listener");
    let addr = listener.local_addr().expect("local address");
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            drop(stream);
        }
    });
    addr
}
