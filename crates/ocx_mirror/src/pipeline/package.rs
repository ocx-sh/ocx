// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::Path;

use anyhow::Result;
use ocx_lib::archive::{Archive, ExtractOptions};
use ocx_lib::package::bundle::BundleBuilder;
use ocx_lib::package::metadata::Metadata;

use crate::spec::{AssetType, MetadataConfig};

/// Process a downloaded asset and create an OCX bundle.
///
/// The [`AssetType`] determines how the asset is handled:
/// - `Archive`: extracted as a tar/zip, with optional `strip_components`.
/// - `Binary`: placed directly into the content directory under the configured name.
///
/// When `compression_threads` is `Some(n)` with n > 1, multi-threaded LZMA compression is used.
pub async fn extract_and_bundle(
    asset_path: &Path,
    content_dir: &Path,
    bundle_path: &Path,
    metadata: &Metadata,
    asset_type: &AssetType,
    asset_name: &str,
    compression_threads: Option<u32>,
) -> Result<()> {
    match asset_type {
        AssetType::Archive { strip_components } => {
            let options = strip_components.map(|sc| ExtractOptions {
                strip_components: sc as usize,
                ..Default::default()
            });
            Archive::extract_with_options(asset_path, content_dir, options).await?;
        }
        AssetType::Binary { name } => {
            place_binary(asset_path, content_dir, name, asset_name).await?;
        }
    }

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

/// Place a binary into the content directory and make it executable.
///
/// The filename is the configured `name` from the spec. If the downloaded asset
/// has a `.exe` extension, it is preserved on the output filename.
async fn place_binary(asset_path: &Path, content_dir: &Path, name: &str, asset_name: &str) -> Result<()> {
    let filename = if asset_name.ends_with(".exe") && !name.ends_with(".exe") {
        format!("{name}.exe")
    } else {
        name.to_string()
    };

    let dest = content_dir.join(&filename);
    tokio::fs::copy(asset_path, &dest).await?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        tokio::fs::set_permissions(&dest, perms).await?;
    }

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

    #[tokio::test]
    async fn place_binary_uses_configured_name() {
        let dir = tempfile::TempDir::new().unwrap();
        let asset = dir.path().join("shfmt_v3.13.0_linux_amd64");
        std::fs::write(&asset, b"fake binary").unwrap();

        let content_dir = dir.path().join("content");
        std::fs::create_dir(&content_dir).unwrap();

        place_binary(&asset, &content_dir, "shfmt", "shfmt_v3.13.0_linux_amd64")
            .await
            .unwrap();

        let dest = content_dir.join("shfmt");
        assert!(dest.exists());
        assert_eq!(std::fs::read(&dest).unwrap(), b"fake binary");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dest).unwrap().permissions().mode();
            assert_eq!(mode & 0o755, 0o755);
        }
    }

    #[tokio::test]
    async fn place_binary_appends_exe_for_windows_assets() {
        let dir = tempfile::TempDir::new().unwrap();
        let asset = dir.path().join("shfmt_v3.13.0_windows_amd64.exe");
        std::fs::write(&asset, b"fake exe").unwrap();

        let content_dir = dir.path().join("content");
        std::fs::create_dir(&content_dir).unwrap();

        place_binary(&asset, &content_dir, "shfmt", "shfmt_v3.13.0_windows_amd64.exe")
            .await
            .unwrap();

        assert!(content_dir.join("shfmt.exe").exists());
    }

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
