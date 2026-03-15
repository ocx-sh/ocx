// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use url::Url;

use super::VersionInfo;
use crate::spec::GeneratorConfig;

/// Root of the url_index JSON format.
///
/// Contains a map of version strings to their release assets.
#[derive(Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
pub struct RemoteIndex {
    /// Map of version string (e.g., "22.15.0") to version entry.
    pub versions: HashMap<String, RemoteVersionEntry>,
}

/// A single version's metadata and download assets.
#[derive(Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
pub struct RemoteVersionEntry {
    /// Whether this is a pre-release version.
    #[serde(default)]
    pub prerelease: bool,
    /// Map of asset filename to download URL.
    pub assets: HashMap<String, String>,
}

/// Parse a `RemoteIndex` into a list of `VersionInfo` entries.
fn parse_remote_index(index: RemoteIndex) -> anyhow::Result<Vec<VersionInfo>> {
    let mut versions = Vec::with_capacity(index.versions.len());
    for (version, entry) in index.versions {
        let assets = entry
            .assets
            .into_iter()
            .map(|(name, url_str)| {
                let url = Url::parse(&url_str)
                    .map_err(|e| anyhow::anyhow!("invalid URL for asset '{name}' in version '{version}': {e}"))?;
                Ok((name, url))
            })
            .collect::<anyhow::Result<HashMap<_, _>>>()?;

        versions.push(VersionInfo {
            version,
            assets,
            is_prerelease: entry.prerelease,
        });
    }
    Ok(versions)
}

/// Convert inline versions from the mirror spec into `VersionInfo` entries.
pub fn from_inline(versions: &HashMap<String, crate::spec::UrlIndexVersion>) -> anyhow::Result<Vec<VersionInfo>> {
    let mut result = Vec::with_capacity(versions.len());
    for (version, entry) in versions {
        let assets = entry
            .assets
            .iter()
            .map(|(name, url_str)| {
                let url = Url::parse(url_str)
                    .map_err(|e| anyhow::anyhow!("invalid URL for asset '{name}' in version '{version}': {e}"))?;
                Ok((name.clone(), url))
            })
            .collect::<anyhow::Result<HashMap<_, _>>>()?;

        result.push(VersionInfo {
            version: version.clone(),
            assets,
            is_prerelease: entry.prerelease,
        });
    }
    Ok(result)
}

/// Fetch versions from a remote JSON URL. The JSON format matches the inline `versions` schema:
/// `{ "versions": { "<ver>": { "prerelease": bool, "assets": { "<name>": "<url>" } } } }`
pub async fn from_remote(url: &str) -> anyhow::Result<Vec<VersionInfo>> {
    let response = reqwest::get(url).await?.error_for_status()?;
    let index: RemoteIndex = response.json().await?;
    parse_remote_index(index)
}

/// Run a generator command and parse its stdout as url_index JSON.
pub async fn from_generator(config: &GeneratorConfig, spec_dir: &Path) -> anyhow::Result<Vec<VersionInfo>> {
    let working_dir = config.resolve_working_directory(spec_dir);

    let timeout = Duration::from_secs(config.timeout_seconds);
    let result = tokio::time::timeout(timeout, async {
        let output = tokio::process::Command::new(&config.command[0])
            .args(&config.command[1..])
            .current_dir(&working_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|e| anyhow::anyhow!("failed to run generator '{}': {e}", config.command[0]))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!(
                "generator '{}' failed (exit {}): {}",
                config.command.join(" "),
                output.status,
                stderr.trim()
            );
        }

        if output.stdout.is_empty() {
            anyhow::bail!("generator '{}' produced no output", config.command.join(" "));
        }

        let index: RemoteIndex = serde_json::from_slice(&output.stdout)
            .map_err(|e| anyhow::anyhow!("generator output is not valid url_index JSON: {e}"))?;

        parse_remote_index(index)
    })
    .await;

    match result {
        Ok(inner) => inner,
        Err(_) => anyhow::bail!(
            "generator '{}' timed out after {}s",
            config.command.join(" "),
            config.timeout_seconds
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::UrlIndexVersion;

    #[test]
    fn inline_versions() {
        let mut versions = HashMap::new();
        let mut assets = HashMap::new();
        assets.insert(
            "tool-1.0.0-linux-amd64.tar.gz".to_string(),
            "https://example.com/tool-1.0.0-linux-amd64.tar.gz".to_string(),
        );
        versions.insert(
            "1.0.0".to_string(),
            UrlIndexVersion {
                prerelease: false,
                assets,
            },
        );

        let result = from_inline(&versions).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].version, "1.0.0");
        assert!(!result[0].is_prerelease);
        assert_eq!(result[0].assets.len(), 1);
    }

    #[test]
    fn inline_prerelease_flag() {
        let mut versions = HashMap::new();
        versions.insert(
            "2.0.0-rc1".to_string(),
            UrlIndexVersion {
                prerelease: true,
                assets: HashMap::new(),
            },
        );

        let result = from_inline(&versions).unwrap();
        assert!(result[0].is_prerelease);
    }

    #[test]
    fn inline_empty() {
        let versions = HashMap::new();
        let result = from_inline(&versions).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn parse_remote_index_basic() {
        let mut versions = HashMap::new();
        versions.insert(
            "1.0.0".to_string(),
            RemoteVersionEntry {
                prerelease: false,
                assets: HashMap::from([(
                    "tool-linux.tar.gz".to_string(),
                    "https://example.com/tool-linux.tar.gz".to_string(),
                )]),
            },
        );
        versions.insert(
            "2.0.0-rc1".to_string(),
            RemoteVersionEntry {
                prerelease: true,
                assets: HashMap::from([(
                    "tool-linux.tar.gz".to_string(),
                    "https://example.com/tool-2-linux.tar.gz".to_string(),
                )]),
            },
        );

        let index = RemoteIndex { versions };
        let result = parse_remote_index(index).unwrap();
        assert_eq!(result.len(), 2);

        let v1 = result.iter().find(|v| v.version == "1.0.0").unwrap();
        assert!(!v1.is_prerelease);
        assert_eq!(v1.assets.len(), 1);

        let v2 = result.iter().find(|v| v.version == "2.0.0-rc1").unwrap();
        assert!(v2.is_prerelease);
    }

    #[tokio::test]
    async fn from_generator_valid_output() {
        let config = GeneratorConfig {
            command: vec![
                "sh".to_string(),
                "-c".to_string(),
                r#"echo '{"versions":{"1.0.0":{"prerelease":false,"assets":{"tool.tar.gz":"https://example.com/tool.tar.gz"}}}}'"#.to_string(),
            ],
            working_directory: None,
            timeout_seconds: 10,
        };

        let result = from_generator(&config, Path::new(".")).await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].version, "1.0.0");
        assert!(!result[0].is_prerelease);
        assert_eq!(result[0].assets.len(), 1);
    }

    #[tokio::test]
    async fn from_generator_nonzero_exit() {
        let config = GeneratorConfig {
            command: vec!["sh".to_string(), "-c".to_string(), "echo err >&2; exit 1".to_string()],
            working_directory: None,
            timeout_seconds: 10,
        };

        let err = from_generator(&config, Path::new(".")).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("failed"), "Expected failure message, got: {msg}");
        assert!(msg.contains("err"), "Expected stderr in message, got: {msg}");
    }

    #[tokio::test]
    async fn from_generator_empty_output() {
        let config = GeneratorConfig {
            command: vec!["true".to_string()],
            working_directory: None,
            timeout_seconds: 10,
        };

        let err = from_generator(&config, Path::new(".")).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("no output"), "Expected 'no output' error, got: {msg}");
    }

    #[tokio::test]
    async fn from_generator_invalid_json() {
        let config = GeneratorConfig {
            command: vec!["echo".to_string(), "not json".to_string()],
            working_directory: None,
            timeout_seconds: 10,
        };

        let err = from_generator(&config, Path::new(".")).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("not valid"), "Expected JSON parse error, got: {msg}");
    }

    #[tokio::test]
    async fn from_generator_timeout() {
        let config = GeneratorConfig {
            command: vec!["sleep".to_string(), "10".to_string()],
            working_directory: None,
            timeout_seconds: 1,
        };

        let err = from_generator(&config, Path::new(".")).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("timed out"), "Expected timeout error, got: {msg}");
    }

    #[tokio::test]
    async fn from_generator_command_not_found() {
        let config = GeneratorConfig {
            command: vec!["nonexistent-command-xyz-12345".to_string()],
            working_directory: None,
            timeout_seconds: 10,
        };

        let err = from_generator(&config, Path::new(".")).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("failed to run"), "Expected spawn failure, got: {msg}");
    }

    #[test]
    fn parse_remote_index_invalid_url() {
        let mut versions = HashMap::new();
        versions.insert(
            "1.0.0".to_string(),
            RemoteVersionEntry {
                prerelease: false,
                assets: HashMap::from([("tool.tar.gz".to_string(), "not-a-url".to_string())]),
            },
        );

        let index = RemoteIndex { versions };
        let result = parse_remote_index(index);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("invalid URL"), "Expected URL error, got: {err}");
    }
}
