// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::Path;

use anyhow::Result;
use ocx_lib::archive::{Archive, ExtractOptions};
use ocx_lib::package::bundle::BundleBuilder;
use ocx_lib::package::metadata::Metadata;

use crate::spec::MetadataConfig;

/// Extract a downloaded archive and create an OCX bundle.
///
/// When `compression_threads` is `Some(n)` with n > 1, multi-threaded LZMA compression is used.
pub async fn extract_and_bundle(
    archive_path: &Path,
    content_dir: &Path,
    bundle_path: &Path,
    metadata: &Metadata,
    strip_components: Option<u8>,
    compression_threads: Option<u32>,
) -> Result<()> {
    // Extract archive
    let options = strip_components.map(|sc| ExtractOptions {
        strip_components: sc as usize,
        ..Default::default()
    });
    Archive::extract_with_options(archive_path, content_dir, options).await?;

    // Write metadata.json into content dir
    let metadata_path = content_dir.join("metadata.json");
    let metadata_json = serde_json::to_string_pretty(metadata)?;
    tokio::fs::write(&metadata_path, metadata_json).await?;

    // Create bundle with optional multi-threaded compression
    let mut builder = BundleBuilder::from_path(content_dir);
    if let Some(threads) = compression_threads {
        use ocx_lib::compression::CompressionOptions;
        builder = builder.with_compression(CompressionOptions::default().with_threads(threads));
    }
    builder.create(bundle_path).await?;

    Ok(())
}

/// Resolve the metadata JSON file for a given platform, falling back to the default.
pub fn resolve_metadata(config: &MetadataConfig, platform: &str, spec_dir: &Path) -> Result<Metadata> {
    let metadata_path = if let Some(platform_path) = config.platforms.get(platform) {
        spec_dir.join(platform_path)
    } else {
        spec_dir.join(&config.default)
    };

    let content = std::fs::read_to_string(&metadata_path)
        .map_err(|e| anyhow::anyhow!("failed to read metadata file {}: {e}", metadata_path.display()))?;

    let metadata: Metadata = serde_json::from_str(&content)
        .map_err(|e| anyhow::anyhow!("failed to parse metadata file {}: {e}", metadata_path.display()))?;

    Ok(metadata)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn resolve_metadata_default() {
        let dir = tempfile::TempDir::new().unwrap();
        let metadata_content = r#"{"type":"bundle","version":1,"strip_components":1,"env":[]}"#;
        std::fs::write(dir.path().join("default.json"), metadata_content).unwrap();

        let config = MetadataConfig {
            default: "default.json".into(),
            platforms: HashMap::new(),
        };

        let _metadata = resolve_metadata(&config, "linux/amd64", dir.path()).unwrap();
    }

    #[test]
    fn resolve_metadata_platform_override() {
        let dir = tempfile::TempDir::new().unwrap();
        let default_content = r#"{"type":"bundle","version":1,"strip_components":1,"env":[]}"#;
        let darwin_content = r#"{"type":"bundle","version":1,"strip_components":2,"env":[]}"#;
        std::fs::write(dir.path().join("default.json"), default_content).unwrap();
        std::fs::write(dir.path().join("darwin.json"), darwin_content).unwrap();

        let config = MetadataConfig {
            default: "default.json".into(),
            platforms: HashMap::from([("darwin/arm64".to_string(), "darwin.json".into())]),
        };

        let _metadata = resolve_metadata(&config, "darwin/arm64", dir.path()).unwrap();
    }
}
