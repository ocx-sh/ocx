// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

pub mod asset_resolution;

use std::collections::{HashMap, HashSet};

use ocx_lib::oci::Platform;
use regex::Regex;
use url::Url;

use asset_resolution::{AmbiguousAsset, AssetResolution, ResolvedPlatformAsset};

/// Resolve assets for each platform using the configured regex patterns.
///
/// For each platform, all patterns are applied against all asset names.
/// - 0 matches → platform absent (skipped, not an error)
/// - 1 distinct asset → resolved
/// - 2+ distinct assets → ambiguous (error)
pub fn resolve_assets(assets: &HashMap<String, Url>, patterns: &HashMap<Platform, Vec<Regex>>) -> AssetResolution {
    let mut resolved = Vec::new();
    let mut ambiguous = Vec::new();

    for (platform, regexes) in patterns {
        let mut matched: HashSet<String> = HashSet::new();

        for regex in regexes {
            for asset_name in assets.keys() {
                if regex.is_match(asset_name) {
                    matched.insert(asset_name.clone());
                }
            }
        }

        match matched.len() {
            0 => {} // Platform absent for this version — skip silently
            1 => {
                let asset_name = matched.into_iter().next().expect("len checked above");
                let url = assets[&asset_name].clone();
                resolved.push(ResolvedPlatformAsset {
                    platform: platform.clone(),
                    asset_name,
                    url,
                });
            }
            _ => {
                ambiguous.push(AmbiguousAsset {
                    platform: platform.clone(),
                    matched_assets: matched.into_iter().collect(),
                });
            }
        }
    }

    if ambiguous.is_empty() {
        AssetResolution::Resolved(resolved)
    } else {
        AssetResolution::Ambiguous(ambiguous)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    fn re(s: &str) -> Regex {
        Regex::new(s).unwrap()
    }

    fn platform(s: &str) -> Platform {
        s.parse().unwrap()
    }

    #[test]
    fn single_pattern_single_match() {
        let mut assets = HashMap::new();
        assets.insert(
            "tool-linux-amd64.tar.gz".to_string(),
            url("https://example.com/tool-linux-amd64.tar.gz"),
        );
        assets.insert(
            "tool-darwin-arm64.tar.gz".to_string(),
            url("https://example.com/tool-darwin-arm64.tar.gz"),
        );

        let mut patterns = HashMap::new();
        patterns.insert(platform("linux/amd64"), vec![re(r"tool-linux-amd64\.tar\.gz")]);
        patterns.insert(platform("darwin/arm64"), vec![re(r"tool-darwin-arm64\.tar\.gz")]);

        match resolve_assets(&assets, &patterns) {
            AssetResolution::Resolved(resolved) => {
                assert_eq!(resolved.len(), 2);
                let linux = resolved.iter().find(|r| r.platform == platform("linux/amd64")).unwrap();
                assert_eq!(linux.asset_name, "tool-linux-amd64.tar.gz");
                let darwin = resolved
                    .iter()
                    .find(|r| r.platform == platform("darwin/arm64"))
                    .unwrap();
                assert_eq!(darwin.asset_name, "tool-darwin-arm64.tar.gz");
            }
            AssetResolution::Ambiguous(_) => panic!("Expected resolved"),
        }
    }

    #[test]
    fn multiple_patterns_same_asset_deduplicates() {
        let mut assets = HashMap::new();
        assets.insert(
            "tool-linux-x86_64.tar.gz".to_string(),
            url("https://example.com/tool.tar.gz"),
        );

        let mut patterns = HashMap::new();
        patterns.insert(
            platform("linux/amd64"),
            vec![re(r"tool-linux-x86_64\.tar\.gz"), re(r"tool-linux-.*\.tar\.gz")],
        );

        match resolve_assets(&assets, &patterns) {
            AssetResolution::Resolved(resolved) => {
                assert_eq!(resolved.len(), 1);
                assert_eq!(resolved[0].asset_name, "tool-linux-x86_64.tar.gz");
            }
            AssetResolution::Ambiguous(_) => panic!("Expected resolved"),
        }
    }

    #[test]
    fn multiple_patterns_different_assets_ambiguous() {
        let mut assets = HashMap::new();
        assets.insert(
            "tool-linux-amd64.tar.gz".to_string(),
            url("https://example.com/a.tar.gz"),
        );
        assets.insert(
            "tool-linux-x86_64.tar.gz".to_string(),
            url("https://example.com/b.tar.gz"),
        );

        let mut patterns = HashMap::new();
        patterns.insert(
            platform("linux/amd64"),
            vec![re(r"tool-linux-amd64\.tar\.gz"), re(r"tool-linux-x86_64\.tar\.gz")],
        );

        match resolve_assets(&assets, &patterns) {
            AssetResolution::Resolved(_) => panic!("Expected ambiguous"),
            AssetResolution::Ambiguous(amb) => {
                assert_eq!(amb.len(), 1);
                assert_eq!(amb[0].platform, platform("linux/amd64"));
                assert_eq!(amb[0].matched_assets.len(), 2);
            }
        }
    }

    #[test]
    fn zero_matches_platform_absent() {
        let mut assets = HashMap::new();
        assets.insert(
            "tool-linux-amd64.tar.gz".to_string(),
            url("https://example.com/tool.tar.gz"),
        );

        let mut patterns = HashMap::new();
        patterns.insert(platform("linux/amd64"), vec![re(r"tool-linux-amd64\.tar\.gz")]);
        patterns.insert(platform("darwin/arm64"), vec![re(r"tool-darwin-arm64\.tar\.gz")]);

        match resolve_assets(&assets, &patterns) {
            AssetResolution::Resolved(resolved) => {
                assert_eq!(resolved.len(), 1);
                assert_eq!(resolved[0].platform, platform("linux/amd64"));
            }
            AssetResolution::Ambiguous(_) => panic!("Expected resolved"),
        }
    }

    #[test]
    fn universal_binary_same_url_different_platforms() {
        let mut assets = HashMap::new();
        assets.insert(
            "tool-macos-universal.tar.gz".to_string(),
            url("https://example.com/tool-macos-universal.tar.gz"),
        );

        let mut patterns = HashMap::new();
        patterns.insert(platform("darwin/amd64"), vec![re(r"tool-macos-universal\.tar\.gz")]);
        patterns.insert(platform("darwin/arm64"), vec![re(r"tool-macos-universal\.tar\.gz")]);

        match resolve_assets(&assets, &patterns) {
            AssetResolution::Resolved(resolved) => {
                assert_eq!(resolved.len(), 2);
                // Both platforms resolve to the same asset
                for r in &resolved {
                    assert_eq!(r.asset_name, "tool-macos-universal.tar.gz");
                }
            }
            AssetResolution::Ambiguous(_) => panic!("Expected resolved"),
        }
    }

    #[test]
    fn cmake_naming_convention_old_and_new() {
        let mut assets = HashMap::new();
        assets.insert(
            "cmake-3.28.0-linux-x86_64.tar.gz".to_string(),
            url("https://example.com/cmake-3.28.0-linux-x86_64.tar.gz"),
        );

        let mut patterns = HashMap::new();
        patterns.insert(
            platform("linux/amd64"),
            vec![
                re(r"cmake-.*-linux-x86_64\.tar\.gz"),
                re(r"cmake-.*-Linux-x86_64\.tar\.gz"),
            ],
        );

        match resolve_assets(&assets, &patterns) {
            AssetResolution::Resolved(resolved) => {
                assert_eq!(resolved.len(), 1);
                assert_eq!(resolved[0].asset_name, "cmake-3.28.0-linux-x86_64.tar.gz");
            }
            AssetResolution::Ambiguous(_) => panic!("Expected resolved"),
        }
    }
}
