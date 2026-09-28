// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::io::IsTerminal;
use std::time::Duration;

use ocx_package_manager::{TagProbe, UpdateCheckResult};

use super::Context;

/// Checks the remote registry for a newer OCX version and prints a notice to stderr.
///
/// Never fails the command: errors are logged at debug level.
pub async fn check_for_update(ctx: &Context) {
    if ocx_util::env::flag("OCX_NO_UPDATE_CHECK", false) {
        log::debug!("Update check skipped: OCX_NO_UPDATE_CHECK is set");
        return;
    }
    if ocx_util::env::is_ci() {
        log::debug!("Update check skipped: CI environment detected");
        return;
    }
    if ctx.is_offline() {
        log::debug!("Update check skipped: offline mode");
        return;
    }
    if !std::io::stderr().is_terminal() {
        log::debug!("Update check skipped: stderr is not a terminal");
        return;
    }

    // `None` (unset or malformed) takes the lib's 24h default.
    let throttle: Option<Duration> = match ocx_util::env::var("OCX_UPDATE_CHECK_INTERVAL") {
        None => None,
        Some(s) => match s.trim().parse::<u64>() {
            Ok(0) => Some(Duration::ZERO),
            Ok(n) => Some(Duration::from_secs(n)),
            Err(_) => {
                log::debug!("Update check: ignoring malformed OCX_UPDATE_CHECK_INTERVAL={s:?}");
                None
            }
        },
    };

    // A live probe regardless of ChainMode: a local read would only echo a stale local index.
    match ctx.manager().self_check_update(throttle, TagProbe::Remote).await {
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
}
