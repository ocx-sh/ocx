// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use anyhow::{Result, bail};
use chrono::Utc;
use ocx_lib::package::version::Version;

use crate::spec::BuildTimestampFormat;

/// Generate a UTC build timestamp string for the current run.
pub fn build_timestamp(format: &BuildTimestampFormat) -> Option<String> {
    let now = Utc::now();
    match format {
        BuildTimestampFormat::Datetime => Some(now.format("%Y%m%d%H%M%S").to_string()),
        BuildTimestampFormat::Date => Some(now.format("%Y%m%d").to_string()),
        BuildTimestampFormat::None => None,
    }
}

/// Normalize a version string, optionally appending a build timestamp.
///
/// Rules (when build is Some):
/// - `X` → Error (major only, too ambiguous)
/// - `X.Y` → Error (minor only, need full X.Y.Z)
/// - `X.Y.Z` → `X.Y.Z+{build}`
/// - `X.Y.Z-pre` → `X.Y.Z-pre+{build}`
/// - `X.Y.Z+build` → Error (already has build metadata)
///
/// When build is None:
/// - `X` → Error
/// - `X.Y` → Error
/// - `X.Y.Z` → `X.Y.Z`
/// - `X.Y.Z-pre` → `X.Y.Z-pre`
pub fn normalize_version(version_str: &str, build: &Option<String>) -> Result<String> {
    let version = Version::parse(version_str).ok_or_else(|| anyhow::anyhow!("cannot parse version '{version_str}'"))?;

    if version.has_build() {
        bail!("version '{version_str}' already has build metadata");
    }

    if !version.has_patch() {
        bail!("version '{version_str}' needs full X.Y.Z format");
    }

    match build {
        Some(build) => {
            let with_build = if let Some(pre) = version.prerelease() {
                Version::new_prerelease_with_build(
                    version.major(),
                    version.minor().unwrap(),
                    version.patch().unwrap(),
                    pre,
                    build,
                )
            } else {
                Version::new_build(
                    version.major(),
                    version.minor().unwrap(),
                    version.patch().unwrap(),
                    build,
                )
            };
            Ok(with_build.to_string())
        }
        None => Ok(version.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts() -> Option<String> {
        Some("20260310142359".to_string())
    }

    #[test]
    fn normalize_patch() {
        assert_eq!(normalize_version("3.28.0", &ts()).unwrap(), "3.28.0_20260310142359");
    }

    #[test]
    fn reject_minor_only() {
        assert!(normalize_version("3.28", &ts()).is_err());
    }

    #[test]
    fn normalize_prerelease() {
        assert_eq!(
            normalize_version("3.28.0-rc1", &ts()).unwrap(),
            "3.28.0-rc1_20260310142359"
        );
    }

    #[test]
    fn reject_major_only() {
        assert!(normalize_version("3", &ts()).is_err());
    }

    #[test]
    fn reject_existing_build() {
        assert!(normalize_version("3.28.0+existing", &ts()).is_err());
    }

    #[test]
    fn normalize_no_timestamp_patch() {
        assert_eq!(normalize_version("3.28.0", &None).unwrap(), "3.28.0");
    }

    #[test]
    fn reject_no_timestamp_minor() {
        assert!(normalize_version("3.28", &None).is_err());
    }

    #[test]
    fn date_format_timestamp() {
        let ts = build_timestamp(&BuildTimestampFormat::Date).unwrap();
        assert_eq!(ts.len(), 8); // YYYYMMDD
        assert!(ts.chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn datetime_format_timestamp() {
        let ts = build_timestamp(&BuildTimestampFormat::Datetime).unwrap();
        assert_eq!(ts.len(), 14); // YYYYMMDDHHmmss
        assert!(ts.chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn none_format_timestamp() {
        assert!(build_timestamp(&BuildTimestampFormat::None).is_none());
    }
}
