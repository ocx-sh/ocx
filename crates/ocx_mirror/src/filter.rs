// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::package::version::Version;

use crate::resolver::asset_resolution::ResolvedPlatformAsset;
use crate::spec::VersionsConfig;
use crate::version_platform_map::VersionPlatformMap;

/// A version with its resolved platform assets, ready for filtering.
#[derive(Debug, Clone)]
pub struct ResolvedVersion {
    pub version: String,
    pub normalized_version: String,
    pub platforms: Vec<ResolvedPlatformAsset>,
    pub is_prerelease: bool,
}

/// Apply the full filter pipeline to a list of resolved versions.
///
/// Filters applied in order:
/// 1. Exact version match (if `exact_version` is set)
/// 2. Skip prereleases (if `skip_prereleases` is true)
/// 3. Apply min/max version bounds
/// 4. Skip versions with no resolved platform assets
/// 5. Subtract already-mirrored versions
/// 6. Apply `new_per_run` cap (oldest first for chronological backfill)
pub fn filter_versions(
    mut versions: Vec<ResolvedVersion>,
    exact_version: Option<&str>,
    skip_prereleases: bool,
    versions_config: Option<&VersionsConfig>,
    existing: &VersionPlatformMap,
) -> Vec<ResolvedVersion> {
    // 1. Exact version match
    if let Some(target) = exact_version {
        versions.retain(|v| v.version == target);
    }

    // 2. Skip prereleases
    if skip_prereleases {
        versions.retain(|v| !v.is_prerelease);
    }

    // 2. Apply min/max bounds
    if let Some(config) = versions_config {
        let min = config.min.as_ref().and_then(|s| Version::parse(s));
        let max = config.max.as_ref().and_then(|s| Version::parse(s));

        if min.is_some() || max.is_some() {
            versions.retain(|v| {
                let parsed = match Version::parse(&v.version) {
                    Some(p) => p,
                    None => return true, // keep unparseable versions
                };
                if let Some(min) = &min
                    && parsed < *min
                {
                    return false;
                }
                if let Some(max) = &max
                    && parsed > *max
                {
                    return false;
                }
                true
            });
        }
    }

    // 3. Skip versions with no resolved platform assets
    versions.retain(|v| !v.platforms.is_empty());

    // 4. Subtract already-mirrored (version, platform) pairs.
    // A version is kept if at least one of its target platforms is not yet pushed.
    // This enables retry of partially-pushed versions (e.g., linux/amd64 succeeded
    // but darwin/arm64 failed on a previous run).
    versions.retain_mut(|v| {
        let version = Version::parse(&v.version).expect("mirror versions must be valid");
        v.platforms.retain(|pa| !existing.has(&version, &pa.platform));
        !v.platforms.is_empty()
    });

    // 5. Sort oldest first and apply new_per_run cap
    versions.sort_by(|a, b| {
        let va = Version::parse(&a.version);
        let vb = Version::parse(&b.version);
        match (va, vb) {
            (Some(a), Some(b)) => a.cmp(&b),
            _ => a.version.cmp(&b.version),
        }
    });

    if let Some(config) = versions_config
        && let Some(cap) = config.new_per_run
    {
        versions.truncate(cap);
    }

    versions
}

#[cfg(test)]
mod tests {
    use ocx_lib::oci::Platform;
    use url::Url;

    use super::*;

    fn platform(s: &str) -> Platform {
        s.parse().unwrap()
    }

    fn rv(version: &str, normalized: &str, prerelease: bool) -> ResolvedVersion {
        ResolvedVersion {
            version: version.to_string(),
            normalized_version: normalized.to_string(),
            platforms: vec![ResolvedPlatformAsset {
                platform: platform("linux/amd64"),
                asset_name: "test.tar.gz".to_string(),
                url: Url::parse("https://example.com/test.tar.gz").unwrap(),
            }],
            is_prerelease: prerelease,
        }
    }

    fn rv_multi(version: &str, normalized: &str, platforms: &[&str]) -> ResolvedVersion {
        ResolvedVersion {
            version: version.to_string(),
            normalized_version: normalized.to_string(),
            platforms: platforms
                .iter()
                .map(|p| ResolvedPlatformAsset {
                    platform: platform(p),
                    asset_name: "test.tar.gz".to_string(),
                    url: Url::parse("https://example.com/test.tar.gz").unwrap(),
                })
                .collect(),
            is_prerelease: false,
        }
    }

    /// Build a VersionPlatformMap with the given (version, platform) pairs already pushed.
    fn existing(pairs: &[(&str, &str)]) -> VersionPlatformMap {
        let mut map = VersionPlatformMap::default();
        for (v, p) in pairs {
            map.add(Version::parse(v).unwrap(), platform(p));
        }
        map
    }

    fn empty() -> VersionPlatformMap {
        VersionPlatformMap::default()
    }

    #[test]
    fn skip_prereleases_when_configured() {
        let versions = vec![
            rv("1.0.0", "1.0.0+ts", false),
            rv("1.1.0-rc1", "1.1.0-rc1+ts", true),
            rv("2.0.0", "2.0.0+ts", false),
        ];

        let result = filter_versions(versions, None, true, None, &empty());
        assert_eq!(result.len(), 2);
        assert!(result.iter().all(|v| !v.is_prerelease));
    }

    #[test]
    fn keep_prereleases_when_not_configured() {
        let versions = vec![rv("1.0.0", "1.0.0+ts", false), rv("1.1.0-rc1", "1.1.0-rc1+ts", true)];

        let result = filter_versions(versions, None, false, None, &empty());
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn filter_min_bound() {
        let versions = vec![
            rv("1.0.0", "1.0.0+ts", false),
            rv("2.0.0", "2.0.0+ts", false),
            rv("3.0.0", "3.0.0+ts", false),
        ];

        let config = VersionsConfig {
            min: Some("2.0.0".to_string()),
            max: None,
            new_per_run: None,
        };

        let result = filter_versions(versions, None, false, Some(&config), &empty());
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].version, "2.0.0");
        assert_eq!(result[1].version, "3.0.0");
    }

    #[test]
    fn filter_max_bound() {
        let versions = vec![
            rv("1.0.0", "1.0.0+ts", false),
            rv("2.0.0", "2.0.0+ts", false),
            rv("3.0.0", "3.0.0+ts", false),
        ];

        let config = VersionsConfig {
            min: None,
            max: Some("2.0.0".to_string()),
            new_per_run: None,
        };

        let result = filter_versions(versions, None, false, Some(&config), &empty());
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].version, "1.0.0");
        assert_eq!(result[1].version, "2.0.0");
    }

    #[test]
    fn subtract_already_mirrored() {
        let versions = vec![
            rv("1.0.0", "1.0.0_20260313150000", false),
            rv("2.0.0", "2.0.0_20260313150000", false),
            rv("3.0.0", "3.0.0_20260313150000", false),
        ];

        // 1.0.0 and 3.0.0 already pushed for linux/amd64
        let existing = existing(&[("1.0.0", "linux/amd64"), ("3.0.0", "linux/amd64")]);

        let result = filter_versions(versions, None, false, None, &existing);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].version, "2.0.0");
    }

    #[test]
    fn subtract_already_mirrored_with_build_metadata() {
        let versions = vec![
            rv("1.0.0+build1", "1.0.0_build1_20260313150000", false),
            rv("2.0.0", "2.0.0_20260313150000", false),
        ];

        // "1.0.0+build1" normalizes to "1.0.0_build1" — already pushed
        let existing = existing(&[("1.0.0_build1", "linux/amd64")]);

        let result = filter_versions(versions, None, false, None, &existing);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].version, "2.0.0");
    }

    #[test]
    fn new_per_run_cap() {
        let versions = vec![
            rv("1.0.0", "1.0.0+ts", false),
            rv("2.0.0", "2.0.0+ts", false),
            rv("3.0.0", "3.0.0+ts", false),
        ];

        let config = VersionsConfig {
            min: None,
            max: None,
            new_per_run: Some(1),
        };

        let result = filter_versions(versions, None, false, Some(&config), &empty());
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].version, "1.0.0");
    }

    #[test]
    fn combined_filters() {
        let versions = vec![
            rv("0.9.0", "0.9.0+ts", false),
            rv("1.0.0", "1.0.0+ts", false),
            rv("1.1.0-rc1", "1.1.0-rc1+ts", true),
            rv("2.0.0", "2.0.0+ts", false),
            rv("3.0.0", "3.0.0+ts", false),
        ];

        let config = VersionsConfig {
            min: Some("1.0.0".to_string()),
            max: Some("3.0.0".to_string()),
            new_per_run: Some(2),
        };

        let existing = existing(&[("1.0.0", "linux/amd64")]);

        let result = filter_versions(versions, None, true, Some(&config), &existing);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].version, "2.0.0");
        assert_eq!(result[1].version, "3.0.0");
    }

    #[test]
    fn skip_versions_with_no_platforms() {
        let mut no_platforms = rv("1.0.0", "1.0.0+ts", false);
        no_platforms.platforms = vec![];

        let versions = vec![no_platforms, rv("2.0.0", "2.0.0+ts", false)];

        let result = filter_versions(versions, None, false, None, &empty());
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].version, "2.0.0");
    }

    #[test]
    fn subtract_already_mirrored_prerelease() {
        let versions = vec![
            rv("1.0.0-rc1", "1.0.0-rc1_20260313150000", true),
            rv("2.0.0", "2.0.0_20260313150000", false),
        ];

        let existing = existing(&[("1.0.0-rc1", "linux/amd64")]);

        let result = filter_versions(versions, None, false, None, &existing);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].version, "2.0.0");
    }

    #[test]
    fn exact_version_filter() {
        let versions = vec![
            rv("1.0.0", "1.0.0+ts", false),
            rv("2.0.0", "2.0.0+ts", false),
            rv("3.0.0", "3.0.0+ts", false),
        ];

        let result = filter_versions(versions, Some("2.0.0"), false, None, &empty());
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].version, "2.0.0");
    }

    #[test]
    fn exact_version_no_match() {
        let versions = vec![rv("1.0.0", "1.0.0+ts", false), rv("2.0.0", "2.0.0+ts", false)];

        let result = filter_versions(versions, Some("9.9.9"), false, None, &empty());
        assert!(result.is_empty());
    }

    #[test]
    fn exact_version_already_mirrored() {
        let versions = vec![rv("2.0.0", "2.0.0_20260313150000", false)];
        let existing = existing(&[("2.0.0", "linux/amd64")]);

        let result = filter_versions(versions, Some("2.0.0"), false, None, &existing);
        assert!(result.is_empty());
    }

    #[test]
    fn empty_input() {
        let result = filter_versions(vec![], None, false, None, &empty());
        assert!(result.is_empty());
    }

    #[test]
    fn all_filtered() {
        let versions = vec![rv("1.0.0", "1.0.0_20260313150000", false)];
        let existing = existing(&[("1.0.0", "linux/amd64")]);

        let result = filter_versions(versions, None, false, None, &existing);
        assert!(result.is_empty());
    }

    #[test]
    fn partial_platform_retry() {
        // Version has 2 platforms, only 1 is already pushed
        let versions = vec![rv_multi("1.0.0", "1.0.0_ts", &["linux/amd64", "darwin/arm64"])];
        let existing = existing(&[("1.0.0", "linux/amd64")]);

        let result = filter_versions(versions, None, false, None, &existing);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].platforms.len(), 1);
        assert_eq!(result[0].platforms[0].platform, platform("darwin/arm64"));
    }

    #[test]
    fn all_platforms_pushed_filters_version() {
        let versions = vec![rv_multi("1.0.0", "1.0.0_ts", &["linux/amd64", "darwin/arm64"])];
        let existing = existing(&[("1.0.0", "linux/amd64"), ("1.0.0", "darwin/arm64")]);

        let result = filter_versions(versions, None, false, None, &existing);
        assert!(result.is_empty());
    }

    #[test]
    fn no_platforms_pushed_keeps_all() {
        let versions = vec![rv_multi("1.0.0", "1.0.0_ts", &["linux/amd64", "darwin/arm64"])];

        let result = filter_versions(versions, None, false, None, &empty());
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].platforms.len(), 2);
    }

    #[test]
    fn version_normalizes_plus_to_underscore() {
        let v = Version::parse("3.15.0+build1").unwrap();
        assert_eq!(v.to_string(), "3.15.0_build1");
    }

    #[test]
    fn version_no_plus_unchanged() {
        let v = Version::parse("3.15.0").unwrap();
        assert_eq!(v.to_string(), "3.15.0");
    }

    #[test]
    fn version_prerelease_with_plus() {
        let v = Version::parse("3.15.0-rc1+build1").unwrap();
        assert_eq!(v.to_string(), "3.15.0-rc1_build1");
    }
}
