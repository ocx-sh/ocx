// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use anyhow::{Result, bail};
use ocx_lib::package::version::Version;

pub use ocx_lib::package::version::build_timestamp;

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
/// - `X.Y.Z` → `X.Y.Z` (pass-through, including existing build metadata)
pub fn normalize_version(version_str: &str, build: &Option<String>) -> Result<String> {
    let version = Version::parse(version_str).ok_or_else(|| anyhow::anyhow!("cannot parse version '{version_str}'"))?;

    if !version.has_patch() {
        bail!("version '{version_str}' needs full X.Y.Z format");
    }

    match build {
        Some(build) => {
            if version.has_build() {
                bail!("version '{version_str}' already has build metadata");
            }
            let with_build = if let Some(pre) = version.prerelease() {
                Version::new_prerelease_with_build(
                    version.major(),
                    version.minor().expect("has_patch guarantees minor"),
                    version.patch().expect("has_patch guarantees patch"),
                    pre,
                    build,
                )
            } else {
                Version::new_build(
                    version.major(),
                    version.minor().expect("has_patch guarantees minor"),
                    version.patch().expect("has_patch guarantees patch"),
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
    fn reject_existing_build_with_timestamp() {
        assert!(normalize_version("3.28.0+existing", &ts()).is_err());
    }

    #[test]
    fn passthrough_existing_build_without_timestamp() {
        assert_eq!(normalize_version("25.0.2_10001", &None).unwrap(), "25.0.2_10001");
    }

    #[test]
    fn normalize_no_timestamp_patch() {
        assert_eq!(normalize_version("3.28.0", &None).unwrap(), "3.28.0");
    }

    #[test]
    fn reject_no_timestamp_minor() {
        assert!(normalize_version("3.28", &None).is_err());
    }

    // `build_timestamp` and `BuildTimestampFormat` itself are tested where
    // they live, in `ocx_lib::package::version::build_meta`.
}
