// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Platform-aware version tracking for cascade correctness.
//!
//! [`VersionPlatformMap`] tracks which `(Version, Platform)` pairs are known
//! to exist on the registry. It is built from the initial registry snapshot
//! and incrementally updated as pushes succeed during a mirror run.
//!
//! This enables:
//! - **Partial-failure retry**: a version missing one platform is re-queued
//!   for just that platform on the next run.
//! - **Clean cascade input**: [`versions_for_cascade`](VersionPlatformMap::versions_for_cascade)
//!   excludes rolling tags that would cause unnecessary blocker manifest fetches.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use ocx_lib::oci::Platform;
use ocx_lib::package::version::Version;

/// Tracks which `(Version, Platform)` pairs exist on the registry.
///
/// Built from the initial tag list + selective manifest fetches, then
/// updated incrementally as pushes succeed. Failed pushes are simply
/// not recorded — the next run will retry them.
#[derive(Default)]
pub struct VersionPlatformMap {
    entries: BTreeMap<Version, HashSet<Platform>>,
}

impl VersionPlatformMap {
    /// Build from a list of tags and a set of (version, platforms) pairs
    /// obtained by fetching manifests for tags that match our source list.
    ///
    /// Tags without platform info are recorded as version-only (they
    /// participate in cascade computation but `has()` returns false for
    /// all platforms).
    pub fn from_tags_and_platforms(all_tags: &[String], platform_info: BTreeMap<Version, HashSet<Platform>>) -> Self {
        let mut entries: BTreeMap<Version, HashSet<Platform>> = BTreeMap::new();

        // Register all parseable tags (version-only, no platform info).
        for tag in all_tags {
            if let Some(v) = Version::parse(tag) {
                entries.entry(v).or_default();
            }
        }

        // Merge in the platform info we fetched for matching versions.
        for (version, platforms) in platform_info {
            entries.entry(version).or_default().extend(platforms);
        }

        Self { entries }
    }

    /// Record a successful `(version, platform)` push.
    pub fn add(&mut self, version: Version, platform: Platform) {
        self.entries.entry(version).or_default().insert(platform);
    }

    /// Check whether a specific `(version, platform)` pair is known to exist.
    pub fn has(&self, version: &Version, platform: &Platform) -> bool {
        self.entries.get(version).is_some_and(|ps| ps.contains(platform))
    }

    /// Versions suitable for cascade blocker computation.
    ///
    /// Excludes rolling tags (major-only like `3`, minor-only like `3.28`)
    /// because build-tagged versions in the same series already provide
    /// correct blocking semantics. Keeping rolling tags would only cause
    /// redundant `has_blocking_platform` manifest fetches.
    pub fn versions_for_cascade(&self) -> BTreeSet<Version> {
        self.entries.keys().filter(|v| v.has_build()).cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn platform(s: &str) -> Platform {
        s.parse().unwrap()
    }

    fn version(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn add_and_has() {
        let mut map = VersionPlatformMap::default();
        let v = version("3.28.1_b1");
        let p = platform("linux/amd64");

        assert!(!map.has(&v, &p));
        map.add(v.clone(), p.clone());
        assert!(map.has(&v, &p));
        assert!(!map.has(&v, &platform("darwin/arm64")));
    }

    #[test]
    fn from_tags_and_platforms_registers_all_tags() {
        let tags = vec![
            "3.28.1_b1".into(),
            "3.28.1".into(),
            "3.28".into(),
            "3".into(),
            "latest".into(),
        ];
        let platform_info = BTreeMap::new();

        let map = VersionPlatformMap::from_tags_and_platforms(&tags, platform_info);

        // All parseable tags are registered (latest doesn't parse)
        assert!(map.entries.contains_key(&version("3.28.1_b1")));
        assert!(map.entries.contains_key(&version("3.28.1")));
        assert!(map.entries.contains_key(&version("3.28")));
        assert!(map.entries.contains_key(&version("3")));
        // But none have platform info
        assert!(!map.has(&version("3.28.1_b1"), &platform("linux/amd64")));
    }

    #[test]
    fn from_tags_and_platforms_merges_platform_info() {
        let tags = vec!["3.28.1_b1".into(), "3.28".into()];
        let mut platform_info = BTreeMap::new();
        platform_info.insert(version("3.28.1_b1"), HashSet::from([platform("linux/amd64")]));

        let map = VersionPlatformMap::from_tags_and_platforms(&tags, platform_info);

        assert!(map.has(&version("3.28.1_b1"), &platform("linux/amd64")));
        assert!(!map.has(&version("3.28"), &platform("linux/amd64")));
    }

    #[test]
    fn versions_for_cascade_excludes_rolling() {
        let mut map = VersionPlatformMap::default();
        map.add(version("3.28.1_b1"), platform("linux/amd64"));
        map.add(version("3.28.1"), platform("linux/amd64"));
        map.add(version("3.28"), platform("linux/amd64"));
        map.add(version("3"), platform("linux/amd64"));

        let cascade = map.versions_for_cascade();
        assert!(cascade.contains(&version("3.28.1_b1")));
        assert!(!cascade.contains(&version("3.28.1")));
        assert!(!cascade.contains(&version("3.28")));
        assert!(!cascade.contains(&version("3")));
    }

    #[test]
    fn multiple_platforms_per_version() {
        let mut map = VersionPlatformMap::default();
        let v = version("3.28.1_b1");
        map.add(v.clone(), platform("linux/amd64"));
        map.add(v.clone(), platform("darwin/arm64"));

        assert!(map.has(&v, &platform("linux/amd64")));
        assert!(map.has(&v, &platform("darwin/arm64")));
        assert!(!map.has(&v, &platform("windows/amd64")));
    }
}
