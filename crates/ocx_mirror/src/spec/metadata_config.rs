// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct MetadataConfig {
    pub default: PathBuf,
    #[serde(default)]
    pub platforms: HashMap<String, PathBuf>,
}

impl MetadataConfig {
    pub fn validate(&self, spec_dir: &Path, errors: &mut Vec<String>) {
        let default_path = spec_dir.join(&self.default);
        if !default_path.exists() {
            errors.push(format!("metadata.default: file not found: {}", self.default.display()));
        }

        for (platform, path) in &self.platforms {
            let full_path = spec_dir.join(path);
            if !full_path.exists() {
                errors.push(format!(
                    "metadata.platforms.{platform}: file not found: {}",
                    path.display()
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// YAML round-trip: the exact `metadata:` block from mirrors/cmake/mirror.yml
    /// must parse and produce a `MetadataConfig` where each platform key resolves
    /// to the correct override file.
    ///
    /// This test guards against the class of bug where YAML keys containing `/`
    /// (the OCI platform separator, e.g. `darwin/arm64`) are silently dropped or
    /// mangled during deserialization, causing all platforms to fall back to the
    /// default metadata and thus embedding `${installPath}/bin` instead of the
    /// platform-specific `${installPath}/CMake.app/Contents/bin` into darwin
    /// bundles.
    #[test]
    fn metadata_config_yaml_round_trip_with_slash_keys() {
        let yaml = r#"
default: metadata.json
platforms:
  windows/amd64: metadata-windows.json
  windows/arm64: metadata-windows.json
  darwin/amd64: metadata-darwin.json
  darwin/arm64: metadata-darwin.json
"#;
        let config: MetadataConfig = serde_yaml_ng::from_str(yaml)
            .expect("MetadataConfig must deserialize from YAML with slash-containing platform keys");

        assert_eq!(config.default, PathBuf::from("metadata.json"));

        // Each platform must be present and map to the correct file.
        for (platform, expected_file) in &[
            ("windows/amd64", "metadata-windows.json"),
            ("windows/arm64", "metadata-windows.json"),
            ("darwin/amd64", "metadata-darwin.json"),
            ("darwin/arm64", "metadata-darwin.json"),
        ] {
            let actual = config.platforms.get(*platform).unwrap_or_else(|| {
                panic!(
                    "platforms map must contain '{platform}' key; got keys: {:?}",
                    config.platforms.keys().collect::<Vec<_>>()
                )
            });
            assert_eq!(
                actual,
                &PathBuf::from(*expected_file),
                "platform '{platform}' must map to '{expected_file}'"
            );
        }

        assert_eq!(
            config.platforms.len(),
            4,
            "must have exactly 4 platform overrides; got: {:?}",
            config.platforms.keys().collect::<Vec<_>>()
        );
    }

    /// `resolve_metadata` selects `metadata-darwin.json` for `darwin/arm64`
    /// when the platforms map contains that key.
    ///
    /// Pre-fix: if YAML deserialization dropped slash-containing keys, the
    /// map would be empty and this call would always return the default
    /// metadata — the exact bug that caused darwin bundles to embed
    /// `${installPath}/bin` instead of `${installPath}/CMake.app/Contents/bin`.
    #[test]
    fn resolve_metadata_selects_darwin_override_from_yaml_config() {
        use crate::pipeline::package;

        let dir = tempfile::TempDir::new().unwrap();
        let default_content = r#"{"type":"bundle","version":1,"env":[{"key":"PATH","type":"path","required":true,"value":"${installPath}/bin","visibility":"public"}]}"#;
        let darwin_content = r#"{"type":"bundle","version":1,"env":[{"key":"PATH","type":"path","required":true,"value":"${installPath}/CMake.app/Contents/bin","visibility":"public"}]}"#;
        std::fs::write(dir.path().join("metadata.json"), default_content).unwrap();
        std::fs::write(dir.path().join("metadata-darwin.json"), darwin_content).unwrap();

        let yaml = r#"
default: metadata.json
platforms:
  darwin/amd64: metadata-darwin.json
  darwin/arm64: metadata-darwin.json
"#;
        let config: MetadataConfig = serde_yaml_ng::from_str(yaml).unwrap();

        for platform in &["darwin/amd64", "darwin/arm64"] {
            let metadata = package::resolve_metadata(&config, platform, dir.path())
                .unwrap_or_else(|e| panic!("resolve_metadata failed for {platform}: {e}"));

            // The resolved metadata must declare the darwin-specific PATH value,
            // not the default `${installPath}/bin`.
            let path_entry = metadata
                .env()
                .and_then(|vars| vars.into_iter().find(|v| v.key == "PATH"))
                .unwrap_or_else(|| panic!("PATH entry missing in metadata for {platform}"));

            let path_value = path_entry
                .value()
                .unwrap_or_else(|| panic!("PATH entry has no value for {platform}"));

            assert_eq!(
                path_value, "${installPath}/CMake.app/Contents/bin",
                "darwin platform '{platform}' must use metadata-darwin.json (CMake.app path), \
                 not the default bin/ path; got: {path_value:?}\n\
                 pre-fix regression: YAML slash-key deserialization must populate platforms map"
            );
        }
    }
}
