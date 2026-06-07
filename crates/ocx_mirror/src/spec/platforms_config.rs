// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Per-platform runner configuration for the test pipeline.
//!
//! [`PlatformConfig`] maps an OCI platform key (e.g. `linux/amd64`) to a
//! GitHub Actions runner label and optional container matrix. Absence of
//! `containers` means native mode; presence means container mode.

use ocx_lib::package::version::Version;
use serde::Deserialize;

use super::tests_config::TestEntry;

/// Configuration for a single container image to test against.
///
/// In container mode the OCX binary is injected via a per-leg ephemeral
/// Dockerfile `ADD` before each test leg runs.
#[derive(Debug, Clone, Deserialize)]
pub struct ContainerConfig {
    /// OCI image reference (e.g. `ubuntu:24.04`, `alpine:3.20`).
    pub image: String,
    /// Shell to invoke inside the container. Defaults by image prefix per A9:
    /// alpine → `sh`; ubuntu/debian/fedora/rocky/opensuse → `bash`; otherwise required.
    pub shell: Option<String>,
    /// Optional stable ID used to construct JUNIT filenames and GHA matrix
    /// check names. Defaults to slugified `image` (`:` and `/` → `_`).
    pub id: Option<String>,
}

/// Configuration for one platform target in the test pipeline.
///
/// A platform without `containers` runs tests natively on the declared GHA
/// runner. A platform with `containers` runs each test in each listed
/// container image (container mode, linux only).
#[derive(Debug, Clone, Deserialize)]
pub struct PlatformConfig {
    /// GitHub Actions runner label (e.g. `ubuntu-latest`, `macos-latest`).
    pub runner: String,
    /// Container images to test against. Absence = native mode.
    #[serde(default)]
    pub containers: Option<Vec<ContainerConfig>>,
    /// Command prefix inserted before every test invocation (e.g.
    /// `["arch", "-x86_64"]` for `darwin/amd64` cross-execution).
    /// Defaults per A8: `darwin/amd64` on `macos-*` → `["arch", "-x86_64"]`; else empty.
    ///
    /// Declared in the spec schema; not yet wired into CI generation.
    #[allow(dead_code)]
    #[serde(default)]
    pub prefix: Option<Vec<String>>,
    /// Default shell for native legs (e.g. `bash`, `pwsh`).
    #[serde(default)]
    pub shell: Option<String>,
    /// Per-platform test override. When set, replaces the top-level `tests:`
    /// list entirely for this platform — no partial merge.
    #[serde(default)]
    pub tests: Option<Vec<TestEntry>>,

    /// Inclusive lower bound: the first upstream version this platform applies
    /// to (e.g. a platform introduced late upstream). Versions below it are
    /// never resolved, scheduled, built, tested, or pushed for this platform.
    #[serde(default)]
    pub min_version: Option<String>,

    /// Exclusive upper bound: the first upstream version this platform no longer
    /// applies to (e.g. a platform dropped upstream). Versions at or above it
    /// are never resolved, scheduled, built, tested, or pushed for this platform.
    #[serde(default)]
    pub max_version: Option<String>,

    /// Explicit `(version[, platform])` holes within the `min_version` /
    /// `max_version` window — e.g. a single release whose build is known broken
    /// on this platform. Each entry is a single `version` **or** a
    /// `min_version`/`max_version` range. See [`ExcludeEntry`].
    #[serde(default)]
    pub exclude: Vec<ExcludeEntry>,
}

/// Surface treatment for an [`ExcludeEntry`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Drop the `(version, platform)` and surface it as a 🔒 row (with the
    /// `reason`, if given) in the Discord notification. The default — broken
    /// holes are visible so a maintainer knows the gap is deliberate.
    #[default]
    Broken,
    /// Drop the `(version, platform)` silently — no 🔒 row. Use for windows the
    /// platform simply does not apply to (introduced late / dropped early) when
    /// a `min_version`/`max_version` bound is awkward to express.
    Skip,
}

/// One entry in a platform's `exclude:` list.
///
/// Either a single `version` **or** a `min_version`/`max_version` range (a
/// range may set either bound alone, open-ended like the parent platform).
/// `version` is mutually exclusive with the range bounds; validation rejects
/// entries that set both or neither.
#[derive(Debug, Clone, Deserialize)]
pub struct ExcludeEntry {
    /// Exclude exactly this version.
    #[serde(default)]
    pub version: Option<String>,
    /// Inclusive lower bound of an excluded range.
    #[serde(default)]
    pub min_version: Option<String>,
    /// Exclusive upper bound of an excluded range.
    #[serde(default)]
    pub max_version: Option<String>,
    /// Human-readable reason, surfaced in the 🔒 row for `broken` excludes.
    #[serde(default)]
    pub reason: Option<String>,
    /// How the dropped pair is surfaced. Defaults to [`Severity::Broken`].
    #[serde(default)]
    pub severity: Severity,
}

impl ExcludeEntry {
    /// Returns `true` when `version` falls inside this exclude entry.
    ///
    /// A single-version entry matches by equality; a range matches `min`
    /// inclusive / `max` exclusive (the same convention as `versions:` and
    /// per-platform `min_version`/`max_version`). An entry with neither a
    /// `version` nor any bound matches nothing — validation rejects that shape,
    /// so this is a defensive fallback only.
    pub fn matches(&self, version: &Version) -> bool {
        if let Some(raw) = &self.version {
            return Version::parse(raw).is_some_and(|v| v == *version);
        }

        let min = self.min_version.as_ref().and_then(|s| Version::parse(s));
        let max = self.max_version.as_ref().and_then(|s| Version::parse(s));
        if min.is_none() && max.is_none() {
            return false;
        }
        if let Some(min) = min
            && *version < min
        {
            return false;
        }
        if let Some(max) = max
            && *version >= max
        {
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).expect("test version must parse")
    }

    fn entry(yaml: &str) -> ExcludeEntry {
        serde_yaml_ng::from_str(yaml).expect("exclude entry must parse")
    }

    #[test]
    fn single_version_matches_by_equality() {
        let e = entry("version: \"0.16.0\"");
        assert!(e.matches(&v("0.16.0")));
        assert!(!e.matches(&v("0.16.1")));
        assert!(!e.matches(&v("0.15.0")));
    }

    #[test]
    fn range_is_min_inclusive_max_exclusive() {
        let e = entry("min_version: \"9.0.0\"\nmax_version: \"11.1.0\"");
        assert!(!e.matches(&v("8.9.9")));
        assert!(e.matches(&v("9.0.0")), "min is inclusive");
        assert!(e.matches(&v("10.5.0")));
        assert!(!e.matches(&v("11.1.0")), "max is exclusive");
        assert!(!e.matches(&v("11.2.0")));
    }

    #[test]
    fn open_ended_max_only_drops_everything_below() {
        let e = entry("max_version: \"9.4.0\"");
        assert!(e.matches(&v("9.3.9")));
        assert!(!e.matches(&v("9.4.0")));
        assert!(!e.matches(&v("12.0.0")));
    }

    #[test]
    fn open_ended_min_only_drops_everything_at_or_above() {
        let e = entry("min_version: \"2.0.0\"");
        assert!(!e.matches(&v("1.9.9")));
        assert!(e.matches(&v("2.0.0")));
        assert!(e.matches(&v("3.1.0")));
    }

    #[test]
    fn severity_defaults_to_broken() {
        let e = entry("version: \"1.0.0\"");
        assert_eq!(e.severity, Severity::Broken);
    }

    #[test]
    fn severity_skip_parses() {
        let e = entry("version: \"1.0.0\"\nseverity: skip");
        assert_eq!(e.severity, Severity::Skip);
    }

    #[test]
    fn reason_is_optional_and_captured() {
        let e = entry("version: \"1.0.0\"\nreason: \"segfaults\"");
        assert_eq!(e.reason.as_deref(), Some("segfaults"));
        let no_reason = entry("version: \"1.0.0\"");
        assert_eq!(no_reason.reason, None);
    }

    #[test]
    fn empty_entry_matches_nothing() {
        // Defensive: an entry with neither version nor bounds matches no
        // version (validation rejects this shape upstream).
        let e = ExcludeEntry {
            version: None,
            min_version: None,
            max_version: None,
            reason: None,
            severity: Severity::Broken,
        };
        assert!(!e.matches(&v("1.0.0")));
    }

    #[test]
    fn build_metadata_breaks_equality_match() {
        // A build-stamped version is NOT equal to the bare exclude version —
        // callers (push visibility) strip build metadata before matching.
        let e = entry("version: \"0.16.0\"");
        assert!(!e.matches(&v("0.16.0_20260604")));
        assert!(e.matches(&v("0.16.0")));
    }
}
