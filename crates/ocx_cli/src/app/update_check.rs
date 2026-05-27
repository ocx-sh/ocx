// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::io::IsTerminal;
use std::time::Duration;

use ocx_lib::{env, log, package_manager::UpdateCheckResult};

use super::Context;

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
    if env::flag("OCX_NO_UPDATE_CHECK", false) {
        log::debug!("Update check skipped: OCX_NO_UPDATE_CHECK is set");
        return;
    }
    if env::is_ci() {
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

    // Parse OCX_UPDATE_CHECK_INTERVAL:
    //   unset → None (lib defaults to 24h)
    //   "0"   → Some(ZERO) (always check)
    //   N     → Some(Duration::from_secs(N))
    let throttle: Option<Duration> = match env::var("OCX_UPDATE_CHECK_INTERVAL") {
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

    match ctx.manager().self_check_update(throttle).await {
        Ok(UpdateCheckResult::AlreadyUpToDate) => {
            log::debug!("Already up to date.");
        }
        Ok(UpdateCheckResult::Skipped(reason)) => {
            // Display impl provides the human-readable skip reason.
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
