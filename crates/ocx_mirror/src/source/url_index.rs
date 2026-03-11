// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashMap;

use url::Url;

use super::VersionInfo;

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
    let body: RemoteIndex = response.json().await?;

    let mut versions = Vec::with_capacity(body.versions.len());
    for (version, entry) in body.versions {
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

#[derive(serde::Deserialize)]
struct RemoteIndex {
    versions: HashMap<String, RemoteVersionEntry>,
}

#[derive(serde::Deserialize)]
struct RemoteVersionEntry {
    #[serde(default)]
    prerelease: bool,
    assets: HashMap<String, String>,
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
}
