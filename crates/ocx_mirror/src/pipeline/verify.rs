// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use ocx_lib::oci::Digest as OciDigest;
use sha2::{Digest, Sha256};

use crate::spec::VerifyConfig;

/// Verify a downloaded file's digest against an expected value.
///
/// The `expected` string is parsed as an OCI digest (`sha256:…`,
/// `sha384:…`, or `sha512:…`) and the file is hashed with the
/// matching algorithm. Returns an error for an unparseable digest
/// string or a content mismatch.
pub async fn verify_digest(file: &Path, expected: &str) -> Result<()> {
    let expected_digest =
        OciDigest::try_from(expected).with_context(|| format!("invalid digest string '{expected}'"))?;
    let actual = expected_digest
        .algorithm()
        .hash_file(file)
        .await
        .with_context(|| format!("failed to hash {}", file.display()))?;

    if actual != expected_digest {
        bail!(
            "digest mismatch for {}: expected {expected_digest}, got {actual}",
            file.display()
        );
    }
    Ok(())
}

/// Verify a downloaded file against a sidecar checksums file.
/// Parses `sha256sum` format: `HASH  FILENAME` or `HASH FILENAME`.
pub async fn verify_checksums_file(
    client: &reqwest::Client,
    file: &Path,
    asset_name: &str,
    checksums_url: &str,
) -> Result<()> {
    let response = client.get(checksums_url).send().await?.error_for_status()?;
    let body = response.text().await?;

    let expected_hash = parse_checksums(&body, asset_name)?;

    let data = tokio::fs::read(file).await?;
    let hash = Sha256::digest(&data);
    let actual = hex::encode(hash);

    if actual != expected_hash {
        bail!("checksums file mismatch for {asset_name}: expected {expected_hash}, got {actual}");
    }
    Ok(())
}

/// Parse a sha256sum-format checksums file and find the hash for a given filename.
fn parse_checksums(content: &str, asset_name: &str) -> Result<String> {
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Format: "HASH  FILENAME" or "HASH FILENAME"
        if let Some((hash, filename)) = line.split_once("  ").or_else(|| line.split_once(' ')) {
            let filename = filename.trim();
            // Match against the asset name (basename only)
            if filename == asset_name || filename.ends_with(&format!("/{asset_name}")) {
                return Ok(hash.to_string());
            }
        }
    }
    bail!("asset '{asset_name}' not found in checksums file");
}

/// Run all configured verification steps on a downloaded file.
pub async fn verify(
    config: &VerifyConfig,
    client: &reqwest::Client,
    file: &Path,
    asset_name: &str,
    asset_digests: &HashMap<String, String>,
    _download_url: &url::Url,
) -> Result<()> {
    // 1. Verify against GitHub asset digest if configured and available
    if config.github_asset_digest
        && let Some(expected) = asset_digests.get(asset_name)
    {
        verify_digest(file, expected).await?;
    }

    // 2. Verify against sidecar checksums file if configured
    if let Some(checksums_pattern) = &config.checksums_file {
        // For now, treat the pattern as a direct URL suffix on the same base
        // A more sophisticated implementation would support {asset}/{version} templates
        verify_checksums_file(client, file, asset_name, checksums_pattern).await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn verify_correct_digest() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.bin");
        let content = b"hello world";
        tokio::fs::write(&file, content).await.unwrap();

        let hash = Sha256::digest(content);
        let expected = format!("sha256:{}", hex::encode(hash));

        verify_digest(&file, &expected).await.unwrap();
    }

    #[tokio::test]
    async fn verify_incorrect_digest() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.bin");
        tokio::fs::write(&file, b"hello world").await.unwrap();

        let result = verify_digest(
            &file,
            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        )
        .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("digest mismatch"));
    }

    #[tokio::test]
    async fn verify_correct_sha512_digest() {
        // Regression: before the algorithm-aware fix, the verify path
        // hashed every file with SHA-256 and compared against the raw
        // `sha512:…` string, producing a spurious mismatch.
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.bin");
        let content = b"hello world";
        tokio::fs::write(&file, content).await.unwrap();

        let hash = sha2::Sha512::digest(content);
        let expected = format!("sha512:{}", hex::encode(hash));

        verify_digest(&file, &expected).await.unwrap();
    }

    #[tokio::test]
    async fn verify_rejects_unparseable_digest() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.bin");
        tokio::fs::write(&file, b"hello world").await.unwrap();

        let result = verify_digest(&file, "not-a-digest").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("invalid digest string"));
    }

    #[test]
    fn parse_standard_checksums_format() {
        let content = "abc123  tool-1.0.0-linux-amd64.tar.gz\ndef456  tool-1.0.0-darwin-arm64.tar.gz\n";
        let hash = parse_checksums(content, "tool-1.0.0-linux-amd64.tar.gz").unwrap();
        assert_eq!(hash, "abc123");
    }

    #[test]
    fn parse_checksums_single_space() {
        let content = "abc123 tool-1.0.0-linux-amd64.tar.gz\n";
        let hash = parse_checksums(content, "tool-1.0.0-linux-amd64.tar.gz").unwrap();
        assert_eq!(hash, "abc123");
    }

    #[test]
    fn parse_checksums_asset_not_found() {
        let content = "abc123  other-tool.tar.gz\n";
        let result = parse_checksums(content, "tool-1.0.0.tar.gz");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn parse_checksums_skips_comments_and_blanks() {
        let content = "# SHA256 checksums\n\nabc123  tool.tar.gz\n";
        let hash = parse_checksums(content, "tool.tar.gz").unwrap();
        assert_eq!(hash, "abc123");
    }
}
