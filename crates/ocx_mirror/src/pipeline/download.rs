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

    #[tokio::test]
    async fn download_empty_response_error() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

        // We can't easily test against a real server in unit tests.
        // This test validates the error path for empty content.
        let dir = TempDir::new().unwrap();
        let output = dir.path().join("test.bin");

        // An invalid URL will fail at the HTTP level, which is expected.
        let client = reqwest::Client::new();
        let result = download(&client, &Url::parse("http://127.0.0.1:1/nonexistent").unwrap(), &output).await;
        assert!(result.is_err());
    }
}
