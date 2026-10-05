// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The tag `ocx upgrade` moves a binding to: the newest published release at the binding's own
//! variant and precision, within its major unless crossing is allowed.

use ocx_oci::PackageRef;

use crate::version::Version;

/// Why `ocx upgrade` leaves a binding's tag where it is; a report row, never an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// The binding names a digest, so there is no tag to move.
    DigestPinned,
    /// The binding tracks `latest`, which already follows every release.
    Latest,
    /// The tag is not a version (`nightly`, a bare variant name).
    NotAVersion,
    /// The tag carries a prerelease or build suffix, which pins one exact artifact.
    PrereleaseOrBuild,
    /// No newer release exists within the policy.
    UpToDate,
}

impl std::fmt::Display for SkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SkipReason::DigestPinned => "digest_pinned",
            SkipReason::Latest => "latest",
            SkipReason::NotAVersion => "not_a_version",
            SkipReason::PrereleaseOrBuild => "prerelease_or_build",
            SkipReason::UpToDate => "up_to_date",
        })
    }
}

/// The version `binding`'s tag tracks, or why `ocx upgrade` skips it. Never [`SkipReason::UpToDate`]:
/// that needs the published tags, see [`upgrade_target`].
pub fn tracked_version(binding: &PackageRef) -> Result<Version, SkipReason> {
    if binding.digest().is_some() {
        return Err(SkipReason::DigestPinned);
    }
    let tag = binding.tag_or_latest();
    if tag == "latest" {
        return Err(SkipReason::Latest);
    }
    let version = Version::parse(tag).ok_or(SkipReason::NotAVersion)?;
    if version.has_prerelease() || version.has_build() {
        return Err(SkipReason::PrereleaseOrBuild);
    }
    Ok(version)
}

/// The newest of `tags` strictly greater than `current` with its variant and precision (major,
/// minor or patch), no prerelease or build suffix, and within `current`'s major unless
/// `allow_major`; `None` when `current` is up to date.
pub fn upgrade_target(current: &Version, tags: &[String], allow_major: bool) -> Option<Version> {
    newest_on_track(current, tags, |candidate| {
        allow_major || candidate.major() == current.major()
    })
}

/// [`upgrade_target`]'s filters over the majors above `current`'s: what `--major` would reach.
pub fn newest_beyond_major(current: &Version, tags: &[String]) -> Option<Version> {
    newest_on_track(current, tags, |candidate| candidate.major() > current.major())
}

fn newest_on_track(current: &Version, tags: &[String], admit: impl Fn(&Version) -> bool) -> Option<Version> {
    tags.iter()
        .filter_map(|tag| Version::parse(tag))
        .filter(|candidate| {
            candidate.variant() == current.variant()
                // Precision is kept: without it `3.28` would move to `3.29.1` and stop rolling.
                && candidate.has_minor() == current.has_minor()
                && candidate.has_patch() == current.has_patch()
                && !candidate.has_prerelease()
                && !candidate.has_build()
                && candidate > current
                && admit(candidate)
        })
        .max()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    fn version(value: &str) -> Version {
        Version::parse(value).expect("version parses")
    }

    fn binding(value: &str) -> PackageRef {
        PackageRef::parse(value).expect("identifier parses")
    }

    // ── upgrade_target ────────────────────────────────────────────────

    #[test]
    fn minor_binding_moves_to_newest_minor_in_major() {
        let tags = tags(&[
            "3", "3.28", "3.28.4", "3.29", "3.29.1", "3.30", "3.30.0", "4", "4.0", "4.0.0",
        ]);
        assert_eq!(upgrade_target(&version("3.28"), &tags, false), Some(version("3.30")));
    }

    #[test]
    fn major_binding_has_nothing_in_major() {
        let tags = tags(&["3", "3.29", "3.29.1", "4", "4.0.0"]);
        assert_eq!(upgrade_target(&version("3"), &tags, false), None);
    }

    #[test]
    fn patch_binding_keeps_patch_precision() {
        let tags = tags(&["3.28.4", "3.28.5", "3.29", "3.29.0", "4.0.0"]);
        assert_eq!(
            upgrade_target(&version("3.28.4"), &tags, false),
            Some(version("3.29.0"))
        );
    }

    #[test]
    fn allow_major_crosses_to_newest_major() {
        let tags = tags(&["3", "4", "5", "5.1", "6.0.0"]);
        assert_eq!(upgrade_target(&version("3"), &tags, true), Some(version("5")));
        assert_eq!(upgrade_target(&version("3"), &tags, false), None);
    }

    #[test]
    fn variant_is_kept() {
        let tags = tags(&["3.29", "debug-3.28", "debug-3.29", "pgo-3.30", "debug-4.0"]);
        assert_eq!(
            upgrade_target(&version("debug-3.28"), &tags, false),
            Some(version("debug-3.29"))
        );
        assert_eq!(upgrade_target(&version("3.28"), &tags, false), Some(version("3.29")));
    }

    #[test]
    fn prerelease_build_and_non_version_tags_are_ignored() {
        let tags = tags(&[
            "3.28.4",
            "3.28.5-rc1",
            "3.28.5_b1",
            "3.29.0-rc1_b2",
            "latest",
            "nightly",
            "debug",
        ]);
        assert_eq!(upgrade_target(&version("3.28.4"), &tags, false), None);
    }

    #[test]
    fn older_and_equal_tags_are_not_targets() {
        let tags = tags(&["3.27", "3.28", "2.99"]);
        assert_eq!(upgrade_target(&version("3.28"), &tags, false), None);
        assert_eq!(upgrade_target(&version("3.28"), &tags, true), None);
    }

    // ── newest_beyond_major ───────────────────────────────────────────

    #[test]
    fn beyond_major_lists_newest_higher_major_at_same_precision() {
        let tags = tags(&["3", "3.29", "4", "4.1", "5", "5.0.0", "6.0.0-rc1", "debug-7"]);
        assert_eq!(newest_beyond_major(&version("3"), &tags), Some(version("5")));
        assert_eq!(newest_beyond_major(&version("3.28"), &tags), Some(version("4.1")));
    }

    #[test]
    fn beyond_major_is_none_without_a_higher_major() {
        let tags = tags(&["3", "3.29", "3.30"]);
        assert_eq!(newest_beyond_major(&version("3.28"), &tags), None);
    }

    // ── tracked_version ───────────────────────────────────────────────

    #[test]
    fn plain_version_tags_are_tracked() {
        assert_eq!(tracked_version(&binding("ocx.sh/cmake:3.28")), Ok(version("3.28")));
        assert_eq!(tracked_version(&binding("ocx.sh/cmake:3")), Ok(version("3")));
        assert_eq!(
            tracked_version(&binding("ocx.sh/cmake:debug-3.28")),
            Ok(version("debug-3.28"))
        );
    }

    #[test]
    fn skip_reasons() {
        let digest = format!("sha256:{}", "a".repeat(64));
        assert_eq!(
            tracked_version(&binding(&format!("ocx.sh/cmake:3.28@{digest}"))),
            Err(SkipReason::DigestPinned)
        );
        assert_eq!(
            tracked_version(&binding(&format!("ocx.sh/cmake@{digest}"))),
            Err(SkipReason::DigestPinned)
        );
        assert_eq!(
            tracked_version(&binding("ocx.sh/cmake:latest")),
            Err(SkipReason::Latest)
        );
        assert_eq!(tracked_version(&binding("ocx.sh/cmake")), Err(SkipReason::Latest));
        assert_eq!(
            tracked_version(&binding("ocx.sh/cmake:nightly")),
            Err(SkipReason::NotAVersion)
        );
        assert_eq!(
            tracked_version(&binding("ocx.sh/cmake:3.28.5-rc1")),
            Err(SkipReason::PrereleaseOrBuild)
        );
        assert_eq!(
            tracked_version(&binding("ocx.sh/cmake:3.28.4_b1")),
            Err(SkipReason::PrereleaseOrBuild)
        );
    }

    #[test]
    fn skip_reason_wire_names_are_snake_case() {
        for reason in [
            SkipReason::DigestPinned,
            SkipReason::Latest,
            SkipReason::NotAVersion,
            SkipReason::PrereleaseOrBuild,
            SkipReason::UpToDate,
        ] {
            assert_eq!(
                serde_json::to_value(reason).unwrap(),
                serde_json::Value::String(reason.to_string())
            );
        }
    }
}
