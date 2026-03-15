// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::Deserialize;

/// Describes how downloaded assets should be processed before bundling.
///
/// Archives are extracted (with optional component stripping). Raw binaries are
/// placed directly into the content directory under the configured name.
///
/// Supports three YAML forms:
///
/// Archive with uniform strip (shorthand):
/// ```yaml
/// asset_type:
///   archive:
///     strip_components: 1
/// ```
///
/// Archive with per-platform strip:
/// ```yaml
/// asset_type:
///   archive:
///     strip_components:
///       default: 1
///       platforms:
///         windows/amd64: 0
/// ```
///
/// Raw binary:
/// ```yaml
/// asset_type:
///   binary:
///     name: shfmt
/// ```
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssetTypeConfig {
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

impl AssetTypeConfig {
    /// Resolve to a concrete [`AssetType`] for a specific platform.
    pub fn resolve(&self, platform: &str) -> AssetType {
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
    fn deserialize_archive_per_platform() {
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
}
