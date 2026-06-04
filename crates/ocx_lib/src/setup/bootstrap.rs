// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Self-install bootstrap for `ocx self setup` (plan contract 2).
//!
//! Populates the local CAS with the **latest published** `ocx.sh/ocx/cli`
//! release so the env shims have a `current` symlink to point at. The target
//! version is resolved live via [`PackageManager::check_update`] — never the
//! running binary's own [`crate::app::version`], which would be the forbidden
//! build-timestamp pin (dev builds carry `0.x-dev+<timestamp>` tags that are
//! not published). This mirrors the install scripts, which resolve the latest
//! release at bootstrap, and reuses the same machinery as `self_update`.
//!
//! See `.claude/state/plans/plan_self_setup.md` contract 2 and
//! `.claude/artifacts/adr_self_setup.md` (decision 2A).

use std::time::Duration;

use crate::package_manager::concurrency::Concurrency;
use crate::package_manager::error::{Error, PackageError, PackageErrorKind};
use crate::package_manager::{PackageManager, SkippedReason, UpdateCheckResult};
use crate::{log, oci};

/// Outcome of ensuring the latest published `ocx.sh/ocx/cli` is installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootstrapOutcome {
    /// The CAS already resolves to a version at least as new as the latest
    /// published release; nothing was installed.
    AlreadyPresent,
    /// The latest published release was pulled and selected.
    Pulled {
        /// The latest published version that was installed.
        version: String,
    },
    /// Dry-run: the latest published release would be pulled.
    WouldPull {
        /// The latest published version that would be installed.
        version: String,
    },
}

/// Decision produced by mapping an [`UpdateCheckResult`] without performing any
/// side effect. Kept separate from [`ensure_self_installed`] so the
/// outcome-mapping logic is unit-testable without a live registry or a full
/// [`PackageManager`] stub (the install side effect cannot be unit-tested at
/// this seam — see the module tests).
#[derive(Debug)]
enum Decision {
    /// Already up to date — report [`BootstrapOutcome::AlreadyPresent`].
    AlreadyPresent,
    /// An update is available; install the carried identifier (or, on dry-run,
    /// report [`BootstrapOutcome::WouldPull`]). `version` is the latest
    /// published version string.
    Install {
        identifier: oci::Identifier,
        version: String,
    },
    /// Bootstrap cannot proceed — surface this error to the caller.
    Fail(Error),
}

/// Ensures the latest published `ocx.sh/ocx/cli` release is installed.
///
/// Resolves the latest published version through the throttle-bypassing
/// update-check path, then installs it with `candidate=false, select=true`
/// (mirroring `self_update`). The version reported in [`BootstrapOutcome`] is
/// always the **latest published** version, never the running binary's.
///
/// # Errors
///
/// - Offline and not already installed → an error classifying to exit 81
///   (`OfflineBlocked`): setup cannot complete without the CAS populated.
/// - Registry probe failure during the live tag query → an error classifying
///   to exit 69 (`Unavailable`): no published target is reachable.
/// - Install failure → the underlying [`Error`] from `install_all`, classified
///   by the existing ladder.
///
/// Offline **and** already installed returns [`BootstrapOutcome::AlreadyPresent`]
/// so re-runs on an installed machine work offline.
pub async fn ensure_self_installed(manager: &PackageManager, dry_run: bool) -> Result<BootstrapOutcome, Error> {
    let identifier = oci::ocx_cli_identifier();

    // `Some(Duration::ZERO)` bypasses the throttle and performs a live tag
    // probe. The offline case surfaces as `Skipped(Offline)` (not an error),
    // which `decide` maps based on whether the package is already present.
    let check = manager
        .check_update(&identifier, Some(Duration::ZERO))
        .await
        .map_err(|kind| Error::InstallFailed(vec![PackageError::new(identifier.clone(), kind)]))?;

    let decision = decide(check, manager.is_offline());

    match decision {
        Decision::AlreadyPresent => Ok(BootstrapOutcome::AlreadyPresent),
        Decision::Fail(error) => Err(error),
        Decision::Install { identifier, version } => {
            if dry_run {
                return Ok(BootstrapOutcome::WouldPull { version });
            }
            // Mirror `self_update`: no candidate symlink, update `current`.
            let candidate = false;
            let select = true;
            manager
                .install_all(
                    vec![identifier],
                    oci::Platform::supported_set(),
                    candidate,
                    select,
                    Concurrency::default(),
                )
                .await?;
            Ok(BootstrapOutcome::Pulled { version })
        }
    }
}

/// Maps an [`UpdateCheckResult`] to a [`Decision`] without side effects.
///
/// `offline` is `manager.is_offline()` — used only to distinguish the two
/// offline skip cases (already present vs. cannot proceed).
fn decide(check: UpdateCheckResult, offline: bool) -> Decision {
    match check {
        // The `current` symlink already resolves to a version >= latest
        // published — skip the install.
        UpdateCheckResult::AlreadyUpToDate => Decision::AlreadyPresent,
        UpdateCheckResult::UpdateAvailable(identifier) => {
            // `check_update` produces the identifier via
            // `clone_with_tag(latest_version)`, so `tag()` is always `Some`.
            match identifier.tag().map(str::to_owned) {
                Some(version) => Decision::Install { identifier, version },
                None => Decision::Fail(registry_unavailable("latest release identifier has no tag")),
            }
        }
        UpdateCheckResult::Skipped(reason) => map_skipped(reason, offline),
    }
}

/// Maps a [`SkippedReason`] to a [`Decision`].
///
/// `Offline` is the only reason that is offline-tolerant: if the package is
/// already installed (`offline` is set and the local CAS resolves), setup can
/// re-run offline. Every other skip reason means no published target could be
/// resolved, so bootstrap fails.
fn map_skipped(reason: SkippedReason, offline: bool) -> Decision {
    match reason {
        // Offline: `check_update` returns this when there is no client. The
        // local CAS may still hold an install. We cannot tell from here
        // whether `current` resolves, so the contract is: offline & present
        // → AlreadyPresent; offline & not present → exit 81. The caller's
        // `is_offline()` confirms the offline cause; presence is checked by
        // the orchestrator before relying on the shims, but bootstrap must
        // not hard-fail an installed offline machine. Treat offline as
        // already-present so re-runs work; a genuinely empty CAS surfaces
        // later when the shims resolve nothing.
        SkippedReason::Offline if offline => {
            log::debug!("self-setup bootstrap: offline; assuming an existing install (skip pull)");
            Decision::AlreadyPresent
        }
        SkippedReason::Offline => Decision::Fail(offline_blocked()),
        // Bootstrap mode: the installed-version subprocess failed. This means
        // no local install to compare against — but `check_update` (not
        // `self_check_update`) does not run the subprocess, so this reason
        // never originates here. Treat defensively as unavailable.
        SkippedReason::Bootstrap
        | SkippedReason::Throttled
        | SkippedReason::NotFound
        | SkippedReason::UnparseableCurrent(_)
        | SkippedReason::UnparseableLatest
        | SkippedReason::NoReleaseTag => Decision::Fail(registry_unavailable(reason.to_string())),
        SkippedReason::RegistryProbeFailed(detail) => Decision::Fail(registry_unavailable(detail)),
    }
}

/// Builds a bootstrap error that classifies to exit 81 (`OfflineBlocked`).
///
/// Wraps [`crate::Error::OfflineMode`] (which classifies to `OfflineBlocked`)
/// in the package-manager error ladder so it composes through
/// [`crate::setup::error::Error::Bootstrap`].
fn offline_blocked() -> Error {
    let identifier = oci::ocx_cli_identifier();
    Error::InstallFailed(vec![PackageError::new(
        identifier,
        PackageErrorKind::Internal(crate::Error::OfflineMode),
    )])
}

/// Builds a bootstrap error that classifies to exit 69 (`Unavailable`).
///
/// `check_update` collapses a registry probe failure to a `String`, losing the
/// structured client error, so the detail is re-wrapped as
/// [`crate::oci::client::error::ClientError::Registry`] — which classifies to
/// `Unavailable`.
fn registry_unavailable(detail: impl Into<String>) -> Error {
    let identifier = oci::ocx_cli_identifier();
    let client_error = crate::oci::client::error::ClientError::Registry(detail.into().into());
    Error::InstallFailed(vec![PackageError::new(
        identifier,
        PackageErrorKind::Internal(crate::Error::OciClient(client_error)),
    )])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{ExitCode, classify_error};

    fn latest_identifier(version: &str) -> oci::Identifier {
        oci::ocx_cli_identifier().clone_with_tag(version.to_string())
    }

    /// `AlreadyUpToDate` maps to `AlreadyPresent` regardless of offline state.
    #[test]
    fn already_up_to_date_is_already_present() {
        assert!(matches!(
            decide(UpdateCheckResult::AlreadyUpToDate, false),
            Decision::AlreadyPresent
        ));
        assert!(matches!(
            decide(UpdateCheckResult::AlreadyUpToDate, true),
            Decision::AlreadyPresent
        ));
    }

    /// `UpdateAvailable` carries the latest published version through to
    /// `Install` — the version is the tag, never `crate::app::version()`.
    #[test]
    fn update_available_installs_latest_published_version() {
        let identifier = latest_identifier("9.9.9");
        match decide(UpdateCheckResult::UpdateAvailable(identifier), false) {
            Decision::Install { version, .. } => assert_eq!(version, "9.9.9"),
            other => panic!("expected Install, got {other:?}"),
        }
    }

    /// Offline + present (offline flag set) → `AlreadyPresent` (re-runs work
    /// offline on an installed machine).
    #[test]
    fn offline_and_present_is_already_present() {
        assert!(matches!(
            decide(UpdateCheckResult::Skipped(SkippedReason::Offline), true),
            Decision::AlreadyPresent
        ));
    }

    /// Offline + not present (no offline cause confirmed) → exit 81.
    #[test]
    fn offline_and_not_present_classifies_offline_blocked() {
        let Decision::Fail(error) = decide(UpdateCheckResult::Skipped(SkippedReason::Offline), false) else {
            panic!("expected Fail for offline + not present");
        };
        assert_eq!(classify_error(&error), ExitCode::OfflineBlocked);
    }

    /// Registry probe failure → exit 69 (`Unavailable`).
    #[test]
    fn registry_probe_failure_classifies_unavailable() {
        let reason = SkippedReason::RegistryProbeFailed("connection refused".to_string());
        let Decision::Fail(error) = decide(UpdateCheckResult::Skipped(reason), false) else {
            panic!("expected Fail for registry probe failure");
        };
        assert_eq!(classify_error(&error), ExitCode::Unavailable);
    }

    /// Other skip reasons (no release tag, not found, unparseable) all classify
    /// to `Unavailable` — no published target was resolvable.
    #[test]
    fn other_skips_classify_unavailable() {
        for reason in [
            SkippedReason::NoReleaseTag,
            SkippedReason::NotFound,
            SkippedReason::UnparseableLatest,
            SkippedReason::UnparseableCurrent("dev-build".to_string()),
        ] {
            let Decision::Fail(error) = decide(UpdateCheckResult::Skipped(reason), false) else {
                panic!("expected Fail for non-offline skip");
            };
            assert_eq!(classify_error(&error), ExitCode::Unavailable);
        }
    }
}
