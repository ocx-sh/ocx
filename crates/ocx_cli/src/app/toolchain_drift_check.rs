// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;
use std::time::Duration;

use ocx_config::env::keys;
use ocx_config::refresh::RefreshPolicy;
use ocx_config::shell::effective_consent;
use ocx_package_manager::DRIFT_DEADLINE;
use ocx_package_manager::activation::{self, ProjectIdentity};
use ocx_project::consent::Decision;
use ocx_project::{Binding, BoundTool, ProjectConfig, ProjectLock};
use ocx_store::file_structure::StateStore;

use super::update_notice::NoticeRow;
use super::{Context, background_check, project_context};

/// Background notice that a locked tool's tag has moved since `ocx.lock` was written.
///
/// Gated like the self check, plus `[update] toolchain = "manual"` and `--frozen`. Probes the
/// consented project toolchain and the global one, each throttled under its own marker, both
/// within one [`DRIFT_DEADLINE`]. Returns one notice row per drifted toolchain, project first.
/// Never fails the command: every failure logs at debug and yields no row.
pub(crate) async fn check_for_toolchain_drift(ctx: &Context) -> Vec<NoticeRow> {
    let mut rows = Vec::new();
    if let Some(reason) = background_check::skip_reason(keys::OCX_NO_UPDATE_CHECK, ctx.is_offline()) {
        log::debug!("Toolchain drift check skipped: {reason}");
        return rows;
    }
    let policy = ctx.update_policy();
    // Before any read, so `manual` never touches the throttle state either.
    if policy.toolchain == RefreshPolicy::Manual {
        log::debug!("Toolchain drift check skipped: [update] toolchain = \"manual\"");
        return rows;
    }
    // A frozen run forbids discovering a new tag mapping.
    if ctx.config_view().frozen {
        log::debug!("Toolchain drift check skipped: --frozen");
        return rows;
    }

    // One instant for both probes, so two toolchains cannot stack two deadlines.
    let deadline = tokio::time::Instant::now() + DRIFT_DEADLINE;
    // An absent global manifest fails to canonicalize: no global toolchain.
    let global = ProjectIdentity::resolve(ProjectConfig::global_manifest_path(ctx.file_structure().root()))
        .await
        .ok();
    // `--global`, or a walk that lands on the home manifest, names the global toolchain: probe it once.
    let project = if ctx.global() {
        None
    } else {
        project_identity(ctx)
            .await
            .filter(|project| global.as_ref().is_none_or(|global| global.dir != project.dir))
    };
    if let Some(project) = project {
        rows.extend(check_one(ctx, &project, false, policy.interval, deadline).await);
    }
    if let Some(global) = global {
        rows.extend(check_one(ctx, &global, true, policy.interval, deadline).await);
    }
    rows
}

async fn project_identity(ctx: &Context) -> Option<ProjectIdentity> {
    let (config_path, _) = project_context::resolve_project_paths(ctx, None)
        .await
        .inspect_err(|error| log::debug!("Toolchain drift check: no project: {error}"))
        .ok()?;
    ProjectIdentity::resolve(config_path)
        .await
        .inspect_err(|error| log::debug!("Toolchain drift check: {error}"))
        .ok()
}

/// Throttle, load and probe one toolchain file; its notice row, if anything moved.
async fn check_one(
    ctx: &Context,
    toolchain: &ProjectIdentity,
    global: bool,
    interval: Duration,
    deadline: tokio::time::Instant,
) -> Option<NoticeRow> {
    let state = &ctx.file_structure().state;
    let marker = if global {
        state.global_toolchain_drift_marker()
    } else {
        state.toolchain_drift_marker(&toolchain.dir)
    };
    if throttled(marker.clone(), interval).await {
        log::debug!(
            "Toolchain drift check throttled for {}",
            toolchain.config_path.display()
        );
        return None;
    }
    let bound = bound_tools(ctx, toolchain, global).await?;
    // Touched before the probe, so a failed or abandoned probe is not retried within the interval.
    StateStore::touch(marker).await;
    let drift = ctx.manager().toolchain_drift(&bound, deadline).await;
    let names: Vec<&str> = drift.iter().map(|tool| tool.name.as_str()).collect();
    if global {
        NoticeRow::global(&names)
    } else {
        NoticeRow::project(&toolchain.dir, &names)
    }
}

async fn throttled(marker: PathBuf, interval: Duration) -> bool {
    // A panicked blocking task reads as not throttled, as in the self check.
    tokio::task::spawn_blocking(move || StateStore::is_throttled(&marker, interval))
        .await
        .unwrap_or(false)
}

/// The current lock's bindings; `None` for a missing or stale lock, a read failure, or an
/// unconsented project.
async fn bound_tools(ctx: &Context, toolchain: &ProjectIdentity, global: bool) -> Option<Vec<BoundTool>> {
    let lock = if global {
        ProjectLock::from_path(&ocx_project::lock::lock_path_for(&toolchain.config_path))
            .await
            .inspect_err(|error| log::debug!("Toolchain drift check: {error}"))
            .ok()
            .flatten()
    } else {
        // An unconsented clone's lock must not choose the registry hosts every command contacts.
        let whitelist = effective_consent(ctx.config().shell.as_ref());
        let platform = crate::conventions::platform_or_default(None);
        let consent =
            activation::evaluate_consent(&whitelist, &platform, &ctx.file_structure().packages, toolchain).await;
        if !matches!(consent.decision(), Decision::Activate(_)) {
            log::debug!("Toolchain drift check skipped: project not consented");
            return None;
        }
        consent.lock().cloned()
    }?;
    let config = ProjectConfig::from_path(&toolchain.config_path)
        .await
        .inspect_err(|error| log::debug!("Toolchain drift check: {error}"))
        .ok()?;
    match lock.bind(&config) {
        Binding::Current(bound) => Some(bound),
        Binding::Stale(drift) => {
            log::debug!("Toolchain drift check skipped: stale lock: {drift}");
            None
        }
    }
}
