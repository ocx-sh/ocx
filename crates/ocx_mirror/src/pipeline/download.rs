// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::Path;

use anyhow::{Result, bail};
use tokio::io::AsyncWriteExt;
use url::Url;

/// Download a file from a URL, streaming to disk.
pub async fn download(client: &reqwest::Client, url: &Url, output: &Path) -> Result<()> {
    let response = client.get(url.as_str()).send().await?.error_for_status()?;

    let mut file = tokio::fs::File::create(output).await?;
    let bytes = response.bytes().await?;

    if bytes.is_empty() {
        bail!("downloaded file is empty: {url}");
    }

    file.write_all(&bytes).await?;
    file.flush().await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Install the rustls crypto provider exactly once per process. Reqwest
    /// builds its TLS stack lazily on first `Client::new` and panics with
    /// "No provider set" if none is registered, even for `http://` URLs.
    fn install_crypto_provider() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        });
    }

    /// Reserve an ephemeral port, then drop the listener so subsequent connects
    /// fail fast with `ECONNREFUSED`. Avoids hardcoded ports like `127.0.0.1:1`
    /// that silently drop SYNs on some hosts (WSL2, hardened kernels), which
    /// pushes the test from ~0ms to the OS TCP retry budget (~130s).
    fn reserved_unused_port() -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        port
    }

    #[tokio::test]
    async fn download_connect_refused() {
        install_crypto_provider();

        let dir = TempDir::new().unwrap();
        let output = dir.path().join("test.bin");
        let url = Url::parse(&format!("http://127.0.0.1:{}/nonexistent", reserved_unused_port())).unwrap();

        let client = reqwest::Client::new();
        let result = download(&client, &url, &output).await;
        assert!(result.is_err(), "expected connect failure to surface as Err");
    }

    #[tokio::test]
    async fn download_empty_body_is_error() {
        install_crypto_provider();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            // Drain request headers; payload not inspected.
            let mut scratch = [0u8; 1024];
            let _ = socket.read(&mut scratch).await;
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            socket.shutdown().await.unwrap();
        });

        let dir = TempDir::new().unwrap();
        let output = dir.path().join("test.bin");
        let url = Url::parse(&format!("http://{addr}/blob")).unwrap();

        let client = reqwest::Client::new();
        let result = download(&client, &url, &output).await;
        server.await.unwrap();

        let err = result.expect_err("empty body must error");
        assert!(
            err.to_string().contains("downloaded file is empty"),
            "unexpected error: {err}"
        );
    }
}
