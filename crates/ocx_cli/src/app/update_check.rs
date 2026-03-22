// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::io::IsTerminal;

use ocx_lib::{Error, env, log, oci, package::version::Version};

use super::Context;

enum UpdateCheckResult {
    AlreadyUpToDate,
    Skipped(&'static str),
    UpdateAvailable(oci::Identifier),
}

/// Checks the remote registry for a newer OCX version and prints a notice to stderr.
///
/// Always queries the **remote** index, regardless of the user's `--remote`/default index
/// selection. Never fails the command — all errors are swallowed and logged at debug level.
///
/// Suppressed when:
/// - `OCX_NO_UPDATE_CHECK` is truthy
/// - `CI` is truthy (see [`env::is_ci`])
/// - `OCX_OFFLINE` is truthy (or `--offline` flag)
/// - stderr is not a terminal
pub async fn check_for_update(ctx: &Context) {
    match try_check_for_update(ctx).await {
        Ok(UpdateCheckResult::AlreadyUpToDate) => {
            log::debug!("Already up to date.");
        }
        Ok(UpdateCheckResult::Skipped(reason)) => {
            log::debug!("Update check skipped: {reason}");
        }
        Ok(UpdateCheckResult::UpdateAvailable(identifier)) => {
            eprintln!(
                "A new OCX version is available: {identifier}. Consider updating by running `ocx --remote install --select ocx`."
            );
        }
        Err(err) => {
            log::error!("Update check failed: {err}");
        }
    }
}

async fn try_check_for_update(ctx: &Context) -> Result<UpdateCheckResult, Error> {
    if env::flag("OCX_NO_UPDATE_CHECK", false) {
        return Ok(UpdateCheckResult::Skipped("OCX_NO_UPDATE_CHECK is set"));
    }
    if env::is_ci() {
        return Ok(UpdateCheckResult::Skipped("CI environment detected"));
    }
    if ctx.is_offline() {
        return Ok(UpdateCheckResult::Skipped("offline mode"));
    }
    if !std::io::stderr().is_terminal() {
        return Ok(UpdateCheckResult::Skipped("stderr is not a terminal"));
    }

    let current_version_str = super::version();
    let current_version = Version::parse(current_version_str)
        .ok_or_else(|| ocx_lib::package::error::Error::VersionInvalid(current_version_str.into()))?;
    let ocx_identifier = oci::Identifier::new_registry("ocx", oci::OCX_SH_REGISTRY);

    let remote_index = ctx.remote_index()?;
    let remote_index = oci::index::Index::from_remote(remote_index.clone());

    let ocx_tags = remote_index.list_tags(&ocx_identifier).await?;
    let Some(ocx_tags) = ocx_tags else {
        return Ok(UpdateCheckResult::Skipped("ocx package not found in registry"));
    };

    let Some(latest_version) = find_latest_version(&ocx_tags) else {
        return Ok(UpdateCheckResult::Skipped("no release version available"));
    };
    if latest_version > current_version {
        Ok(UpdateCheckResult::UpdateAvailable(
            ocx_identifier.clone_with_tag(latest_version.to_string()),
        ))
    } else {
        Ok(UpdateCheckResult::AlreadyUpToDate)
    }
}

/// Finds the highest `major.minor.patch` version among tags.
///
/// Keeps only versions with exactly `major.minor.patch` — filters out
/// rolling tags (`1`, `1.2`), build-tagged versions (`1.2.3+build`),
/// and pre-releases (`1.2.3-rc1`) so that the message recommends installing
/// a clean release version.
fn find_latest_version(tags: &[String]) -> Option<Version> {
    tags.iter()
        .filter_map(|tag| Version::parse(tag))
        .filter(|version| version.has_patch() && !version.has_build() && !version.has_prerelease())
        .max()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_latest_core_version_picks_highest() {
        let tags = vec![
            "1.0.0".to_string(),
            "2.1.0".to_string(),
            "1.5.3".to_string(),
            "latest".to_string(),
            "2.0.9".to_string(),
        ];
        assert_eq!(find_latest_version(&tags), Some(Version::new_patch(2, 1, 0)));
    }

    #[test]
    fn find_latest_core_version_empty() {
        let tags: Vec<String> = vec![];
        assert_eq!(find_latest_version(&tags), None);
    }

    #[test]
    fn find_latest_core_version_no_valid_tags() {
        let tags = vec!["latest".to_string(), "nightly".to_string(), "edge".to_string()];
        assert_eq!(find_latest_version(&tags), None);
    }

    #[test]
    fn find_latest_version_skips_rolling_and_build_tags() {
        let tags = vec![
            "2".to_string(),              // rolling major — skipped
            "2.1".to_string(),            // rolling minor — skipped
            "2.1.0+20260101".to_string(), // build-tagged — skipped
            "2.1.0-rc1".to_string(),      // pre-release — skipped
            "2.1.0".to_string(),          // patch release — selected
            "1.5.3".to_string(),
        ];
        assert_eq!(find_latest_version(&tags), Some(Version::new_patch(2, 1, 0)));
    }

    #[test]
    fn find_latest_version_none_when_only_rolling() {
        let tags = vec!["1".to_string(), "2".to_string(), "2.1".to_string()];
        assert_eq!(find_latest_version(&tags), None);
    }
}
