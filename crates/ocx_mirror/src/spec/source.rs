// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

const DEFAULT_TAG_PATTERN: &str = r"^v?(?P<version>\d+\.\d+\.\d+)(?:-(?P<prerelease>[0-9a-zA-Z]+))?$";

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Source {
    GithubRelease {
        owner: String,
        repo: String,
        #[serde(default = "default_tag_pattern")]
        tag_pattern: String,
    },
    UrlIndex(UrlIndexSource),
}

/// The three modes of providing url_index data.
///
/// Exactly one of `url`, `versions`, or `generator` must be specified.
/// This is enforced by a custom `Deserialize` impl that rejects
/// missing fields, multiple fields, and unknown fields.
#[derive(Debug)]
pub enum UrlIndexSource {
    /// Fetch url_index JSON from a remote URL.
    Remote { url: String },
    /// Inline version->assets map directly in the mirror spec.
    Inline { versions: HashMap<String, UrlIndexVersion> },
    /// Run an external command that outputs url_index JSON to stdout.
    Generator { generator: GeneratorConfig },
}

/// Helper for validating exactly one url_index mode is specified.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UrlIndexSourceRaw {
    url: Option<String>,
    versions: Option<HashMap<String, UrlIndexVersion>>,
    generator: Option<GeneratorConfig>,
}

impl<'de> Deserialize<'de> for UrlIndexSource {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = UrlIndexSourceRaw::deserialize(deserializer)?;
        match (raw.url, raw.versions, raw.generator) {
            (Some(url), None, None) => Ok(UrlIndexSource::Remote { url }),
            (None, Some(versions), None) => Ok(UrlIndexSource::Inline { versions }),
            (None, None, Some(generator)) => Ok(UrlIndexSource::Generator { generator }),
            (None, None, None) => Err(serde::de::Error::custom(
                "url_index source requires one of: url, versions, generator",
            )),
            _ => Err(serde::de::Error::custom(
                "url_index source must have exactly one of: url, versions, generator",
            )),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct UrlIndexVersion {
    #[serde(default)]
    pub prerelease: bool,
    pub assets: HashMap<String, String>,
}

/// Configuration for an external generator command.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeneratorConfig {
    /// Command to execute. First element is the executable, rest are arguments.
    /// Must output url_index JSON to stdout.
    pub command: Vec<String>,
    /// Working directory for the command.
    /// Relative paths are resolved from the mirror spec directory.
    /// Default: the spec directory.
    pub working_directory: Option<String>,
    /// Timeout in seconds for the generator command. Default: 60.
    #[serde(default = "default_generator_timeout")]
    pub timeout_seconds: u64,
}

impl GeneratorConfig {
    /// Resolve the working directory for this generator.
    /// Default: spec directory. If `working_directory` is set, resolve relative to spec dir.
    pub fn resolve_working_directory(&self, spec_dir: &Path) -> PathBuf {
        match &self.working_directory {
            Some(wd) => spec_dir.join(wd),
            None => spec_dir.to_path_buf(),
        }
    }
}

fn default_generator_timeout() -> u64 {
    60
}

fn default_tag_pattern() -> String {
    DEFAULT_TAG_PATTERN.to_string()
}

impl Source {
    pub fn validate(&self, errors: &mut Vec<String>) {
        match self {
            Source::GithubRelease { tag_pattern, .. } => match regex::Regex::new(tag_pattern) {
                Ok(re) => {
                    if re.capture_names().flatten().all(|n| n != "version") {
                        errors
                            .push("source.tag_pattern must contain a named capture group (?P<version>...)".to_string());
                    }
                }
                Err(e) => {
                    errors.push(format!("source.tag_pattern is not a valid regex: {e}"));
                }
            },
            Source::UrlIndex(UrlIndexSource::Generator { generator }) => {
                if generator.command.is_empty() {
                    errors.push("source.generator.command must be a non-empty list".to_string());
                }
            }
            Source::UrlIndex(_) => {}
        }
    }
}
