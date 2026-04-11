// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashMap;

use serde::Deserialize;

/// Describes how downloaded assets should be processed before bundling.
///
/// Archives are extracted (with optional component stripping). Raw binaries are
/// placed directly into the content directory under the configured name.
///
/// Supports three YAML forms:
///
/// Uniform archive (all platforms):
/// ```yaml
/// asset_type:
///   type: archive
///   strip_components: 1
/// ```
///
/// Uniform archive with per-platform strip:
/// ```yaml
/// asset_type:
///   type: archive
///   strip_components:
///     default: 1
///     platforms:
///       windows/amd64: 0
/// ```
///
/// Uniform binary (all platforms):
/// ```yaml
/// asset_type:
///   type: binary
///   name: shfmt
/// ```
///
/// Per-platform mix — e.g. archive on Linux/macOS, raw binary on Windows:
/// ```yaml
/// asset_type:
///   default:
///     type: archive
///     strip_components: 0
///   platforms:
///     windows/amd64:
///       type: binary
///       name: lychee
/// ```
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum AssetTypeConfig {
    /// One asset type for all platforms.
    Uniform(UniformAssetType),
    /// Per-platform override map with a default fallback.
    PerPlatform {
        default: UniformAssetType,
        #[serde(default)]
        platforms: HashMap<String, UniformAssetType>,
    },
}

/// A single asset type definition, used either on its own or as the default /
/// per-platform value inside [`AssetTypeConfig::PerPlatform`].
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UniformAssetType {
    /// The asset is an archive (tar, zip, etc.) to be extracted.
    Archive {
        /// Leading path components to strip from the archive before rebundling.
        #[serde(default)]
        strip_components: Option<super::StripComponentsConfig>,
    },
    /// The asset is a standalone executable binary.
    Binary {
        /// The filename for the binary in the package (e.g. `shfmt`).
        /// On Windows, `.exe` is appended automatically if the downloaded asset has it.
        name: String,
    },
}

impl UniformAssetType {
    fn resolve(&self, platform: &str) -> AssetType {
        match self {
            Self::Archive { strip_components } => {
                let strip = strip_components.as_ref().and_then(|sc| sc.resolve(platform));
                AssetType::Archive {
                    strip_components: strip,
                }
            }
            Self::Binary { name } => AssetType::Binary { name: name.clone() },
        }
    }
}

impl AssetTypeConfig {
    /// Resolve to a concrete [`AssetType`] for a specific platform.
    pub fn resolve(&self, platform: &str) -> AssetType {
        match self {
            Self::Uniform(u) => u.resolve(platform),
            Self::PerPlatform { default, platforms } => platforms.get(platform).unwrap_or(default).resolve(platform),
        }
    }
}

/// Resolved asset type for a specific platform, ready for the pipeline.
#[derive(Debug, Clone)]
pub enum AssetType {
    Archive { strip_components: Option<u8> },
    Binary { name: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_archive_uniform() {
        let yaml = r#"
type: archive
strip_components: 1
"#;
        let config: AssetTypeConfig = serde_yaml_ng::from_str(yaml).unwrap();
        match config.resolve("linux/amd64") {
            AssetType::Archive { strip_components } => assert_eq!(strip_components, Some(1)),
            _ => panic!("expected Archive"),
        }
    }

    #[test]
    fn deserialize_archive_per_platform_strip() {
        let yaml = r#"
type: archive
strip_components:
  default: 1
  platforms:
    windows/amd64: 0
"#;
        let config: AssetTypeConfig = serde_yaml_ng::from_str(yaml).unwrap();
        match config.resolve("linux/amd64") {
            AssetType::Archive { strip_components } => assert_eq!(strip_components, Some(1)),
            _ => panic!("expected Archive"),
        }
        match config.resolve("windows/amd64") {
            AssetType::Archive { strip_components } => assert_eq!(strip_components, Some(0)),
            _ => panic!("expected Archive"),
        }
    }

    #[test]
    fn deserialize_archive_no_strip() {
        let yaml = r#"
type: archive
"#;
        let config: AssetTypeConfig = serde_yaml_ng::from_str(yaml).unwrap();
        match config.resolve("linux/amd64") {
            AssetType::Archive { strip_components } => assert_eq!(strip_components, None),
            _ => panic!("expected Archive"),
        }
    }

    #[test]
    fn deserialize_binary() {
        let yaml = r#"
type: binary
name: shfmt
"#;
        let config: AssetTypeConfig = serde_yaml_ng::from_str(yaml).unwrap();
        match config.resolve("linux/amd64") {
            AssetType::Binary { name } => assert_eq!(name, "shfmt"),
            _ => panic!("expected Binary"),
        }
    }

    #[test]
    fn deserialize_per_platform_mix_archive_and_binary() {
        let yaml = r#"
default:
  type: archive
  strip_components: 0
platforms:
  windows/amd64:
    type: binary
    name: lychee
"#;
        let config: AssetTypeConfig = serde_yaml_ng::from_str(yaml).unwrap();
        match config.resolve("linux/amd64") {
            AssetType::Archive { strip_components } => assert_eq!(strip_components, Some(0)),
            _ => panic!("expected Archive for linux/amd64"),
        }
        match config.resolve("darwin/arm64") {
            AssetType::Archive { strip_components } => assert_eq!(strip_components, Some(0)),
            _ => panic!("expected Archive for darwin/arm64"),
        }
        match config.resolve("windows/amd64") {
            AssetType::Binary { name } => assert_eq!(name, "lychee"),
            _ => panic!("expected Binary for windows/amd64"),
        }
    }

    #[test]
    fn per_platform_falls_back_to_default_for_unmatched_platform() {
        let yaml = r#"
default:
  type: binary
  name: tool
platforms:
  linux/amd64:
    type: archive
    strip_components: 1
"#;
        let config: AssetTypeConfig = serde_yaml_ng::from_str(yaml).unwrap();
        match config.resolve("linux/amd64") {
            AssetType::Archive { strip_components } => assert_eq!(strip_components, Some(1)),
            _ => panic!("expected Archive for linux/amd64"),
        }
        match config.resolve("darwin/arm64") {
            AssetType::Binary { name } => assert_eq!(name, "tool"),
            _ => panic!("expected Binary fallback for darwin/arm64"),
        }
    }

    #[test]
    fn per_platform_without_explicit_platforms_key_uses_default() {
        let yaml = r#"
default:
  type: archive
  strip_components: 2
"#;
        let config: AssetTypeConfig = serde_yaml_ng::from_str(yaml).unwrap();
        match config.resolve("linux/amd64") {
            AssetType::Archive { strip_components } => assert_eq!(strip_components, Some(2)),
            _ => panic!("expected Archive"),
        }
    }
}
