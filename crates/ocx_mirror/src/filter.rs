// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::package::version::Version;

use crate::resolver::asset_resolution::ResolvedPlatformAsset;
use crate::spec::{BackfillOrder, VersionsConfig};
use crate::version_platform_map::VersionPlatformMap;

/// A version with its resolved platform assets, ready for filtering.
#[derive(Debug, Clone)]
pub struct ResolvedVersion {
    pub version: String,
    pub normalized_version: String,
    /// Variant name for this resolution, used for variant-aware already-mirrored checks.
    pub variant: Option<String>,
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
/// 5. Sort by version
/// 6. Keep only the latest (highest) version (if `latest` is true)
/// 7. Subtract already-mirrored versions
/// 8. Apply `new_per_run` cap respecting `backfill` order:
///    - `newest_first` (default): newest non-mirrored versions first
///    - `oldest_first`: chronological backfill from the oldest
///
/// Note: `latest` is applied before subtracting already-mirrored versions so that
/// `--latest` always targets the true latest version. If it's already mirrored,
/// the result is empty rather than falling back to the next-highest version.
pub fn filter_versions(
    mut versions: Vec<ResolvedVersion>,
    exact_versions: &[String],
    skip_prereleases: bool,
    versions_config: Option<&VersionsConfig>,
    existing: &VersionPlatformMap,
    latest: bool,
) -> Vec<ResolvedVersion> {
    // 1. Exact version match (version-aware: "3.12.13+20260310" matches "3.12.13_20260310")
    if !exact_versions.is_empty() {
        let parsed_exact: Vec<Option<Version>> = exact_versions.iter().map(|s| Version::parse(s)).collect();
        versions.retain(|v| {
            // Try parsed comparison first (handles +/_ equivalence), fall back to raw string
            let v_parsed = Version::parse(&v.version);
            exact_versions.iter().zip(&parsed_exact).any(|(raw, parsed)| {
                if let (Some(vp), Some(ep)) = (&v_parsed, parsed) {
                    vp == ep
                } else {
                    v.version == *raw
                }
            })
        });
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
                    && parsed >= *max
                {
                    return false;
                }
                true
            });
        }
    }

    // 3. Skip versions with no resolved platform assets
    versions.retain(|v| !v.platforms.is_empty());

    // 4. Sort by version (oldest first)
    versions.sort_by(|a, b| {
        let va = Version::parse(&a.version);
        let vb = Version::parse(&b.version);
        match (va, vb) {
            (Some(a), Some(b)) => a.cmp(&b),
            _ => a.version.cmp(&b.version),
        }
    });

    // 5. Keep only the latest (highest) version BEFORE subtracting already-mirrored.
    // This ensures --latest always targets the true latest, and if it's already
    // mirrored, the result is empty (nothing to do) rather than falling back
    // to the next-highest version.
    if latest && let Some(last) = versions.last() {
        let latest_version = last.version.clone();
        versions.retain(|v| v.version == latest_version);
    }

    // 6. Subtract already-mirrored (version, platform) pairs.
    // A version is kept if at least one of its target platforms is not yet pushed.
    // This enables retry of partially-pushed versions (e.g., linux/amd64 succeeded
    // but darwin/arm64 failed on a previous run).
    //
    // For variant specs, the check uses the variant-prefixed version (e.g.,
    // "debug-3.12.5") to match registry tags. Min/max bounds (step 2) use the
    // bare source version to avoid Ord issues with variant-first sorting.
    versions.retain_mut(|v| {
        let check_tag = match &v.variant {
            Some(name) => format!("{name}-{}", v.version),
            None => v.version.clone(),
        };
        let version = Version::parse(&check_tag).expect("mirror versions must be valid");
        v.platforms.retain(|pa| !existing.has(&version, &pa.platform));
        !v.platforms.is_empty()
    });

    // 7. Apply new_per_run cap (not applicable when --latest is set)
    if !latest
        && let Some(config) = versions_config
        && let Some(cap) = config.new_per_run
    {
        match config.backfill {
            BackfillOrder::OldestFirst => {
                versions.truncate(cap);
            }
            BackfillOrder::NewestFirst => {
                let start = versions.len().saturating_sub(cap);
                versions = versions.split_off(start);
            }
        }
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
            variant: None,
            platforms: vec![ResolvedPlatformAsset {
                platform: platform("linux/amd64"),
                asset_name: "test.tar.gz".to_string(),
                url: Url::parse("https://example.com/test.tar.gz").unwrap(),
            }],
            is_prerelease: prerelease,
        }
    }

    fn rv_variant(version: &str, normalized: &str, variant: &str) -> ResolvedVersion {
        ResolvedVersion {
            version: version.to_string(),
            normalized_version: normalized.to_string(),
            variant: Some(variant.to_string()),
            platforms: vec![ResolvedPlatformAsset {
                platform: platform("linux/amd64"),
                asset_name: "test.tar.gz".to_string(),
                url: Url::parse("https://example.com/test.tar.gz").unwrap(),
            }],
            is_prerelease: false,
        }
    }

    fn rv_multi(version: &str, normalized: &str, platforms: &[&str]) -> ResolvedVersion {
        ResolvedVersion {
            version: version.to_string(),
            normalized_version: normalized.to_string(),
            variant: None,
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

        let result = filter_versions(versions, &[], true, None, &empty(), false);
        assert_eq!(result.len(), 2);
        assert!(result.iter().all(|v| !v.is_prerelease));
    }

    #[test]
    fn keep_prereleases_when_not_configured() {
        let versions = vec![rv("1.0.0", "1.0.0+ts", false), rv("1.1.0-rc1", "1.1.0-rc1+ts", true)];

        let result = filter_versions(versions, &[], false, None, &empty(), false);
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
            ..Default::default()
        };

        let result = filter_versions(versions, &[], false, Some(&config), &empty(), false);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].version, "2.0.0");
        assert_eq!(result[1].version, "3.0.0");
    }

    #[test]
    fn filter_max_bound_exclusive() {
        let versions = vec![
            rv("1.0.0", "1.0.0+ts", false),
            rv("2.0.0", "2.0.0+ts", false),
            rv("3.0.0", "3.0.0+ts", false),
        ];

        let config = VersionsConfig {
            max: Some("2.0.0".to_string()),
            ..Default::default()
        };

        // max is exclusive: 2.0.0 itself is excluded
        let result = filter_versions(versions, &[], false, Some(&config), &empty(), false);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].version, "1.0.0");
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

        let result = filter_versions(versions, &[], false, None, &existing, false);
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

        let result = filter_versions(versions, &[], false, None, &existing, false);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].version, "2.0.0");
    }

    #[test]
    fn new_per_run_cap_newest_first() {
        let versions = vec![
            rv("1.0.0", "1.0.0+ts", false),
            rv("2.0.0", "2.0.0+ts", false),
            rv("3.0.0", "3.0.0+ts", false),
        ];

        let config = VersionsConfig {
            new_per_run: Some(1),
            ..Default::default()
        };

        // Default (newest_first): picks the highest version
        let result = filter_versions(versions, &[], false, Some(&config), &empty(), false);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].version, "3.0.0");
    }

    #[test]
    fn new_per_run_cap_oldest_first() {
        let versions = vec![
            rv("1.0.0", "1.0.0+ts", false),
            rv("2.0.0", "2.0.0+ts", false),
            rv("3.0.0", "3.0.0+ts", false),
        ];

        let config = VersionsConfig {
            new_per_run: Some(1),
            backfill: BackfillOrder::OldestFirst,
            ..Default::default()
        };

        let result = filter_versions(versions, &[], false, Some(&config), &empty(), false);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].version, "1.0.0");
    }

    #[test]
    fn newest_first_with_larger_cap() {
        let versions = vec![
            rv("1.0.0", "1.0.0+ts", false),
            rv("2.0.0", "2.0.0+ts", false),
            rv("3.0.0", "3.0.0+ts", false),
            rv("4.0.0", "4.0.0+ts", false),
            rv("5.0.0", "5.0.0+ts", false),
        ];

        let config = VersionsConfig {
            new_per_run: Some(3),
            ..Default::default()
        };

        let result = filter_versions(versions, &[], false, Some(&config), &empty(), false);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].version, "3.0.0");
        assert_eq!(result[1].version, "4.0.0");
        assert_eq!(result[2].version, "5.0.0");
    }

    #[test]
    fn newest_first_successive_runs() {
        // Simulates day 1: get newest 2, day 2: get next newest 2
        let all_versions = vec![
            rv("1.0.0", "1.0.0+ts", false),
            rv("2.0.0", "2.0.0+ts", false),
            rv("3.0.0", "3.0.0+ts", false),
            rv("4.0.0", "4.0.0+ts", false),
        ];

        let config = VersionsConfig {
            new_per_run: Some(2),
            ..Default::default()
        };

        // Day 1: nothing mirrored yet → get [3.0.0, 4.0.0]
        let result = filter_versions(all_versions.clone(), &[], false, Some(&config), &empty(), false);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].version, "3.0.0");
        assert_eq!(result[1].version, "4.0.0");

        // Day 2: 3.0.0 and 4.0.0 already mirrored → get [1.0.0, 2.0.0]
        let existing = existing(&[("3.0.0", "linux/amd64"), ("4.0.0", "linux/amd64")]);
        let result = filter_versions(all_versions, &[], false, Some(&config), &existing, false);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].version, "1.0.0");
        assert_eq!(result[1].version, "2.0.0");
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

        // max is exclusive, so 3.0.0 is excluded; prerelease 1.1.0-rc1 also skipped
        let config = VersionsConfig {
            min: Some("1.0.0".to_string()),
            max: Some("3.0.0".to_string()),
            new_per_run: Some(2),
            ..Default::default()
        };

        let existing = existing(&[("1.0.0", "linux/amd64")]);

        let result = filter_versions(versions, &[], true, Some(&config), &existing, false);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].version, "2.0.0");
    }

    #[test]
    fn skip_versions_with_no_platforms() {
        let mut no_platforms = rv("1.0.0", "1.0.0+ts", false);
        no_platforms.platforms = vec![];

        let versions = vec![no_platforms, rv("2.0.0", "2.0.0+ts", false)];

        let result = filter_versions(versions, &[], false, None, &empty(), false);
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

        let result = filter_versions(versions, &[], false, None, &existing, false);
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

        let result = filter_versions(versions, &["2.0.0".to_string()], false, None, &empty(), false);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].version, "2.0.0");
    }

    #[test]
    fn exact_version_plus_underscore_equivalence() {
        // Generator produces "3.12.13+20260310" (with +), user passes --version 3.12.13_20260310 (with _)
        let versions = vec![
            rv("3.12.13+20260310", "3.12.13_20260310", false),
            rv("3.13.0+20260310", "3.13.0_20260310", false),
        ];

        let result = filter_versions(
            versions,
            &["3.12.13_20260310".to_string()],
            false,
            None,
            &empty(),
            false,
        );
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].version, "3.12.13+20260310");
    }

    #[test]
    fn exact_version_no_match() {
        let versions = vec![rv("1.0.0", "1.0.0+ts", false), rv("2.0.0", "2.0.0+ts", false)];

        let result = filter_versions(versions, &["9.9.9".to_string()], false, None, &empty(), false);
        assert!(result.is_empty());
    }

    #[test]
    fn exact_version_already_mirrored() {
        let versions = vec![rv("2.0.0", "2.0.0_20260313150000", false)];
        let existing = existing(&[("2.0.0", "linux/amd64")]);

        let result = filter_versions(versions, &["2.0.0".to_string()], false, None, &existing, false);
        assert!(result.is_empty());
    }

    #[test]
    fn multiple_exact_versions() {
        let versions = vec![
            rv("1.0.0", "1.0.0+ts", false),
            rv("2.0.0", "2.0.0+ts", false),
            rv("3.0.0", "3.0.0+ts", false),
        ];

        let result = filter_versions(
            versions,
            &["1.0.0".to_string(), "3.0.0".to_string()],
            false,
            None,
            &empty(),
            false,
        );
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].version, "1.0.0");
        assert_eq!(result[1].version, "3.0.0");
    }

    #[test]
    fn empty_input() {
        let result = filter_versions(vec![], &[], false, None, &empty(), false);
        assert!(result.is_empty());
    }

    #[test]
    fn all_filtered() {
        let versions = vec![rv("1.0.0", "1.0.0_20260313150000", false)];
        let existing = existing(&[("1.0.0", "linux/amd64")]);

        let result = filter_versions(versions, &[], false, None, &existing, false);
        assert!(result.is_empty());
    }

    #[test]
    fn partial_platform_retry() {
        // Version has 2 platforms, only 1 is already pushed
        let versions = vec![rv_multi("1.0.0", "1.0.0_ts", &["linux/amd64", "darwin/arm64"])];
        let existing = existing(&[("1.0.0", "linux/amd64")]);

        let result = filter_versions(versions, &[], false, None, &existing, false);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].platforms.len(), 1);
        assert_eq!(result[0].platforms[0].platform, platform("darwin/arm64"));
    }

    #[test]
    fn all_platforms_pushed_filters_version() {
        let versions = vec![rv_multi("1.0.0", "1.0.0_ts", &["linux/amd64", "darwin/arm64"])];
        let existing = existing(&[("1.0.0", "linux/amd64"), ("1.0.0", "darwin/arm64")]);

        let result = filter_versions(versions, &[], false, None, &existing, false);
        assert!(result.is_empty());
    }

    #[test]
    fn no_platforms_pushed_keeps_all() {
        let versions = vec![rv_multi("1.0.0", "1.0.0_ts", &["linux/amd64", "darwin/arm64"])];

        let result = filter_versions(versions, &[], false, None, &empty(), false);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].platforms.len(), 2);
    }

    #[test]
    fn latest_keeps_highest_version() {
        let versions = vec![
            rv("1.0.0", "1.0.0+ts", false),
            rv("2.0.0", "2.0.0+ts", false),
            rv("3.0.0", "3.0.0+ts", false),
        ];

        let result = filter_versions(versions, &[], false, None, &empty(), true);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].version, "3.0.0");
    }

    #[test]
    fn latest_with_empty_input() {
        let result = filter_versions(vec![], &[], false, None, &empty(), true);
        assert!(result.is_empty());
    }

    #[test]
    fn latest_skips_new_per_run_cap() {
        let versions = vec![
            rv("1.0.0", "1.0.0+ts", false),
            rv("2.0.0", "2.0.0+ts", false),
            rv("3.0.0", "3.0.0+ts", false),
            rv("4.0.0", "4.0.0+ts", false),
            rv("5.0.0", "5.0.0+ts", false),
        ];

        let config = VersionsConfig {
            new_per_run: Some(2),
            ..Default::default()
        };

        // Without --latest, new_per_run=2 keeps [1.0.0, 2.0.0]
        // With --latest, should get 5.0.0 (the true highest), not 2.0.0
        let result = filter_versions(versions, &[], false, Some(&config), &empty(), true);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].version, "5.0.0");
    }

    #[test]
    fn latest_combined_with_exact_versions() {
        let versions = vec![
            rv("1.0.0", "1.0.0+ts", false),
            rv("2.0.0", "2.0.0+ts", false),
            rv("3.0.0", "3.0.0+ts", false),
        ];

        let result = filter_versions(
            versions,
            &["1.0.0".to_string(), "2.0.0".to_string()],
            false,
            None,
            &empty(),
            true,
        );
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].version, "2.0.0");
    }

    #[test]
    fn latest_does_not_fallback_when_already_mirrored() {
        // Regression: --latest should target the true latest (3.0.0), and if it's
        // already mirrored, return empty — NOT fall back to the next-highest (2.0.0).
        let versions = vec![
            rv("1.0.0", "1.0.0+ts", false),
            rv("2.0.0", "2.0.0+ts", false),
            rv("3.0.0", "3.0.0+ts", false),
        ];

        let existing = existing(&[("3.0.0", "linux/amd64")]);

        let result = filter_versions(versions, &[], false, None, &existing, true);
        assert!(
            result.is_empty(),
            "should be empty when latest is already mirrored, got: {:?}",
            result.iter().map(|v| &v.version).collect::<Vec<_>>()
        );
    }

    #[test]
    fn latest_retries_partial_platforms() {
        // --latest should still return the latest if only some platforms are mirrored
        let versions = vec![
            rv_multi("1.0.0", "1.0.0_ts", &["linux/amd64", "darwin/arm64"]),
            rv_multi("2.0.0", "2.0.0_ts", &["linux/amd64", "darwin/arm64"]),
        ];

        let existing = existing(&[("2.0.0", "linux/amd64")]);

        let result = filter_versions(versions, &[], false, None, &existing, true);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].version, "2.0.0");
        assert_eq!(result[0].platforms.len(), 1);
        assert_eq!(result[0].platforms[0].platform, platform("darwin/arm64"));
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

    // -- variant-aware filter tests --

    #[test]
    fn variant_already_mirrored_detected() {
        // debug-1.0.0 is already on registry, should be filtered out
        let versions = vec![rv_variant("1.0.0", "debug-1.0.0_ts", "debug")];
        let existing = existing(&[("debug-1.0.0", "linux/amd64")]);

        let result = filter_versions(versions, &[], false, None, &existing, false);
        assert!(
            result.is_empty(),
            "variant version should be detected as already mirrored"
        );
    }

    #[test]
    fn variant_not_confused_with_default() {
        // Default variant 1.0.0 is mirrored, but debug-1.0.0 is not
        let versions = vec![rv_variant("1.0.0", "debug-1.0.0_ts", "debug")];
        let existing = existing(&[("1.0.0", "linux/amd64")]);

        let result = filter_versions(versions, &[], false, None, &existing, false);
        assert_eq!(result.len(), 1, "debug variant should not be confused with default");
    }

    #[test]
    fn different_variants_same_version_independent() {
        // Both debug and pgo.lto for 1.0.0, only debug is mirrored
        let versions = vec![
            rv_variant("1.0.0", "debug-1.0.0_ts", "debug"),
            rv_variant("1.0.0", "pgo.lto-1.0.0_ts", "pgo.lto"),
        ];
        let existing = existing(&[("debug-1.0.0", "linux/amd64")]);

        let result = filter_versions(versions, &[], false, None, &existing, false);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].variant.as_deref(), Some("pgo.lto"));
    }

    #[test]
    fn unnamed_default_mirrored_slim_still_uploaded() {
        // Default (unnamed) variant is fully mirrored, slim variant should still pass
        let versions = vec![
            rv("1.0.0", "1.0.0_ts", false),               // default (unnamed)
            rv_variant("1.0.0", "slim-1.0.0_ts", "slim"), // slim
        ];
        let existing = existing(&[("1.0.0", "linux/amd64")]); // only default is on registry

        let result = filter_versions(versions, &[], false, None, &existing, false);
        assert_eq!(result.len(), 1, "slim variant should still be uploaded");
        assert_eq!(result[0].variant.as_deref(), Some("slim"));
    }

    #[test]
    fn variant_min_max_uses_bare_version() {
        // Min/max bounds should apply to the bare source version, not variant-prefixed
        let config = VersionsConfig {
            min: Some("2.0.0".to_string()),
            max: Some("4.0.0".to_string()),
            ..Default::default()
        };

        let versions = vec![
            rv_variant("1.0.0", "debug-1.0.0_ts", "debug"),
            rv_variant("3.0.0", "debug-3.0.0_ts", "debug"),
            rv_variant("5.0.0", "debug-5.0.0_ts", "debug"),
        ];

        let result = filter_versions(versions, &[], false, Some(&config), &empty(), false);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].version, "3.0.0");
    }

    #[test]
    fn variant_latest_keeps_all_variants() {
        // --latest should keep ALL variants of the latest source version, not just one
        let versions = vec![
            rv_variant("3.11.0", "debug-3.11.0_ts", "debug"),
            rv_variant("3.11.0", "pgo.lto-3.11.0_ts", "pgo.lto"),
            rv_variant("3.12.5", "debug-3.12.5_ts", "debug"),
            rv_variant("3.12.5", "pgo.lto-3.12.5_ts", "pgo.lto"),
        ];

        let result = filter_versions(versions, &[], false, None, &empty(), true);
        assert_eq!(result.len(), 2, "both variants of 3.12.5 should be kept");
        assert_eq!(result[0].version, "3.12.5");
        assert_eq!(result[0].variant.as_deref(), Some("debug"));
        assert_eq!(result[1].version, "3.12.5");
        assert_eq!(result[1].variant.as_deref(), Some("pgo.lto"));
    }
}
