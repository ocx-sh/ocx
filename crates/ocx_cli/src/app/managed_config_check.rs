// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use super::{Context, background_check};

/// Throttled background-refresh probe for the corporate managed-config tier, sibling of
/// [`super::update_check::check_for_update`].
///
/// Gated by [`background_check::skip_reason`] with the `OCX_NO_CONFIG_REFRESH` kill switch.
/// Never fails the command (see
/// [`ocx_package_manager::PackageManager::check_managed_config_refresh`]). Resolves the effective
/// `[managed]` tier via [`ocx_config::managed::resolve_managed_target`] and hands off to
/// `check_managed_config_refresh` unless its `refresh` posture is
/// [`ocx_config::refresh::RefreshPolicy::Manual`].
pub async fn check_for_managed_config_refresh(ctx: &Context) {
    if let Some(reason) = background_check::skip_reason(&ocx_env::OCX_NO_CONFIG_REFRESH, ctx.is_offline()) {
        log::debug!("Managed-config refresh skipped: {reason}");
        return;
    }

    probe_managed_config_refresh(ctx).await;
}

async fn probe_managed_config_refresh(ctx: &Context) {
    use ocx_config::managed::resolve_managed_target;
    use ocx_config::refresh::RefreshPolicy;
    use ocx_package_manager::ManagedConfigRefreshOutcome;

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
