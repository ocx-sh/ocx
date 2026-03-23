// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

mod asset_type;
mod assets;
mod concurrency_config;
mod metadata_config;
mod source;
mod strip_components_config;
mod target;
mod variant;
mod verify_config;
mod versions_config;

pub use asset_type::{AssetType, AssetTypeConfig};
pub use assets::AssetPatterns;
pub use concurrency_config::{ConcurrencyConfig, resolve_compression_threads};
pub use metadata_config::MetadataConfig;
pub use source::{GeneratorConfig, Source, UrlIndexSource, UrlIndexVersion};
pub use strip_components_config::StripComponentsConfig;
pub use target::Target;
pub use variant::{EffectiveVariant, VariantSpec};
pub use verify_config::VerifyConfig;
pub(crate) use versions_config::BackfillOrder;
pub use versions_config::VersionsConfig;

use serde::Deserialize;
use std::collections::HashSet;
use std::path::Path;

use crate::error::MirrorError;

#[derive(Debug, Deserialize)]
pub struct MirrorSpec {
    pub name: String,
    pub target: Target,
    pub source: Source,

    /// Asset patterns for non-variant specs. Mutually exclusive with `variants`.
    #[serde(default)]
    pub assets: Option<AssetPatterns>,

    /// Variant declarations. Mutually exclusive with top-level `assets`.
    /// Each variant has its own asset patterns and can override `metadata`
    /// and `asset_type` from the top-level spec.
    #[serde(default)]
    pub variants: Option<Vec<VariantSpec>>,

    #[serde(default)]
    pub metadata: Option<MetadataConfig>,

    /// How to process downloaded assets before bundling.
    ///
    /// - `archive`: Extract the asset as a tar/zip archive, optionally stripping
    ///   leading path components (e.g. `strip_components: 1`).
    /// - `binary`: The asset is a standalone executable. Place it directly into
    ///   the content directory under the configured `name`.
    ///
    /// Defaults to `archive` with no stripping when omitted.
    #[serde(default)]
    pub asset_type: Option<AssetTypeConfig>,

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

/// Regex for valid variant names: starts with lowercase letter, then lowercase
/// letters, digits, or dots.
static VARIANT_NAME_RE: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"^[a-z][a-z0-9.]*$").unwrap());

impl MirrorSpec {
    pub fn validate(&self, spec_path: &Path) -> Vec<String> {
        let mut errors = Vec::new();
        let spec_dir = spec_path.parent().unwrap_or(Path::new("."));

        self.source.validate(&mut errors);

        // Validate assets/variants mutual exclusivity
        match (&self.assets, &self.variants) {
            (Some(_), Some(_)) => {
                errors.push("cannot specify both top-level 'assets' and 'variants'".to_string());
            }
            (None, None) => {
                errors.push("must specify either 'assets' or 'variants'".to_string());
            }
            (Some(assets), None) => {
                assets.validate(&mut errors);
            }
            (None, Some(variants)) => {
                self.validate_variants(variants, spec_dir, &mut errors);
            }
        }

        if let Some(metadata) = &self.metadata {
            metadata.validate(spec_dir, &mut errors);
        }

        if let Some(versions) = &self.versions {
            versions.validate(&mut errors);
        }

        errors
    }

    fn validate_variants(&self, variants: &[VariantSpec], spec_dir: &Path, errors: &mut Vec<String>) {
        if variants.is_empty() {
            errors.push("variants: must declare at least one variant".to_string());
            return;
        }

        let default_count = variants.iter().filter(|v| v.default).count();
        if default_count != 1 {
            errors.push(format!(
                "variants: exactly one variant must be default, found {default_count}"
            ));
        }

        let mut seen_names: HashSet<Option<&String>> = HashSet::new();
        for v in variants {
            match &v.name {
                Some(name) => {
                    // Name format
                    if !VARIANT_NAME_RE.is_match(name) {
                        errors.push(format!("variants: invalid name '{name}' (must match [a-z][a-z0-9.]*)",));
                    }

                    // Reserved name
                    if name == "latest" {
                        errors.push("variants: 'latest' is reserved and cannot be used as a variant name".to_string());
                    }
                }
                None => {
                    // Unnamed variant must be the default
                    if !v.default {
                        errors.push("variants: unnamed variant must be the default".to_string());
                    }
                }
            }

            // Duplicate check (None counts as a unique entry)
            if !seen_names.insert(v.name.as_ref()) {
                match &v.name {
                    Some(name) => errors.push(format!("variants: duplicate name '{name}'")),
                    None => errors.push("variants: duplicate unnamed variant".to_string()),
                }
            }

            // Per-variant asset validation
            v.assets.validate(errors);

            // Per-variant metadata validation
            if let Some(metadata) = &v.metadata {
                metadata.validate(spec_dir, errors);
            }
        }
    }

    /// Returns the effective variant list, handling backward compatibility.
    ///
    /// - No `variants` key: single synthetic variant using top-level fields.
    /// - With `variants` key: one [`EffectiveVariant`] per declared variant,
    ///   inheriting top-level `metadata` and `asset_type` as defaults.
    pub fn effective_variants(&self) -> Vec<EffectiveVariant> {
        match &self.variants {
            Some(variants) => variants
                .iter()
                .map(|v| EffectiveVariant {
                    name: v.name.clone(),
                    is_default: v.default,
                    assets: v.assets.clone(),
                    metadata: v.metadata.clone().or_else(|| self.metadata.clone()),
                    asset_type: v.asset_type.clone().or_else(|| self.asset_type.clone()),
                })
                .collect(),
            None => vec![EffectiveVariant {
                name: None,
                is_default: true,
                assets: self
                    .assets
                    .clone()
                    .expect("validated: assets or variants must be present"),
                metadata: self.metadata.clone(),
                asset_type: self.asset_type.clone(),
            }],
        }
    }
}

/// Load and validate a mirror spec from a YAML file, resolving `extends` chains.
///
/// If the spec contains an `extends` key, the referenced base file is loaded first
/// and the child's top-level keys are shallow-merged on top. Chains of arbitrary
/// depth are supported; circular references are detected and rejected.
pub async fn load_spec(spec_path: &Path) -> Result<MirrorSpec, MirrorError> {
    if !spec_path.exists() {
        return Err(MirrorError::SpecNotFound(spec_path.display().to_string()));
    }

    let content = tokio::fs::read_to_string(spec_path)
        .await
        .map_err(|e| MirrorError::SpecNotFound(format!("{}: {e}", spec_path.display())))?;

    let chain = resolve_extends_chain(spec_path, &content).await?;

    let merged = if chain.is_empty() {
        // No extends — parse directly
        serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&content)
            .map_err(|e| MirrorError::SpecInvalid(vec![format!("YAML parse error: {e}")]))?
    } else {
        // Load chain in reverse (grandparent first), shallow-merge each layer on top
        let mut base = serde_yaml_ng::Value::Mapping(serde_yaml_ng::Mapping::new());
        for path in chain.iter().rev() {
            let file_content = tokio::fs::read_to_string(path)
                .await
                .map_err(|e| MirrorError::SpecNotFound(format!("{}: {e}", path.display())))?;
            let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(&file_content)
                .map_err(|e| MirrorError::SpecInvalid(vec![format!("YAML parse error in {}: {e}", path.display())]))?;
            shallow_merge(&mut base, value);
        }
        // Finally merge the child (spec_path itself) on top
        let child: serde_yaml_ng::Value = serde_yaml_ng::from_str(&content)
            .map_err(|e| MirrorError::SpecInvalid(vec![format!("YAML parse error: {e}")]))?;
        shallow_merge(&mut base, child);
        // Strip the extends key from the merged result
        if let serde_yaml_ng::Value::Mapping(ref mut map) = base {
            map.remove("extends");
        }
        base
    };

    let spec: MirrorSpec = serde_yaml_ng::from_value(merged)
        .map_err(|e| MirrorError::SpecInvalid(vec![format!("YAML parse error: {e}")]))?;

    let errors = spec.validate(spec_path);
    if !errors.is_empty() {
        return Err(MirrorError::SpecInvalid(errors));
    }

    Ok(spec)
}

/// Walk the `extends` chain collecting file paths: [parent, grandparent, ...].
/// Detects circular dependencies via `HashSet<PathBuf>`.
async fn resolve_extends_chain(spec_path: &Path, content: &str) -> Result<Vec<std::path::PathBuf>, MirrorError> {
    let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(content)
        .map_err(|e| MirrorError::SpecInvalid(vec![format!("YAML parse error: {e}")]))?;

    let mapping = match &value {
        serde_yaml_ng::Value::Mapping(m) => m,
        _ => return Ok(vec![]),
    };

    let extends_value = match mapping.get("extends") {
        Some(v) => v,
        None => return Ok(vec![]),
    };

    let spec_dir = spec_path.parent().unwrap_or(Path::new("."));
    let mut chain = Vec::new();
    let mut seen = HashSet::new();
    seen.insert(spec_path.canonicalize().unwrap_or_else(|_| spec_path.to_path_buf()));

    // Start with the first extends reference
    let mut current_extends = extends_value.clone();
    let mut current_dir = spec_dir.to_path_buf();

    loop {
        let base_rel = match current_extends.as_str() {
            Some(s) => s.to_string(),
            None => {
                return Err(MirrorError::SpecInvalid(vec![
                    "extends: value must be a string path".to_string(),
                ]));
            }
        };

        let base_path = current_dir.join(&base_rel);
        if !base_path.exists() {
            return Err(MirrorError::SpecInvalid(vec![format!(
                "extends: base file not found: {}",
                base_path.display()
            )]));
        }

        let canonical = base_path.canonicalize().unwrap_or_else(|_| base_path.clone());
        if !seen.insert(canonical) {
            // Build a nice cycle description
            let cycle: Vec<String> = std::iter::once(spec_path.display().to_string())
                .chain(chain.iter().map(|p: &std::path::PathBuf| p.display().to_string()))
                .chain(std::iter::once(base_path.display().to_string()))
                .collect();
            return Err(MirrorError::SpecInvalid(vec![format!(
                "extends: circular dependency: {}",
                cycle.join(" -> ")
            )]));
        }

        chain.push(base_path.clone());

        // Check if the base file also has an extends
        let base_content = tokio::fs::read_to_string(&base_path)
            .await
            .map_err(|e| MirrorError::SpecNotFound(format!("{}: {e}", base_path.display())))?;
        let base_value: serde_yaml_ng::Value = serde_yaml_ng::from_str(&base_content)
            .map_err(|e| MirrorError::SpecInvalid(vec![format!("YAML parse error in {}: {e}", base_path.display())]))?;

        match base_value.as_mapping().and_then(|m| m.get("extends")) {
            Some(next) => {
                current_extends = next.clone();
                current_dir = base_path.parent().unwrap_or(Path::new(".")).to_path_buf();
            }
            None => break,
        }
    }

    Ok(chain)
}

/// Shallow-merge: for each top-level key in `overlay`, replace the corresponding
/// key in `base` entirely. No recursion into nested maps.
fn shallow_merge(base: &mut serde_yaml_ng::Value, overlay: serde_yaml_ng::Value) {
    let base_map = match base {
        serde_yaml_ng::Value::Mapping(m) => m,
        _ => return,
    };
    let overlay_map = match overlay {
        serde_yaml_ng::Value::Mapping(m) => m,
        _ => return,
    };
    for (key, value) in overlay_map {
        base_map.insert(key, value);
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

        if let Source::UrlIndex(UrlIndexSource::Inline { versions }) = &spec.source {
            assert_eq!(versions.len(), 2);
            assert!(versions["1.1.0"].prerelease);
        } else {
            panic!("Expected UrlIndex Inline source, got: {:?}", spec.source);
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
        if let Source::UrlIndex(UrlIndexSource::Remote { url }) = &spec.source {
            assert_eq!(url, "https://example.com/versions.json");
        } else {
            panic!("Expected UrlIndex Remote source, got: {:?}", spec.source);
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
    fn reject_url_index_with_neither_url_nor_versions_nor_generator() {
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

        let result: Result<MirrorSpec, _> = serde_yaml_ng::from_str(yaml);
        assert!(result.is_err(), "Expected parse error for empty url_index");
    }

    #[test]
    fn parse_url_index_generator_spec() {
        let yaml = r#"
name: nodejs
target:
  registry: ocx.sh
  repository: nodejs
source:
  type: url_index
  generator:
    command: ["uv", "run", "generate.py"]
    working_directory: scripts
assets:
  linux/amd64:
    - "node-.*-linux-x64\\.tar\\.xz"
"#;

        let spec: MirrorSpec = serde_yaml_ng::from_str(yaml).unwrap();
        if let Source::UrlIndex(UrlIndexSource::Generator { generator }) = &spec.source {
            assert_eq!(generator.command, vec!["uv", "run", "generate.py"]);
            assert_eq!(generator.working_directory.as_deref(), Some("scripts"));
        } else {
            panic!("Expected UrlIndex Generator source, got: {:?}", spec.source);
        }
    }

    #[test]
    fn parse_url_index_generator_default_working_directory() {
        let yaml = r#"
name: nodejs
target:
  registry: ocx.sh
  repository: nodejs
source:
  type: url_index
  generator:
    command: ["uv", "run", "generate.py"]
assets:
  linux/amd64:
    - "node-.*-linux-x64\\.tar\\.xz"
"#;

        let spec: MirrorSpec = serde_yaml_ng::from_str(yaml).unwrap();
        if let Source::UrlIndex(UrlIndexSource::Generator { generator }) = &spec.source {
            assert!(generator.working_directory.is_none());
            let resolved = generator.resolve_working_directory(Path::new("/mirrors/nodejs"));
            assert_eq!(resolved, Path::new("/mirrors/nodejs"));
        } else {
            panic!("Expected UrlIndex Generator source, got: {:?}", spec.source);
        }
    }

    #[test]
    fn validate_generator_empty_command() {
        let yaml = r#"
name: test
target:
  registry: localhost:5000
  repository: test
source:
  type: url_index
  generator:
    command: []
assets:
  linux/amd64:
    - "test\\.tar\\.gz"
"#;

        let spec: MirrorSpec = serde_yaml_ng::from_str(yaml).unwrap();
        let errors = spec.validate(Path::new("test.yaml"));
        assert!(
            errors.iter().any(|e| e.contains("non-empty")),
            "Expected empty command error, got: {errors:?}"
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
        assert!(spec.asset_type.is_none(), "asset_type should default to None");
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
    fn parse_asset_type_archive() {
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
    - "cmake-.*\\.tar\\.gz"
asset_type:
  type: archive
  strip_components: 1
"#;

        let spec: MirrorSpec = serde_yaml_ng::from_str(yaml).unwrap();
        match spec.asset_type.as_ref().unwrap().resolve("linux/amd64") {
            asset_type::AssetType::Archive { strip_components } => assert_eq!(strip_components, Some(1)),
            _ => panic!("expected Archive"),
        }
    }

    #[test]
    fn parse_asset_type_archive_per_platform() {
        let yaml = r#"
name: shellcheck
target:
  registry: ocx.sh
  repository: shellcheck
source:
  type: github_release
  owner: koalaman
  repo: shellcheck
  tag_pattern: "^v(?P<version>\\d+\\.\\d+\\.\\d+)$"
assets:
  linux/amd64:
    - "shellcheck-.*\\.tar\\.xz"
asset_type:
  type: archive
  strip_components:
    default: 1
    platforms:
      windows/amd64: 0
"#;

        let spec: MirrorSpec = serde_yaml_ng::from_str(yaml).unwrap();
        let at = spec.asset_type.as_ref().unwrap();
        match at.resolve("linux/amd64") {
            asset_type::AssetType::Archive { strip_components } => assert_eq!(strip_components, Some(1)),
            _ => panic!("expected Archive"),
        }
        match at.resolve("windows/amd64") {
            asset_type::AssetType::Archive { strip_components } => assert_eq!(strip_components, Some(0)),
            _ => panic!("expected Archive"),
        }
    }

    #[test]
    fn parse_asset_type_binary() {
        let yaml = r#"
name: shfmt
target:
  registry: ocx.sh
  repository: shfmt
source:
  type: github_release
  owner: mvdan
  repo: sh
  tag_pattern: "^v(?P<version>\\d+\\.\\d+\\.\\d+)$"
assets:
  linux/amd64:
    - "shfmt_v.*_linux_amd64$"
asset_type:
  type: binary
  name: shfmt
"#;

        let spec: MirrorSpec = serde_yaml_ng::from_str(yaml).unwrap();
        match spec.asset_type.as_ref().unwrap().resolve("linux/amd64") {
            asset_type::AssetType::Binary { name } => assert_eq!(name, "shfmt"),
            _ => panic!("expected Binary"),
        }
    }

    #[test]
    fn reject_url_index_with_both_url_and_versions() {
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

        let result: Result<MirrorSpec, _> = serde_yaml_ng::from_str(yaml);
        assert!(
            result.is_err(),
            "Expected parse error for url_index with both url and versions"
        );
        let err = result.unwrap_err().to_string();
        assert!(err.contains("exactly one"), "Expected 'exactly one' error, got: {err}");
    }

    #[test]
    fn reject_url_index_with_both_url_and_generator() {
        let yaml = r#"
name: test
target:
  registry: localhost:5000
  repository: test
source:
  type: url_index
  url: "https://example.com/versions.json"
  generator:
    command: ["echo", "{}"]
assets:
  linux/amd64:
    - "test\\.tar\\.gz"
"#;

        let result: Result<MirrorSpec, _> = serde_yaml_ng::from_str(yaml);
        assert!(
            result.is_err(),
            "Expected parse error for url_index with both url and generator"
        );
        let err = result.unwrap_err().to_string();
        assert!(err.contains("exactly one"), "Expected 'exactly one' error, got: {err}");
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

    // -- extends tests --

    #[tokio::test]
    async fn load_spec_without_extends() {
        let dir = tempfile::tempdir().unwrap();
        let spec_path = dir.path().join("mirror.yml");
        std::fs::write(
            &spec_path,
            r#"
name: test
target:
  registry: ocx.sh
  repository: test
source:
  type: github_release
  owner: test
  repo: test
  tag_pattern: "^v(?P<version>\\d+)$"
assets:
  linux/amd64:
    - "test\\.tar\\.gz"
"#,
        )
        .unwrap();

        let spec = load_spec(&spec_path).await.unwrap();
        assert_eq!(spec.name, "test");
        assert!(spec.cascade);
    }

    #[tokio::test]
    async fn load_spec_extends_happy_path() {
        let dir = tempfile::tempdir().unwrap();

        std::fs::write(
            dir.path().join("base.yml"),
            r#"
target:
  registry: ocx.sh
  repository: test
assets:
  linux/amd64:
    - "test\\.tar\\.gz"
cascade: true
build_timestamp: none
"#,
        )
        .unwrap();

        std::fs::write(
            dir.path().join("child.yml"),
            r#"
extends: base.yml
name: child-test
source:
  type: github_release
  owner: test
  repo: test
  tag_pattern: "^v(?P<version>\\d+)$"
"#,
        )
        .unwrap();

        let spec = load_spec(&dir.path().join("child.yml")).await.unwrap();
        assert_eq!(spec.name, "child-test");
        assert_eq!(spec.target.registry, "ocx.sh");
        assert!(spec.cascade);
        assert_eq!(spec.build_timestamp, BuildTimestampFormat::None);
    }

    #[tokio::test]
    async fn load_spec_extends_shallow_override() {
        let dir = tempfile::tempdir().unwrap();

        std::fs::write(
            dir.path().join("base.yml"),
            r#"
target:
  registry: ocx.sh
  repository: test
assets:
  linux/amd64:
    - "base\\.tar\\.gz"
  darwin/arm64:
    - "base-darwin\\.tar\\.gz"
versions:
  min: "1.0.0"
  new_per_run: 5
"#,
        )
        .unwrap();

        std::fs::write(
            dir.path().join("child.yml"),
            r#"
extends: base.yml
name: child
source:
  type: github_release
  owner: test
  repo: test
  tag_pattern: "^v(?P<version>\\d+)$"
versions:
  min: "8.0.0"
  new_per_run: 10
"#,
        )
        .unwrap();

        let spec = load_spec(&dir.path().join("child.yml")).await.unwrap();
        // versions should be entirely replaced, not deep-merged
        let versions = spec.versions.unwrap();
        assert_eq!(versions.min.as_deref(), Some("8.0.0"));
        assert_eq!(versions.new_per_run, Some(10));
        // assets should still come from base (not overridden)
        assert!(matches!(spec.source, Source::GithubRelease { .. }));
    }

    #[tokio::test]
    async fn load_spec_extends_circular() {
        let dir = tempfile::tempdir().unwrap();

        std::fs::write(
            dir.path().join("a.yml"),
            r#"
extends: b.yml
name: a
"#,
        )
        .unwrap();

        std::fs::write(
            dir.path().join("b.yml"),
            r#"
extends: a.yml
name: b
"#,
        )
        .unwrap();

        let err = load_spec(&dir.path().join("a.yml")).await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("circular dependency"),
            "Expected circular error, got: {msg}"
        );
    }

    #[tokio::test]
    async fn load_spec_extends_file_not_found() {
        let dir = tempfile::tempdir().unwrap();

        std::fs::write(
            dir.path().join("child.yml"),
            r#"
extends: nonexistent.yml
name: child
"#,
        )
        .unwrap();

        let err = load_spec(&dir.path().join("child.yml")).await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("base file not found"),
            "Expected not found error, got: {msg}"
        );
    }

    #[tokio::test]
    async fn load_spec_extends_missing_required_fields() {
        let dir = tempfile::tempdir().unwrap();

        // Base provides target but no source
        std::fs::write(
            dir.path().join("base.yml"),
            r#"
target:
  registry: ocx.sh
  repository: test
assets:
  linux/amd64:
    - "test\\.tar\\.gz"
"#,
        )
        .unwrap();

        // Child adds name but no source — merged result is missing required `source`
        std::fs::write(
            dir.path().join("child.yml"),
            r#"
extends: base.yml
name: incomplete
"#,
        )
        .unwrap();

        let err = load_spec(&dir.path().join("child.yml")).await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("source") || msg.contains("missing"),
            "Expected missing field error, got: {msg}"
        );
    }

    #[tokio::test]
    async fn load_spec_extends_chain() {
        let dir = tempfile::tempdir().unwrap();

        // grandparent: provides target and assets
        std::fs::write(
            dir.path().join("grandparent.yml"),
            r#"
target:
  registry: ocx.sh
  repository: test
assets:
  linux/amd64:
    - "test\\.tar\\.gz"
cascade: false
build_timestamp: date
"#,
        )
        .unwrap();

        // parent: extends grandparent, overrides cascade
        std::fs::write(
            dir.path().join("parent.yml"),
            r#"
extends: grandparent.yml
cascade: true
skip_prereleases: true
"#,
        )
        .unwrap();

        // child: extends parent, adds name and source
        std::fs::write(
            dir.path().join("child.yml"),
            r#"
extends: parent.yml
name: chain-test
source:
  type: github_release
  owner: test
  repo: test
  tag_pattern: "^v(?P<version>\\d+)$"
"#,
        )
        .unwrap();

        let spec = load_spec(&dir.path().join("child.yml")).await.unwrap();
        assert_eq!(spec.name, "chain-test");
        assert_eq!(spec.target.registry, "ocx.sh");
        // cascade: grandparent=false, parent=true → true
        assert!(spec.cascade);
        // build_timestamp: grandparent=date, not overridden → date
        assert_eq!(spec.build_timestamp, BuildTimestampFormat::Date);
        // skip_prereleases: parent=true → true
        assert!(spec.skip_prereleases);
    }

    // -- variant tests --

    #[test]
    fn parse_spec_with_variants() {
        let yaml = r#"
name: python
target:
  registry: ocx.sh
  repository: python
source:
  type: github_release
  owner: astral-sh
  repo: python-build-standalone
  tag_pattern: "^(?P<version>\\d+\\.\\d+\\.\\d+)\\+\\d+$"
variants:
  - name: pgo.lto
    default: true
    assets:
      linux/amd64:
        - "cpython-.*-x86_64-.*-pgo\\+lto-.*\\.tar\\.zst"
      darwin/arm64:
        - "cpython-.*-aarch64-apple-darwin-pgo\\+lto-.*\\.tar\\.zst"
  - name: debug
    assets:
      linux/amd64:
        - "cpython-.*-x86_64-.*-debug-.*\\.tar\\.zst"
metadata:
  default: metadata/python.json
"#;

        let spec: MirrorSpec = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(spec.name, "python");
        assert!(spec.assets.is_none(), "top-level assets should be None");
        let variants = spec.variants.as_ref().unwrap();
        assert_eq!(variants.len(), 2);
        assert_eq!(variants[0].name.as_deref(), Some("pgo.lto"));
        assert!(variants[0].default);
        assert_eq!(variants[1].name.as_deref(), Some("debug"));
        assert!(!variants[1].default);
    }

    #[test]
    fn parse_spec_without_variants_backward_compat() {
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
"#;

        let spec: MirrorSpec = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(spec.assets.is_some());
        assert!(spec.variants.is_none());
        let errors = spec.validate(Path::new("test.yaml"));
        assert!(errors.is_empty(), "backward-compat spec should validate: {errors:?}");
    }

    #[test]
    fn validate_reject_both_assets_and_variants() {
        let yaml = r#"
name: test
target:
  registry: ocx.sh
  repository: test
source:
  type: github_release
  owner: test
  repo: test
  tag_pattern: "^v(?P<version>\\d+)$"
assets:
  linux/amd64:
    - "test\\.tar\\.gz"
variants:
  - name: debug
    default: true
    assets:
      linux/amd64:
        - "test-debug\\.tar\\.gz"
"#;

        let spec: MirrorSpec = serde_yaml_ng::from_str(yaml).unwrap();
        let errors = spec.validate(Path::new("test.yaml"));
        assert!(
            errors.iter().any(|e| e.contains("cannot specify both")),
            "Expected mutual exclusivity error, got: {errors:?}"
        );
    }

    #[test]
    fn validate_reject_neither_assets_nor_variants() {
        let yaml = r#"
name: test
target:
  registry: ocx.sh
  repository: test
source:
  type: github_release
  owner: test
  repo: test
  tag_pattern: "^v(?P<version>\\d+)$"
"#;

        let spec: MirrorSpec = serde_yaml_ng::from_str(yaml).unwrap();
        let errors = spec.validate(Path::new("test.yaml"));
        assert!(
            errors.iter().any(|e| e.contains("must specify either")),
            "Expected missing assets/variants error, got: {errors:?}"
        );
    }

    #[test]
    fn validate_variant_exactly_one_default() {
        let yaml = r#"
name: test
target:
  registry: ocx.sh
  repository: test
source:
  type: github_release
  owner: test
  repo: test
  tag_pattern: "^v(?P<version>\\d+)$"
variants:
  - name: debug
    assets:
      linux/amd64:
        - "test-debug\\.tar\\.gz"
  - name: release
    assets:
      linux/amd64:
        - "test-release\\.tar\\.gz"
"#;

        let spec: MirrorSpec = serde_yaml_ng::from_str(yaml).unwrap();
        let errors = spec.validate(Path::new("test.yaml"));
        assert!(
            errors.iter().any(|e| e.contains("exactly one variant must be default")),
            "Expected default count error, got: {errors:?}"
        );
    }

    #[test]
    fn validate_variant_two_defaults() {
        let yaml = r#"
name: test
target:
  registry: ocx.sh
  repository: test
source:
  type: github_release
  owner: test
  repo: test
  tag_pattern: "^v(?P<version>\\d+)$"
variants:
  - name: debug
    default: true
    assets:
      linux/amd64:
        - "test-debug\\.tar\\.gz"
  - name: release
    default: true
    assets:
      linux/amd64:
        - "test-release\\.tar\\.gz"
"#;

        let spec: MirrorSpec = serde_yaml_ng::from_str(yaml).unwrap();
        let errors = spec.validate(Path::new("test.yaml"));
        assert!(
            errors
                .iter()
                .any(|e| e.contains("exactly one variant must be default, found 2")),
            "Expected two-default error, got: {errors:?}"
        );
    }

    #[test]
    fn validate_variant_invalid_name() {
        let yaml = r#"
name: test
target:
  registry: ocx.sh
  repository: test
source:
  type: github_release
  owner: test
  repo: test
  tag_pattern: "^v(?P<version>\\d+)$"
variants:
  - name: Debug-Build
    default: true
    assets:
      linux/amd64:
        - "test\\.tar\\.gz"
"#;

        let spec: MirrorSpec = serde_yaml_ng::from_str(yaml).unwrap();
        let errors = spec.validate(Path::new("test.yaml"));
        assert!(
            errors.iter().any(|e| e.contains("invalid name")),
            "Expected invalid name error, got: {errors:?}"
        );
    }

    #[test]
    fn validate_variant_latest_reserved() {
        let yaml = r#"
name: test
target:
  registry: ocx.sh
  repository: test
source:
  type: github_release
  owner: test
  repo: test
  tag_pattern: "^v(?P<version>\\d+)$"
variants:
  - name: latest
    default: true
    assets:
      linux/amd64:
        - "test\\.tar\\.gz"
"#;

        let spec: MirrorSpec = serde_yaml_ng::from_str(yaml).unwrap();
        let errors = spec.validate(Path::new("test.yaml"));
        assert!(
            errors.iter().any(|e| e.contains("reserved")),
            "Expected reserved name error, got: {errors:?}"
        );
    }

    #[test]
    fn validate_variant_duplicate_names() {
        let yaml = r#"
name: test
target:
  registry: ocx.sh
  repository: test
source:
  type: github_release
  owner: test
  repo: test
  tag_pattern: "^v(?P<version>\\d+)$"
variants:
  - name: debug
    default: true
    assets:
      linux/amd64:
        - "test\\.tar\\.gz"
  - name: debug
    assets:
      linux/amd64:
        - "test2\\.tar\\.gz"
"#;

        let spec: MirrorSpec = serde_yaml_ng::from_str(yaml).unwrap();
        let errors = spec.validate(Path::new("test.yaml"));
        assert!(
            errors.iter().any(|e| e.contains("duplicate")),
            "Expected duplicate name error, got: {errors:?}"
        );
    }

    #[test]
    fn effective_variants_without_variants_key() {
        let yaml = r#"
name: cmake
target:
  registry: ocx.sh
  repository: cmake
source:
  type: github_release
  owner: test
  repo: test
  tag_pattern: "^v(?P<version>\\d+)$"
assets:
  linux/amd64:
    - "cmake-.*\\.tar\\.gz"
metadata:
  default: metadata/cmake.json
"#;

        let spec: MirrorSpec = serde_yaml_ng::from_str(yaml).unwrap();
        let variants = spec.effective_variants();
        assert_eq!(variants.len(), 1);
        assert!(variants[0].name.is_none());
        assert!(variants[0].is_default);
        assert!(variants[0].metadata.is_some());
    }

    #[test]
    fn effective_variants_unnamed_default_with_named_variant() {
        let yaml = r#"
name: cpython
target:
  registry: ocx.sh
  repository: cpython
source:
  type: github_release
  owner: test
  repo: test
  tag_pattern: "^v(?P<version>\\d+)$"
variants:
  - default: true
    assets:
      linux/amd64:
        - "install_only\\.tar\\.gz"
  - name: slim
    assets:
      linux/amd64:
        - "install_only_stripped\\.tar\\.gz"
"#;

        let spec: MirrorSpec = serde_yaml_ng::from_str(yaml).unwrap();
        let errors = spec.validate(Path::new("test.yaml"));
        assert!(errors.is_empty(), "Expected no errors, got: {errors:?}");

        let variants = spec.effective_variants();
        assert_eq!(variants.len(), 2);

        assert!(variants[0].name.is_none());
        assert!(variants[0].is_default);

        assert_eq!(variants[1].name.as_deref(), Some("slim"));
        assert!(!variants[1].is_default);
    }

    #[test]
    fn validate_variant_unnamed_non_default_rejected() {
        let yaml = r#"
name: test
target:
  registry: ocx.sh
  repository: test
source:
  type: github_release
  owner: test
  repo: test
  tag_pattern: "^v(?P<version>\\d+)$"
variants:
  - name: release
    default: true
    assets:
      linux/amd64:
        - "release\\.tar\\.gz"
  - assets:
      linux/amd64:
        - "other\\.tar\\.gz"
"#;

        let spec: MirrorSpec = serde_yaml_ng::from_str(yaml).unwrap();
        let errors = spec.validate(Path::new("test.yaml"));
        assert!(
            errors.iter().any(|e| e.contains("unnamed variant must be the default")),
            "Expected unnamed-must-be-default error, got: {errors:?}"
        );
    }

    #[test]
    fn effective_variants_with_variants_key() {
        let yaml = r#"
name: python
target:
  registry: ocx.sh
  repository: python
source:
  type: github_release
  owner: test
  repo: test
  tag_pattern: "^v(?P<version>\\d+)$"
variants:
  - name: pgo.lto
    default: true
    assets:
      linux/amd64:
        - "pgo-lto-.*\\.tar\\.gz"
  - name: debug
    assets:
      linux/amd64:
        - "debug-.*\\.tar\\.gz"
metadata:
  default: metadata/python.json
"#;

        let spec: MirrorSpec = serde_yaml_ng::from_str(yaml).unwrap();
        let variants = spec.effective_variants();
        assert_eq!(variants.len(), 2);

        assert_eq!(variants[0].name.as_deref(), Some("pgo.lto"));
        assert!(variants[0].is_default);
        // Inherits top-level metadata
        assert!(variants[0].metadata.is_some());

        assert_eq!(variants[1].name.as_deref(), Some("debug"));
        assert!(!variants[1].is_default);
        // Also inherits top-level metadata
        assert!(variants[1].metadata.is_some());
    }

    #[test]
    fn effective_variants_variant_overrides_metadata() {
        let yaml = r#"
name: python
target:
  registry: ocx.sh
  repository: python
source:
  type: github_release
  owner: test
  repo: test
  tag_pattern: "^v(?P<version>\\d+)$"
variants:
  - name: pgo.lto
    default: true
    assets:
      linux/amd64:
        - "pgo-lto-.*\\.tar\\.gz"
    metadata:
      default: metadata/python-pgo.json
  - name: debug
    assets:
      linux/amd64:
        - "debug-.*\\.tar\\.gz"
metadata:
  default: metadata/python.json
"#;

        let spec: MirrorSpec = serde_yaml_ng::from_str(yaml).unwrap();
        let variants = spec.effective_variants();

        // pgo.lto overrides metadata
        let pgo = &variants[0];
        assert!(pgo.metadata.is_some());

        // debug inherits top-level metadata
        let debug = &variants[1];
        assert!(debug.metadata.is_some());
    }
}
