// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::io::IsTerminal;

use ocx_lib::{env, log};

use super::Context;

/// Throttled background-refresh probe for the corporate managed-config tier
/// (ADR Decision F), sibling of [`super::update_check::check_for_update`].
///
/// Gate skeleton mirrors the update-check hook exactly: kill switch, CI
/// detection, offline, then stderr-TTY. Suppressed when:
/// - `OCX_NO_CONFIG_REFRESH` is truthy
/// - `CI` is truthy (see [`env::is_ci`])
/// - `OCX_OFFLINE` is truthy (or `--offline` flag)
/// - stderr is not a terminal
///
/// Never fails the command — see
/// [`ocx_lib::package_manager::PackageManager::check_managed_config_refresh`].
///
/// Resolves the effective `[managed]` tier via [`ocx_lib::resolve_managed_target`]
/// and, unless the tier's `refresh` posture is [`ocx_lib::RefreshPolicy::Manual`],
/// hands off to `check_managed_config_refresh`.
pub async fn check_for_managed_config_refresh(ctx: &Context) {
    if env::flag(env::keys::OCX_NO_CONFIG_REFRESH, false) {
        log::debug!("Managed-config refresh skipped: OCX_NO_CONFIG_REFRESH is set");
        return;
    }
    if env::is_ci() {
        log::debug!("Managed-config refresh skipped: CI environment detected");
        return;
    }
    if ctx.is_offline() {
        log::debug!("Managed-config refresh skipped: offline mode");
        return;
    }
    if !std::io::stderr().is_terminal() {
        log::debug!("Managed-config refresh skipped: stderr is not a terminal");
        return;
    }

    probe_managed_config_refresh(ctx).await;
}

async fn probe_managed_config_refresh(ctx: &Context) {
    use ocx_lib::package_manager::ManagedConfigRefreshOutcome;
    use ocx_lib::{RefreshPolicy, resolve_managed_target};

    let resolved = match resolve_managed_target(ctx.config(), ctx.managed_config_env_override()) {
        Ok(Some(resolved)) => resolved,
        Ok(None) => return,
        Err(source) => {
            log::debug!("managed-config refresh: target resolution failed: {source}");
            return;
        }
    };

    // `manual` posture skips the background tick entirely — only `ocx config
    // update` refreshes the snapshot.
    if resolved.refresh == RefreshPolicy::Manual {
        return;
    }

    match ctx.manager().check_managed_config_refresh(&resolved).await {
        ManagedConfigRefreshOutcome::UpToDate => {
            log::debug!("managed-config refresh: up to date");
        }
        ManagedConfigRefreshOutcome::DriftDetected => {
            eprintln!(
                "The managed configuration for {} has changed. Run `ocx config update` to sync it.",
                resolved.source
            );
        }
        ManagedConfigRefreshOutcome::Applied { digest } => {
            log::debug!("managed-config refresh: applied new snapshot {digest}");
        }
        ManagedConfigRefreshOutcome::Unreachable => {
            log::debug!("managed-config refresh: registry unreachable");
        }
        ManagedConfigRefreshOutcome::Paused => {
            log::debug!("managed-config refresh: paused via `ocx config update --pause`");
        }
    }
}
