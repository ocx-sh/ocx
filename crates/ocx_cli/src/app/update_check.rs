// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_config::env::keys;
use ocx_config::refresh::RefreshPolicy;
use ocx_oci::PackageRef;
use ocx_package_manager::{PackageManager, TagProbe, UpdateCheckResult};

use super::{Context, background_check};

/// Checks the remote registry for a newer OCX version and prints a notice to stderr.
///
/// Returns the release to install after the command when `[update] self = "apply"`; always
/// `None` for now, so `apply` behaves as `notify`. Never fails the command: errors are logged
/// at debug level.
pub async fn check_for_update(ctx: &Context) -> Option<PackageRef> {
    if let Some(reason) = background_check::skip_reason(keys::OCX_NO_UPDATE_CHECK, ctx.is_offline()) {
        log::debug!("Update check skipped: {reason}");
        return None;
    }
    let policy = ctx.update_policy();
    // Before the probe, so `manual` never touches the throttle state file either.
    if policy.self_policy == RefreshPolicy::Manual {
        log::debug!("Update check skipped: [update] self = \"manual\"");
        return None;
    }

    // A live probe regardless of ChainMode: a local read would only echo a stale local index.
    match ctx
        .manager()
        .self_check_update(Some(policy.interval), TagProbe::Remote)
        .await
    {
        Ok(UpdateCheckResult::AlreadyUpToDate) => {
            log::debug!("Already up to date.");
        }
        Ok(UpdateCheckResult::Skipped(reason)) => {
            log::debug!("Update check skipped: {reason}");
        }
        Ok(UpdateCheckResult::UpdateAvailable(identifier)) => {
            eprintln!("A new OCX version is available: {identifier}. Consider updating by running `ocx self update`.");
        }
        Err(err) => {
            log::debug!("Update check failed: {err}");
        }
    }
    None
}

/// Installs `identifier` after the user's command has finished; never changes its outcome.
///
/// Not implemented yet: returns without installing.
pub async fn apply_pending(_manager: &PackageManager, _identifier: &PackageRef) {}
