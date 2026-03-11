// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

mod assets;
mod concurrency_config;
mod metadata_config;
mod source;
mod target;
mod verify_config;
mod versions_config;

pub use assets::AssetPatterns;
pub use concurrency_config::{ConcurrencyConfig, resolve_compression_threads};
pub use metadata_config::MetadataConfig;
pub use source::{Source, UrlIndexVersion};
pub use target::Target;
pub use verify_config::VerifyConfig;
pub use versions_config::VersionsConfig;

use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct MirrorSpec {
    pub name: String,
    pub target: Target,
    pub source: Source,
    pub assets: AssetPatterns,

    #[serde(default)]
    pub metadata: Option<MetadataConfig>,

    #[serde(default = "default_build_timestamp")]
    pub build_timestamp: BuildTimestampFormat,

    #[serde(default = "default_true")]
    pub cascade: bool,

    #[serde(default)]
    pub versions: Option<VersionsConfig>,

    #[serde(default)]
    pub skip_prereleases: bool,

    #[serde(default)]
    pub verify: Option<VerifyConfig>,

    #[serde(default)]
    pub concurrency: ConcurrencyConfig,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BuildTimestampFormat {
    Datetime,
    Date,
    None,
}

fn default_build_timestamp() -> BuildTimestampFormat {
    BuildTimestampFormat::Datetime
}

fn default_true() -> bool {
    true
}

impl MirrorSpec {
    pub fn validate(&self, spec_path: &Path) -> Vec<String> {
        let mut errors = Vec::new();
        let spec_dir = spec_path.parent().unwrap_or(Path::new("."));

        self.source.validate(&mut errors);
        self.assets.validate(&mut errors);

        if let Some(metadata) = &self.metadata {
            metadata.validate(spec_dir, &mut errors);
        }

        if let Some(versions) = &self.versions {
            versions.validate(&mut errors);
        }

        errors
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_github_release_spec() {
        let yaml = r#"
name: cmake
target:
  registry: ocx.sh
  repository: cmake
source:
  type: github_release
  owner: Kitware
  repo: CMake
  tag_pattern: "^v(?P<version>\\d+\\.\\d+\\.\\d+)$"
assets:
  linux/amd64:
    - "cmake-.*-linux-x86_64\\.tar\\.gz"
  linux/arm64:
    - "cmake-.*-linux-aarch64\\.tar\\.gz"
  darwin/amd64:
    - "cmake-.*-macos-universal\\.tar\\.gz"
  darwin/arm64:
    - "cmake-.*-macos-universal\\.tar\\.gz"
metadata:
  default: metadata/cmake.json
"#;

        let spec: MirrorSpec = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(spec.name, "cmake");
        assert_eq!(spec.target.registry, "ocx.sh");
        assert_eq!(spec.target.repository, "cmake");
        assert!(matches!(spec.source, Source::GithubRelease { .. }));
        assert_eq!(spec.build_timestamp, BuildTimestampFormat::Datetime);
        assert!(spec.cascade);
        assert!(!spec.skip_prereleases);
    }

    #[test]
    fn parse_url_index_inline_spec() {
        let yaml = r#"
name: test-tool
target:
  registry: localhost:5000
  repository: test-tool
source:
  type: url_index
  versions:
    "1.0.0":
      assets:
        test-tool-1.0.0-linux-amd64.tar.gz: "https://example.com/test-tool-1.0.0-linux-amd64.tar.gz"
    "1.1.0":
      prerelease: true
      assets:
        test-tool-1.1.0-linux-amd64.tar.gz: "https://example.com/test-tool-1.1.0-linux-amd64.tar.gz"
assets:
  linux/amd64:
    - "test-tool-.*-linux-amd64\\.tar\\.gz"
build_timestamp: date
cascade: false
skip_prereleases: true
"#;

        let spec: MirrorSpec = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(spec.name, "test-tool");
        assert_eq!(spec.build_timestamp, BuildTimestampFormat::Date);
        assert!(!spec.cascade);
        assert!(spec.skip_prereleases);

        if let Source::UrlIndex { versions, url } = &spec.source {
            assert!(url.is_none());
            let versions = versions.as_ref().unwrap();
            assert_eq!(versions.len(), 2);
            assert!(versions["1.1.0"].prerelease);
        } else {
            panic!("Expected UrlIndex source");
        }
    }

    #[test]
    fn parse_url_index_remote_spec() {
        let yaml = r#"
name: test-tool
target:
  registry: localhost:5000
  repository: test-tool
source:
  type: url_index
  url: "https://example.com/versions.json"
assets:
  linux/amd64:
    - "test-tool-.*-linux-amd64\\.tar\\.gz"
"#;

        let spec: MirrorSpec = serde_yaml_ng::from_str(yaml).unwrap();
        if let Source::UrlIndex { url, versions } = &spec.source {
            assert_eq!(url.as_deref(), Some("https://example.com/versions.json"));
            assert!(versions.is_none());
        } else {
            panic!("Expected UrlIndex source");
        }
    }

    #[test]
    fn reject_missing_name() {
        let yaml = r#"
target:
  registry: ocx.sh
  repository: cmake
source:
  type: github_release
  owner: Kitware
  repo: CMake
assets:
  linux/amd64:
    - "cmake-.*\\.tar\\.gz"
"#;

        let result: Result<MirrorSpec, _> = serde_yaml_ng::from_str(yaml);
        assert!(result.is_err());
    }

    #[test]
    fn reject_missing_target() {
        let yaml = r#"
name: cmake
source:
  type: github_release
  owner: Kitware
  repo: CMake
assets:
  linux/amd64:
    - "cmake-.*\\.tar\\.gz"
"#;

        let result: Result<MirrorSpec, _> = serde_yaml_ng::from_str(yaml);
        assert!(result.is_err());
    }

    #[test]
    fn validate_tag_pattern_without_version_group() {
        let yaml = r#"
name: cmake
target:
  registry: ocx.sh
  repository: cmake
source:
  type: github_release
  owner: Kitware
  repo: CMake
  tag_pattern: "^v(\\d+\\.\\d+\\.\\d+)$"
assets:
  linux/amd64:
    - "cmake-.*\\.tar\\.gz"
"#;

        let spec: MirrorSpec = serde_yaml_ng::from_str(yaml).unwrap();
        let errors = spec.validate(Path::new("test.yaml"));
        assert!(
            errors.iter().any(|e| e.contains("version")),
            "Expected version group error, got: {errors:?}"
        );
    }

    #[test]
    fn validate_invalid_regex_in_assets() {
        let yaml = r#"
name: cmake
target:
  registry: ocx.sh
  repository: cmake
source:
  type: github_release
  owner: Kitware
  repo: CMake
assets:
  linux/amd64:
    - "[invalid"
"#;

        let spec: MirrorSpec = serde_yaml_ng::from_str(yaml).unwrap();
        let errors = spec.validate(Path::new("test.yaml"));
        assert!(
            errors.iter().any(|e| e.contains("regex")),
            "Expected regex error, got: {errors:?}"
        );
    }

    #[test]
    fn validate_url_index_with_both_url_and_versions() {
        let yaml = r#"
name: test
target:
  registry: localhost:5000
  repository: test
source:
  type: url_index
  url: "https://example.com/versions.json"
  versions:
    "1.0.0":
      assets:
        test.tar.gz: "https://example.com/test.tar.gz"
assets:
  linux/amd64:
    - "test\\.tar\\.gz"
"#;

        let spec: MirrorSpec = serde_yaml_ng::from_str(yaml).unwrap();
        let errors = spec.validate(Path::new("test.yaml"));
        assert!(
            errors.iter().any(|e| e.contains("exactly one")),
            "Expected url/versions exclusivity error, got: {errors:?}"
        );
    }

    #[test]
    fn validate_url_index_with_neither_url_nor_versions() {
        let yaml = r#"
name: test
target:
  registry: localhost:5000
  repository: test
source:
  type: url_index
assets:
  linux/amd64:
    - "test\\.tar\\.gz"
"#;

        let spec: MirrorSpec = serde_yaml_ng::from_str(yaml).unwrap();
        let errors = spec.validate(Path::new("test.yaml"));
        assert!(
            errors.iter().any(|e| e.contains("exactly one")),
            "Expected url/versions exclusivity error, got: {errors:?}"
        );
    }

    #[test]
    fn default_values() {
        let yaml = r#"
name: minimal
target:
  registry: ocx.sh
  repository: minimal
source:
  type: github_release
  owner: test
  repo: test
assets:
  linux/amd64:
    - "test\\.tar\\.gz"
"#;

        let spec: MirrorSpec = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(spec.build_timestamp, BuildTimestampFormat::Datetime);
        assert!(spec.cascade);
        assert!(!spec.skip_prereleases);
        assert_eq!(spec.concurrency.max_downloads, 8);
        assert_eq!(spec.concurrency.max_pushes, 2);
        assert_eq!(spec.concurrency.rate_limit_ms, 0);
        assert_eq!(spec.concurrency.max_retries, 3);
    }

    #[test]
    fn default_verify_values() {
        let yaml = r#"
name: test
target:
  registry: ocx.sh
  repository: test
source:
  type: github_release
  owner: test
  repo: test
assets:
  linux/amd64:
    - "test\\.tar\\.gz"
verify:
  github_asset_digest: false
"#;

        let spec: MirrorSpec = serde_yaml_ng::from_str(yaml).unwrap();
        let verify = spec.verify.unwrap();
        assert!(!verify.github_asset_digest);
        assert!(verify.checksums_file.is_none());
    }

    #[test]
    fn reject_unknown_source_type() {
        let yaml = r#"
name: test
target:
  registry: ocx.sh
  repository: test
source:
  type: unknown_source
  owner: test
  repo: test
assets:
  linux/amd64:
    - "test\\.tar\\.gz"
"#;

        let result: Result<MirrorSpec, _> = serde_yaml_ng::from_str(yaml);
        assert!(result.is_err());
    }
}
