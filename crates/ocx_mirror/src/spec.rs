// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

mod asset_type;
mod assets;
mod catalog_config;
mod concurrency_config;
mod metadata_config;
mod notify_config;
mod ocx_mirror_config;
mod platforms_config;
mod source;
mod strip_components_config;
mod target;
mod tests_config;
mod variant;
mod verify_config;
mod versions_config;

pub use asset_type::{AssetType, AssetTypeConfig};
pub use assets::AssetPatterns;
pub use catalog_config::CatalogConfig;
pub use concurrency_config::{ConcurrencyConfig, resolve_compression_threads};
pub use metadata_config::MetadataConfig;
#[allow(unused_imports)]
pub use notify_config::{DiscordConfig, NotifyConfig};
pub use ocx_mirror_config::OcxMirrorConfig;
#[allow(unused_imports)]
pub use platforms_config::{ContainerConfig, PlatformConfig};
pub use source::{GeneratorConfig, Source, UrlIndexSource, UrlIndexVersion};
pub use strip_components_config::StripComponentsConfig;
pub use target::Target;
pub use tests_config::TestEntry;
pub use variant::{EffectiveVariant, VariantSpec};
pub use verify_config::VerifyConfig;
pub(crate) use versions_config::BackfillOrder;
pub use versions_config::VersionsConfig;

use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::error::MirrorError;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
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

    // ── Pipeline test configuration (added in test-pipeline phase) ──
    /// Test commands to run against each installed package before publishing.
    /// Required by `ocx-mirror push`; optional for backwards-compat parsing.
    #[serde(default)]
    pub tests: Option<Vec<TestEntry>>,

    /// Per-platform runner + container matrix for the generated GHA workflow.
    /// Keys must match the OCI platform format (`^[a-z0-9_-]+/[a-z0-9_-]+$`).
    #[serde(default)]
    pub platforms: Option<HashMap<String, PlatformConfig>>,

    /// Pins the `ocx-mirror` release tag (and optionally a git SHA) used
    /// when installing `ocx-mirror` and downloading the `ocx` binary inside
    /// the generated workflow.
    #[serde(default)]
    pub ocx_mirror: Option<OcxMirrorConfig>,

    /// Notification settings (currently only Discord webhooks).
    #[serde(default)]
    pub notify: Option<NotifyConfig>,

    /// Catalog publishing settings (README + logo → `__ocx.desc`).
    /// When omitted, defaults apply: `readme: CATALOG.md`, logo probed.
    #[serde(default)]
    pub catalog: Option<CatalogConfig>,
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

/// Regex for valid test entry names: starts with letter, then letters/digits/hyphens/underscores.
static TEST_NAME_RE: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"^[a-zA-Z][a-zA-Z0-9_-]*$").unwrap());

/// Regex for valid OCI platform keys: `os/arch` format with lowercase alphanumerics, hyphens, underscores.
static PLATFORM_KEY_RE: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"^[a-z0-9_-]+/[a-z0-9_-]+$").unwrap());

/// Regex for valid `release_tag` — semantic version with optional pre-release.
static RELEASE_TAG_RE: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"^v\d+\.\d+\.\d+(-[a-z0-9.]+)?$").unwrap());

/// Regex for a 40-character lowercase hexadecimal git SHA.
static GIT_REV_RE: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"^[0-9a-f]{40}$").unwrap());

/// Regex for valid GitHub Actions secret names: `^[A-Z][A-Z0-9_]+$`.
///
/// Requires at least one uppercase letter, then one or more uppercase letters, digits, or
/// underscores. Names starting with `_` or containing only a single character are rejected
/// (GHA enforces both constraints in practice).
static GHA_SECRET_NAME_RE: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"^[A-Z][A-Z0-9_]+$").unwrap());

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

        if let Some(tests) = &self.tests {
            validate_tests(tests, &mut errors);
        }
        if let Some(platforms) = &self.platforms {
            validate_platforms(platforms, &mut errors);
        }
        if let Some(ocx_mirror) = &self.ocx_mirror {
            validate_ocx_mirror_config(ocx_mirror, &mut errors);
        }
        if let Some(notify) = &self.notify {
            validate_notify_config(notify, &mut errors);
        }

        // Cross-field: release_tag required when any platform declares containers.
        // This catches the case where ocx_mirror block is absent or release_tag is omitted.
        let any_platform_has_containers = self.platforms.as_ref().is_some_and(|plats| {
            plats
                .values()
                .any(|p| p.containers.as_ref().is_some_and(|c| !c.is_empty()))
        });
        if any_platform_has_containers {
            let has_release_tag = self.ocx_mirror.as_ref().and_then(|m| m.release_tag.as_ref()).is_some();
            if !has_release_tag {
                errors.push("ocx_mirror.release_tag is required when any platform declares containers".to_string());
            }
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

// ── Pipeline field validators ────────────────────────────────────────────────

/// Validate `tests:` entries: non-empty, unique names, valid name regex.
fn validate_tests(tests: &[TestEntry], errors: &mut Vec<String>) {
    if tests.is_empty() {
        errors.push("tests: must contain at least one entry".to_string());
        return;
    }

    let mut seen = HashSet::new();
    for entry in tests {
        if !TEST_NAME_RE.is_match(&entry.name) {
            errors.push(format!(
                "tests: invalid name '{}' (must match ^[a-zA-Z][a-zA-Z0-9_-]*$)",
                entry.name
            ));
        }
        if !seen.insert(&entry.name) {
            errors.push(format!("tests: duplicate name '{}'", entry.name));
        }
    }
}

/// Infer the default shell for a container image based on its image-name prefix.
///
/// Returns `Some(shell)` when a well-known distro prefix matches, `None` when
/// the image is non-standard and an explicit `shell` is required.
fn infer_shell_from_image(image: &str) -> Option<&'static str> {
    // Strip optional tag (everything after `:`) and optional registry prefix
    // (everything before the last `/` component that looks like `host/repo`).
    // We only look at the repository basename for prefix matching.
    let image_name = image.split(':').next().unwrap_or(image);
    // Take the last path component for matching (`ubuntu:24.04` → `ubuntu`,
    // `docker.io/library/alpine:3.20` → `alpine`).
    let base = image_name.split('/').next_back().unwrap_or(image_name);

    // Well-known distros that default to bash.
    const BASH_PREFIXES: &[&str] = &["ubuntu", "debian", "fedora", "rocky", "opensuse"];
    // Alpine defaults to sh (no bash by default).
    const SH_PREFIXES: &[&str] = &["alpine"];

    for prefix in BASH_PREFIXES {
        if base.starts_with(prefix) {
            return Some("bash");
        }
    }
    for prefix in SH_PREFIXES {
        if base.starts_with(prefix) {
            return Some("sh");
        }
    }

    None
}

/// Validate `platforms:` map: valid platform keys, runner present, container
/// image format, shell defaults for known distros, explicit shell required for unknown.
fn validate_platforms(platforms: &HashMap<String, PlatformConfig>, errors: &mut Vec<String>) {
    for (key, config) in platforms {
        if !PLATFORM_KEY_RE.is_match(key) {
            errors.push(format!(
                "platforms: invalid key '{key}' (must be os/arch format, e.g. linux/amd64)"
            ));
        }

        if config.runner.trim().is_empty() {
            errors.push(format!("platforms: '{key}': runner must not be empty"));
        }

        if let Some(containers) = &config.containers {
            if containers.is_empty() {
                errors.push(format!(
                    "platforms: '{key}': containers must contain at least one entry when declared"
                ));
            } else {
                for container in containers {
                    // If no explicit shell, the image must have a known default.
                    if container.shell.is_none() && infer_shell_from_image(&container.image).is_none() {
                        errors.push(format!(
                            "platforms: '{key}': container image '{}' has ambiguous shell; \
                             set an explicit shell (e.g. shell: bash)",
                            container.image
                        ));
                    }
                }
            }
        }
    }
}

/// Validate `ocx_mirror:` block: release_tag format, rev format.
fn validate_ocx_mirror_config(config: &OcxMirrorConfig, errors: &mut Vec<String>) {
    if let Some(tag) = &config.release_tag
        && !RELEASE_TAG_RE.is_match(tag)
    {
        errors.push(format!(
            "ocx_mirror: release_tag '{tag}' must match ^v\\d+\\.\\d+\\.\\d+(-[a-z0-9.]+)?$"
        ));
    }

    if let Some(rev) = &config.rev
        && !GIT_REV_RE.is_match(rev)
    {
        errors.push(format!(
            "ocx_mirror: rev '{rev}' must be a 40-character lowercase hex SHA"
        ));
    }
}

/// Content-policy check on the `notify:` block.
///
/// Rejects any `webhook_secret` value that looks like a hardcoded URL. This is a
/// *policy* violation (exit 64 / `SpecUsageError`), distinct from the structural
/// format check in `validate_notify_config` (exit 65 / `SpecInvalid`).
///
/// Call this from `load_spec` **before** `spec.validate()` so the correct exit code
/// is returned even when a structurally-valid spec contains a bad policy choice.
pub(crate) fn policy_check_notify(notify: &NotifyConfig) -> Result<(), MirrorError> {
    let Some(discord) = &notify.discord else {
        return Ok(());
    };
    let secret = &discord.webhook_secret;

    // R3 mitigation: reject any hardcoded URL — catches accidental paste of the raw webhook URL.
    if secret.starts_with("https://") || secret.starts_with("http://") {
        return Err(MirrorError::SpecUsageError(format!(
            "webhook_secret: hardcoded URL not allowed; use a GitHub Actions secret name instead (got '{secret}')"
        )));
    }
    if secret.contains("discord.com") || secret.contains("discordapp.com") {
        return Err(MirrorError::SpecUsageError(format!(
            "webhook_secret: value must not contain a Discord URL; use a GitHub Actions secret name instead (got '{secret}')"
        )));
    }

    Ok(())
}

/// Validate `notify:` block: webhook_secret must be a valid GHA secret name format.
///
/// URL-literal checks are handled separately by [`policy_check_notify`] with a
/// `SpecUsageError` (exit 64). This function only checks the structural format,
/// contributing to `SpecInvalid` (exit 65) errors.
fn validate_notify_config(config: &NotifyConfig, errors: &mut Vec<String>) {
    let Some(discord) = &config.discord else {
        return;
    };

    let secret = &discord.webhook_secret;

    // Must match GHA secret name format.
    if !GHA_SECRET_NAME_RE.is_match(secret) {
        errors.push(format!(
            "webhook_secret: '{secret}' is not a valid GitHub Actions secret name \
             (must match ^[A-Z][A-Z0-9_]+$)"
        ));
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

    // Policy check (exit 64 / SpecUsageError) must run before structural validate
    // (exit 65 / SpecInvalid) so the correct exit code is returned for URL-literal
    // webhook secrets.
    if let Some(notify) = &spec.notify {
        policy_check_notify(notify)?;
    }

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

    // ── §3.1 S1: Pipeline schema round-trip and validation tests ────────────

    /// Helper: base YAML suitable for all §3.1 round-trip tests. Adds the
    /// minimum required fields so pipeline-specific blocks can be appended.
    const MINIMAL_BASE_YAML: &str = r#"
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
  linux/arm64:
    - "shfmt_v.*_linux_arm64$"
  darwin/arm64:
    - "shfmt_v.*_darwin_arm64$"
asset_type:
  type: binary
  name: shfmt
"#;

    #[test]
    fn round_trip_full_pipeline_spec() {
        // §3.1: Round-trip: valid mirror.yml with full tests:, platforms:,
        // ocx_mirror:, notify: blocks parses correctly.
        let yaml = format!(
            r#"{base}
tests:
  - name: version
    command: shfmt --version
  - name: smoke
    command: bash ./tests/smoke.sh

platforms:
  linux/amd64:
    runner: ubuntu-latest
    containers:
      - image: ubuntu:24.04
        shell: bash
      - image: alpine:3.20
        shell: sh
  darwin/arm64:
    runner: macos-latest
    shell: bash
  windows/amd64:
    runner: windows-latest
    shell: pwsh
    tests:
      - name: version
        command: shfmt.exe --version

ocx_mirror:
  release_tag: v0.7.2
  rev: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

notify:
  discord:
    webhook_secret: DISCORD_WEBHOOK_URL
"#,
            base = MINIMAL_BASE_YAML
        );

        let spec: MirrorSpec = serde_yaml_ng::from_str(&yaml).unwrap();

        // tests block
        let tests = spec.tests.as_ref().unwrap();
        assert_eq!(tests.len(), 2);
        assert_eq!(tests[0].name, "version");
        assert_eq!(tests[0].command, "shfmt --version");
        assert_eq!(tests[1].name, "smoke");

        // platforms block
        let platforms = spec.platforms.as_ref().unwrap();
        assert!(platforms.contains_key("linux/amd64"));
        assert!(platforms.contains_key("darwin/arm64"));
        assert!(platforms.contains_key("windows/amd64"));

        let linux = &platforms["linux/amd64"];
        assert_eq!(linux.runner, "ubuntu-latest");
        let containers = linux.containers.as_ref().unwrap();
        assert_eq!(containers.len(), 2);
        assert_eq!(containers[0].image, "ubuntu:24.04");
        assert_eq!(containers[0].shell.as_deref(), Some("bash"));
        assert_eq!(containers[1].image, "alpine:3.20");

        // per-platform test override
        let windows = &platforms["windows/amd64"];
        let win_tests = windows.tests.as_ref().unwrap();
        assert_eq!(win_tests.len(), 1);
        assert_eq!(win_tests[0].name, "version");
        assert_eq!(win_tests[0].command, "shfmt.exe --version");

        // ocx_mirror block
        let ocx_mirror = spec.ocx_mirror.as_ref().unwrap();
        assert_eq!(ocx_mirror.release_tag.as_deref(), Some("v0.7.2"));
        assert_eq!(
            ocx_mirror.rev.as_deref(),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );

        // notify block
        let notify = spec.notify.as_ref().unwrap();
        let discord = notify.discord.as_ref().unwrap();
        assert_eq!(discord.webhook_secret, "DISCORD_WEBHOOK_URL");
    }

    #[test]
    fn backward_compat_existing_mirrors_parse() {
        // §3.1: Backward-compat regression: parse every mirrors/*/mirror.yml
        // unchanged — none should fail to deserialize. Uses glob to ensure
        // we catch new mirrors added to the repo.
        //
        // NOTE: validate() is NOT called here because existing mirrors don't
        // have the new required pipeline fields. This only verifies serde
        // deserialization does not break.
        let pattern = concat!(env!("CARGO_MANIFEST_DIR"), "/../../mirrors/*/mirror.yml");
        let paths: Vec<_> = glob::glob(pattern)
            .expect("glob pattern should be valid")
            .filter_map(|r| r.ok())
            .collect();

        assert!(
            !paths.is_empty(),
            "No mirrors/*/mirror.yml found — check path relative to crate"
        );

        for path in &paths {
            let content =
                std::fs::read_to_string(path).unwrap_or_else(|e| panic!("Failed to read {}: {e}", path.display()));
            let result: Result<MirrorSpec, _> = serde_yaml_ng::from_str(&content);
            assert!(result.is_ok(), "Failed to parse {}: {:?}", path.display(), result.err());
        }
    }

    #[test]
    fn validate_empty_tests_array() {
        // §3.1: Rejection — empty tests: array
        let yaml = format!(
            r#"{base}
tests: []
platforms:
  linux/amd64:
    runner: ubuntu-latest
    containers:
      - image: ubuntu:24.04
        shell: bash
ocx_mirror:
  release_tag: v0.7.2
"#,
            base = MINIMAL_BASE_YAML
        );

        let spec: MirrorSpec = serde_yaml_ng::from_str(&yaml).unwrap();
        let errors = spec.validate(Path::new("test.yml"));
        assert!(
            errors
                .iter()
                .any(|e| e.contains("tests") && (e.contains("empty") || e.contains("least"))),
            "Expected error about empty tests:, got: {errors:?}"
        );
    }

    #[test]
    fn validate_duplicate_test_names() {
        // §3.1: Rejection — duplicate tests[].name
        let yaml = format!(
            r#"{base}
tests:
  - name: version
    command: shfmt --version
  - name: version
    command: shfmt --version --again
platforms:
  linux/amd64:
    runner: ubuntu-latest
ocx_mirror:
  release_tag: v0.7.2
"#,
            base = MINIMAL_BASE_YAML
        );

        let spec: MirrorSpec = serde_yaml_ng::from_str(&yaml).unwrap();
        let errors = spec.validate(Path::new("test.yml"));
        assert!(
            errors.iter().any(|e| e.contains("duplicate") || e.contains("unique")),
            "Expected duplicate test name error, got: {errors:?}"
        );
    }

    #[test]
    fn validate_invalid_test_name_starts_with_digit() {
        // §3.1: Rejection — invalid tests[].name (starts with digit)
        let yaml = format!(
            r#"{base}
tests:
  - name: 1version
    command: shfmt --version
platforms:
  linux/amd64:
    runner: ubuntu-latest
"#,
            base = MINIMAL_BASE_YAML
        );

        let spec: MirrorSpec = serde_yaml_ng::from_str(&yaml).unwrap();
        let errors = spec.validate(Path::new("test.yml"));
        assert!(
            errors.iter().any(|e| e.contains("name") || e.contains("invalid")),
            "Expected invalid test name error, got: {errors:?}"
        );
    }

    #[test]
    fn validate_invalid_platform_key_no_arch() {
        // §3.1: Rejection — invalid platform key (linux without arch)
        let yaml = format!(
            r#"{base}
tests:
  - name: version
    command: shfmt --version
platforms:
  linux:
    runner: ubuntu-latest
"#,
            base = MINIMAL_BASE_YAML
        );

        let spec: MirrorSpec = serde_yaml_ng::from_str(&yaml).unwrap();
        let errors = spec.validate(Path::new("test.yml"));
        assert!(
            errors.iter().any(|e| e.contains("platform") || e.contains("linux")),
            "Expected invalid platform key error, got: {errors:?}"
        );
    }

    #[test]
    fn validate_platform_missing_runner() {
        // §3.1: Rejection — missing runner on declared platform
        // PlatformConfig.runner is required (non-optional) so this fails at
        // parse time with serde error.
        let yaml = format!(
            r#"{base}
tests:
  - name: version
    command: shfmt --version
platforms:
  linux/amd64:
    containers:
      - image: ubuntu:24.04
        shell: bash
"#,
            base = MINIMAL_BASE_YAML
        );

        // Missing required `runner` field → serde parse error
        let result: Result<MirrorSpec, _> = serde_yaml_ng::from_str(&yaml);
        assert!(result.is_err(), "Expected parse error for missing runner, but got Ok");
    }

    #[test]
    fn validate_empty_containers_array() {
        // §3.1: Rejection — empty containers: array (must be absent OR ≥1)
        let yaml = format!(
            r#"{base}
tests:
  - name: version
    command: shfmt --version
platforms:
  linux/amd64:
    runner: ubuntu-latest
    containers: []
ocx_mirror:
  release_tag: v0.7.2
"#,
            base = MINIMAL_BASE_YAML
        );

        let spec: MirrorSpec = serde_yaml_ng::from_str(&yaml).unwrap();
        let errors = spec.validate(Path::new("test.yml"));
        assert!(
            errors
                .iter()
                .any(|e| e.contains("container") && (e.contains("empty") || e.contains("least"))),
            "Expected error about empty containers:, got: {errors:?}"
        );
    }

    #[test]
    fn validate_ambiguous_shell_on_nonstandard_image() {
        // §3.1: Rejection — ambiguous shell on non-standard image (no default)
        let yaml = format!(
            r#"{base}
tests:
  - name: version
    command: shfmt --version
platforms:
  linux/amd64:
    runner: ubuntu-latest
    containers:
      - image: mycorp/custom-runner:1.0
ocx_mirror:
  release_tag: v0.7.2
"#,
            base = MINIMAL_BASE_YAML
        );

        let spec: MirrorSpec = serde_yaml_ng::from_str(&yaml).unwrap();
        let errors = spec.validate(Path::new("test.yml"));
        assert!(
            errors.iter().any(|e| e.contains("shell") || e.contains("ambiguous")),
            "Expected ambiguous shell error for non-standard image, got: {errors:?}"
        );
    }

    #[test]
    fn validate_release_tag_required_when_linux_has_containers() {
        // §3.1: Rejection — ocx_mirror.release_tag absent when any linux platform has containers
        let yaml = format!(
            r#"{base}
tests:
  - name: version
    command: shfmt --version
platforms:
  linux/amd64:
    runner: ubuntu-latest
    containers:
      - image: ubuntu:24.04
        shell: bash
"#,
            base = MINIMAL_BASE_YAML
        );

        let spec: MirrorSpec = serde_yaml_ng::from_str(&yaml).unwrap();
        let errors = spec.validate(Path::new("test.yml"));
        assert!(
            errors
                .iter()
                .any(|e| e.contains("release_tag") || e.contains("ocx_mirror")),
            "Expected error about missing release_tag when containers declared, got: {errors:?}"
        );
    }

    #[test]
    fn validate_release_tag_format() {
        // §3.1: Rejection — release_tag not matching ^v\d+\.\d+\.\d+(-[a-z0-9.]+)?$
        let yaml = format!(
            r#"{base}
tests:
  - name: version
    command: shfmt --version
platforms:
  linux/amd64:
    runner: ubuntu-latest
    containers:
      - image: ubuntu:24.04
        shell: bash
ocx_mirror:
  release_tag: "not-a-semver"
"#,
            base = MINIMAL_BASE_YAML
        );

        let spec: MirrorSpec = serde_yaml_ng::from_str(&yaml).unwrap();
        let errors = spec.validate(Path::new("test.yml"));
        assert!(
            errors
                .iter()
                .any(|e| e.contains("release_tag") || e.contains("semver") || e.contains("format")),
            "Expected invalid release_tag format error, got: {errors:?}"
        );
    }

    #[test]
    fn validate_rev_not_40_hex() {
        // §3.1: Rejection — rev not 40-hex
        let yaml = format!(
            r#"{base}
tests:
  - name: version
    command: shfmt --version
platforms:
  linux/amd64:
    runner: ubuntu-latest
ocx_mirror:
  release_tag: v0.7.2
  rev: "short"
"#,
            base = MINIMAL_BASE_YAML
        );

        let spec: MirrorSpec = serde_yaml_ng::from_str(&yaml).unwrap();
        let errors = spec.validate(Path::new("test.yml"));
        assert!(
            errors
                .iter()
                .any(|e| e.contains("rev") || e.contains("hex") || e.contains("40")),
            "Expected invalid rev format error, got: {errors:?}"
        );
    }

    #[test]
    fn validate_rejects_ocx_install_block() {
        // §3.1: Rejection — ocx_install: block present at all → SpecUsageError
        // Catches early adopters who copied an earlier draft spec.
        // Since ocx_install is not in the schema, serde rejects unknown fields
        // OR it silently ignores them (depends on #[serde(deny_unknown_fields)]).
        // We test via validate() returning an error for this field.
        //
        // Implementation note: the validator should check for `ocx_install` key
        // via a raw YAML pass or a dedicated sentinel field, and emit:
        //   "ocx binary is installed via direct release download; remove `ocx_install:` block"
        let yaml = format!(
            r#"{base}
tests:
  - name: version
    command: shfmt --version
platforms:
  linux/amd64:
    runner: ubuntu-latest
ocx_mirror:
  release_tag: v0.7.2
ocx_install: {{}}
"#,
            base = MINIMAL_BASE_YAML
        );

        // If serde rejects unknown fields, this is a parse error.
        // If serde ignores unknown fields, it's a validate() error.
        // Either satisfies the rejection requirement.
        let result: Result<MirrorSpec, _> = serde_yaml_ng::from_str(&yaml);
        match result {
            Err(_) => {
                // serde rejected the unknown field — test passes
            }
            Ok(spec) => {
                let errors = spec.validate(Path::new("test.yml"));
                assert!(
                    errors
                        .iter()
                        .any(|e| e.contains("ocx_install") || e.contains("release download")),
                    "Expected rejection of ocx_install: block, got: {errors:?}"
                );
            }
        }
    }

    #[test]
    fn validate_r3_discord_com_in_webhook_secret() {
        // §3.1 R3 mitigation: webhook_secret containing "discord.com" → rejected
        let yaml = format!(
            r#"{base}
tests:
  - name: version
    command: shfmt --version
platforms:
  linux/amd64:
    runner: ubuntu-latest
notify:
  discord:
    webhook_secret: "https://discord.com/api/webhooks/1234/token"
"#,
            base = MINIMAL_BASE_YAML
        );

        let spec: MirrorSpec = serde_yaml_ng::from_str(&yaml).unwrap();
        let errors = spec.validate(Path::new("test.yml"));
        assert!(
            errors
                .iter()
                .any(|e| e.contains("webhook_secret") || e.contains("discord") || e.contains("URL")),
            "Expected R3 rejection for discord.com URL in webhook_secret, got: {errors:?}"
        );
    }

    #[test]
    fn validate_r3_discordapp_com_in_webhook_secret() {
        // §3.1 R3 mitigation: webhook_secret containing "discordapp.com" → rejected
        let yaml = format!(
            r#"{base}
tests:
  - name: version
    command: shfmt --version
platforms:
  linux/amd64:
    runner: ubuntu-latest
notify:
  discord:
    webhook_secret: "https://discordapp.com/api/webhooks/1234/token"
"#,
            base = MINIMAL_BASE_YAML
        );

        let spec: MirrorSpec = serde_yaml_ng::from_str(&yaml).unwrap();
        let errors = spec.validate(Path::new("test.yml"));
        assert!(
            errors
                .iter()
                .any(|e| e.contains("webhook_secret") || e.contains("discordapp") || e.contains("URL")),
            "Expected R3 rejection for discordapp.com URL in webhook_secret, got: {errors:?}"
        );
    }

    #[test]
    fn validate_r3_http_url_in_webhook_secret() {
        // §3.1 R3 mitigation: webhook_secret matching ^https?:// → rejected
        let yaml = format!(
            r#"{base}
tests:
  - name: version
    command: shfmt --version
platforms:
  linux/amd64:
    runner: ubuntu-latest
notify:
  discord:
    webhook_secret: "https://example.com/webhook/abc123"
"#,
            base = MINIMAL_BASE_YAML
        );

        let spec: MirrorSpec = serde_yaml_ng::from_str(&yaml).unwrap();
        let errors = spec.validate(Path::new("test.yml"));
        assert!(
            errors
                .iter()
                .any(|e| e.contains("webhook_secret") || e.contains("https") || e.contains("URL")),
            "Expected R3 rejection for http:// URL in webhook_secret, got: {errors:?}"
        );
    }

    #[test]
    fn validate_r3_valid_secret_name_accepted() {
        // §3.1 R3 positive: valid GHA secret name accepted without error
        let yaml = format!(
            r#"{base}
tests:
  - name: version
    command: shfmt --version
platforms:
  linux/amd64:
    runner: ubuntu-latest
    containers:
      - image: ubuntu:24.04
        shell: bash
ocx_mirror:
  release_tag: v0.7.2
notify:
  discord:
    webhook_secret: DISCORD_WEBHOOK_URL
"#,
            base = MINIMAL_BASE_YAML
        );

        let spec: MirrorSpec = serde_yaml_ng::from_str(&yaml).unwrap();
        let errors = spec.validate(Path::new("test.yml"));
        // No webhook_secret errors expected
        let webhook_errors: Vec<_> = errors
            .iter()
            .filter(|e| e.contains("webhook_secret") || e.contains("discord"))
            .collect();
        assert!(
            webhook_errors.is_empty(),
            "Unexpected webhook_secret errors for valid GHA secret name: {webhook_errors:?}"
        );
    }

    #[test]
    fn validate_per_platform_tests_override_replaces_top_level() {
        // §3.1: Per-platform tests: override replaces top-level entirely (no merge)
        let yaml = format!(
            r#"{base}
tests:
  - name: version
    command: shfmt --version
  - name: smoke
    command: bash ./tests/smoke.sh
platforms:
  linux/amd64:
    runner: ubuntu-latest
    containers:
      - image: ubuntu:24.04
        shell: bash
  windows/amd64:
    runner: windows-latest
    shell: pwsh
    tests:
      - name: version
        command: shfmt.exe --version
ocx_mirror:
  release_tag: v0.7.2
"#,
            base = MINIMAL_BASE_YAML
        );

        let spec: MirrorSpec = serde_yaml_ng::from_str(&yaml).unwrap();
        let platforms = spec.platforms.as_ref().unwrap();

        // Top-level tests: 2 entries
        let top_tests = spec.tests.as_ref().unwrap();
        assert_eq!(top_tests.len(), 2);

        // windows/amd64 override: 1 entry only (replacement, not merge)
        let windows = &platforms["windows/amd64"];
        let win_tests = windows.tests.as_ref().unwrap();
        assert_eq!(
            win_tests.len(),
            1,
            "Per-platform override must replace, not merge top-level tests"
        );
        assert_eq!(win_tests[0].name, "version");

        // linux/amd64 has no override — platforms[].tests is None
        let linux = &platforms["linux/amd64"];
        assert!(
            linux.tests.is_none(),
            "linux/amd64 must inherit top-level tests (no override)"
        );
    }

    #[test]
    fn validate_default_shell_alpine_infers_sh() {
        // §3.1: Default-from-image shell inference: alpine:3.20 → sh
        let yaml = format!(
            r#"{base}
tests:
  - name: version
    command: shfmt --version
platforms:
  linux/amd64:
    runner: ubuntu-latest
    containers:
      - image: alpine:3.20
ocx_mirror:
  release_tag: v0.7.2
"#,
            base = MINIMAL_BASE_YAML
        );

        let spec: MirrorSpec = serde_yaml_ng::from_str(&yaml).unwrap();
        let errors = spec.validate(Path::new("test.yml"));
        // alpine:3.20 has a known default (sh) — no ambiguous shell error expected
        let shell_errors: Vec<_> = errors
            .iter()
            .filter(|e| e.contains("shell") || e.contains("ambiguous"))
            .collect();
        assert!(
            shell_errors.is_empty(),
            "alpine:3.20 should have inferred shell 'sh'; got errors: {shell_errors:?}"
        );
    }

    #[test]
    fn validate_default_shell_ubuntu_infers_bash() {
        // §3.1: Default-from-image shell inference: ubuntu:24.04 → bash
        let yaml = format!(
            r#"{base}
tests:
  - name: version
    command: shfmt --version
platforms:
  linux/amd64:
    runner: ubuntu-latest
    containers:
      - image: ubuntu:24.04
ocx_mirror:
  release_tag: v0.7.2
"#,
            base = MINIMAL_BASE_YAML
        );

        let spec: MirrorSpec = serde_yaml_ng::from_str(&yaml).unwrap();
        let errors = spec.validate(Path::new("test.yml"));
        let shell_errors: Vec<_> = errors
            .iter()
            .filter(|e| e.contains("shell") || e.contains("ambiguous"))
            .collect();
        assert!(
            shell_errors.is_empty(),
            "ubuntu:24.04 should have inferred shell 'bash'; got errors: {shell_errors:?}"
        );
    }
}
