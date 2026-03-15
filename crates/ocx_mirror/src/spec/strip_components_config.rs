// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashMap;

use serde::Deserialize;

/// Configuration for stripping leading path components from archives before rebundling.
///
/// Supports two YAML forms:
///
/// Simple (all platforms share the same value):
/// ```yaml
/// strip_components: 1
/// ```
///
/// Per-platform (with optional default):
/// ```yaml
/// strip_components:
///   default: 1
///   platforms:
///     windows/amd64: 0
/// ```
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum StripComponentsConfig {
    Uniform(u8),
    PerPlatform {
        #[serde(default)]
        default: Option<u8>,
        #[serde(default)]
        platforms: HashMap<String, u8>,
    },
}

impl StripComponentsConfig {
    /// Resolve the strip_components value for a specific platform.
    ///
    /// For `Uniform`, returns the same value for all platforms.
    /// For `PerPlatform`, checks the platform map first, then falls back to the default.
    pub fn resolve(&self, platform: &str) -> Option<u8> {
        match self {
            Self::Uniform(n) => Some(*n),
            Self::PerPlatform { default, platforms } => platforms.get(platform).copied().or(*default),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniform_resolves_same_for_all_platforms() {
        let config = StripComponentsConfig::Uniform(1);
        assert_eq!(config.resolve("linux/amd64"), Some(1));
        assert_eq!(config.resolve("windows/amd64"), Some(1));
    }

    #[test]
    fn per_platform_resolves_specific_platform() {
        let config = StripComponentsConfig::PerPlatform {
            default: Some(1),
            platforms: HashMap::from([("windows/amd64".to_string(), 0)]),
        };
        assert_eq!(config.resolve("linux/amd64"), Some(1));
        assert_eq!(config.resolve("windows/amd64"), Some(0));
    }

    #[test]
    fn per_platform_without_default_returns_none_for_unmatched() {
        let config = StripComponentsConfig::PerPlatform {
            default: None,
            platforms: HashMap::from([("windows/amd64".to_string(), 0)]),
        };
        assert_eq!(config.resolve("linux/amd64"), None);
        assert_eq!(config.resolve("windows/amd64"), Some(0));
    }

    #[test]
    fn deserialize_uniform() {
        let yaml = "1";
        let config: StripComponentsConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(config.resolve("linux/amd64"), Some(1));
    }

    #[test]
    fn deserialize_per_platform() {
        let yaml = r#"
default: 1
platforms:
  windows/amd64: 0
"#;
        let config: StripComponentsConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(config.resolve("linux/amd64"), Some(1));
        assert_eq!(config.resolve("windows/amd64"), Some(0));
    }
}
