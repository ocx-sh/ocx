// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Self-install bootstrap for `ocx self setup`.
//!
//! Installs the latest published `ocx.sh/ocx/cli`, never the running binary's own
//! version: dev builds carry unpublished build-timestamp tags
//! (`.claude/artifacts/adr_self_setup.md` Decision 2).

use std::time::Duration;

use crate::error::Error as SetupError;
use crate::version_spec::VersionSpec;
use ocx_package_manager::concurrency::Concurrency;
use ocx_package_manager::error::{Error as PmError, PackageError, PackageErrorKind};
use ocx_package_manager::{PackageManager, SkippedReason, TagProbe, UpdateCheckResult};
use ocx_store::file_structure::FileStructure;

/// Status discriminant for a bootstrap run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootstrapStatus {
    /// The installed `current` already points at the requested version.
    AlreadyPresent,
    /// The requested version was pulled and selected.
    Pulled,
    /// Dry-run: the requested version would be pulled.
    WouldPull,
}

/// Outcome of ensuring `ocx.sh/ocx/cli` is installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapOutcome {
    /// Bootstrap status discriminant.
    pub status: BootstrapStatus,
    /// Tag string when known; `None` for digest-only pins.
    pub version: Option<String>,
    /// Resolved digest when available; `None` on the unpinned fast path.
    pub digest: Option<ocx_oci::Digest>,
}

/// Decision produced by mapping an [`UpdateCheckResult`], without side effects.
#[derive(Debug)]
enum Decision {
    /// Already up to date — report [`BootstrapOutcome::AlreadyPresent`].
    AlreadyPresent,
    /// Install the carried identifier; `version` is the latest published tag.
    Install {
        identifier: ocx_oci::PackageRef,
        version: String,
    },
    /// Bootstrap cannot proceed — surface this error to the caller.
    Fail(PmError),
}

/// Ensures the specified (or latest published) `ocx.sh/ocx/cli` release is installed.
///
/// Selects it as `current`; offline and already installed returns `AlreadyPresent`.
///
/// # Errors
///
/// - Offline and not installed: exit 81.
/// - Registry probe failure: exit 69.
/// - `tag@digest` mismatch: [`crate::error::Error::PinDigestMismatch`] (exit 65).
/// - Install failure: the underlying [`ocx_package_manager::Error`].
pub async fn ensure_self_installed(
    manager: &PackageManager,
    file_structure: &FileStructure,
    dry_run: bool,
    version: Option<&VersionSpec>,
) -> Result<BootstrapOutcome, crate::error::Error> {
    if let Some(spec) = version {
        return ensure_pinned(manager, spec, dry_run).await;
    }

    let identifier = ocx_oci::ocx_cli_identifier();

    // `Duration::ZERO` bypasses the throttle, whose `Skipped(Throttled)` would fail bootstrap as unavailable.
    let check = manager
        .check_update(&identifier, Some(Duration::ZERO), TagProbe::Remote)
        .await
        .map_err(|kind| PmError::InstallFailed(vec![PackageError::new(identifier.clone(), kind)]))?;

    // The only signal separating offline-and-installed from offline-and-empty (exit 81); a dangling link reads absent.
    let current = file_structure.symlinks.current(&identifier);
    let already_installed = ocx_util::fs::path_exists_lossy(&current).await;

    let decision = decide(check, already_installed);

    match decision {
        Decision::AlreadyPresent => Ok(BootstrapOutcome {
            status: BootstrapStatus::AlreadyPresent,
            version: None,
            digest: None,
        }),
        Decision::Fail(error) => Err(SetupError::Bootstrap(error)),
        Decision::Install { identifier, version } => {
            if dry_run {
                return Ok(BootstrapOutcome {
                    status: BootstrapStatus::WouldPull,
                    version: Some(version),
                    digest: None,
                });
            }
            let candidate = false;
            let select = true;
            // Patch discovery on ocx itself can abort bootstrap with a spurious required-companion error.
            let skip_discovery = true;
            manager
                .install_all(
                    vec![identifier],
                    ocx_oci::Platform::current().unwrap_or_else(ocx_oci::Platform::any),
                    candidate,
                    select,
                    Concurrency::default(),
                    skip_discovery,
                )
                .await?;
            Ok(BootstrapOutcome {
                status: BootstrapStatus::Pulled,
                version: Some(version),
                digest: None,
            })
        }
    }
}

/// Pinned bootstrap path; dry-run still resolves, read-only.
async fn ensure_pinned(
    manager: &PackageManager,
    spec: &VersionSpec,
    dry_run: bool,
) -> Result<BootstrapOutcome, SetupError> {
    let id = spec.apply(ocx_oci::ocx_cli_identifier());

    let resolved = resolve_pinned_digest(manager, spec, &id).await?;

    // `test_tag_digest_mismatch_exits_65` owns this fail-closed check.
    if let (Some(tag), Some(pinned)) = (spec.tag(), spec.digest())
        && *pinned != resolved
    {
        return Err(SetupError::PinDigestMismatch {
            tag: tag.to_string(),
            expected: pinned.clone(),
            resolved,
            // Unconditional: `ChainMode` is not observable here, and the hint is harmless online.
            hint: Some(
                "if you froze resolution to a stale local index, run `ocx index update` without --frozen".to_string(),
            ),
        });
    }

    let installed = manager
        .installed_current_digest(&id)
        .await
        .map_err(|e| bootstrap_error(PackageErrorKind::Internal(e)))?;

    if installed.as_ref() == Some(&resolved) {
        return Ok(BootstrapOutcome {
            status: BootstrapStatus::AlreadyPresent,
            version: spec.tag().map(str::to_owned),
            digest: Some(resolved),
        });
    }

    if dry_run {
        return Ok(BootstrapOutcome {
            status: BootstrapStatus::WouldPull,
            version: spec.tag().map(str::to_owned),
            digest: Some(resolved),
        });
    }

    maybe_warn_downgrade(manager, spec, &id, installed.as_ref()).await;

    let install_id = id.clone_with_digest(resolved.clone());
    let candidate = false;
    let select = true;
    // Patch discovery on ocx itself can abort bootstrap with a spurious required-companion error.
    let skip_discovery = true;
    manager
        .install_all(
            vec![install_id],
            ocx_oci::Platform::current().unwrap_or_else(ocx_oci::Platform::any),
            candidate,
            select,
            Concurrency::default(),
            skip_discovery,
        )
        .await?;

    Ok(BootstrapOutcome {
        status: BootstrapStatus::Pulled,
        version: spec.tag().map(str::to_owned),
        digest: Some(resolved),
    })
}

/// Resolves the pin to the platform-selected manifest digest, the one `current`'s
/// `digest` file holds, so the already-current check compares like with like.
async fn resolve_pinned_digest(
    manager: &PackageManager,
    spec: &VersionSpec,
    id: &ocx_oci::PackageRef,
) -> Result<ocx_oci::Digest, SetupError> {
    // Drop a `tag@digest` pin's digest, or the mismatch cross-check compares the pin with itself.
    let resolve_id = if spec.tag().is_some() {
        id.without_digest()
    } else {
        id.clone()
    };

    let chain = manager
        .resolve(
            &resolve_id,
            ocx_oci::Platform::current().unwrap_or_else(ocx_oci::Platform::any),
        )
        .await
        .map_err(|kind| match kind {
            PackageErrorKind::NotFound => bootstrap_error(PackageErrorKind::NotFound),
            other => bootstrap_error(other),
        })?;

    Ok(chain.pinned.digest())
}

/// Warns when the pinned tag is semver-older than the installed `current` version;
/// silently skips when either side is unknown or unparseable.
async fn maybe_warn_downgrade(
    manager: &PackageManager,
    spec: &VersionSpec,
    id: &ocx_oci::PackageRef,
    installed: Option<&ocx_oci::Digest>,
) {
    use ocx_package::version::Version;

    if installed.is_none() {
        return;
    }

    let Some(pinned_tag) = spec.tag() else {
        return;
    };
    let Some(pinned_version) = Version::parse(pinned_tag) else {
        return;
    };
    let Some(installed_str) = manager.query_installed_self_version(id).await else {
        return;
    };
    let Some(installed_version) = Version::parse(&installed_str) else {
        return;
    };

    if pinned_version < installed_version {
        log::warn!("downgrade {installed_version} -> {pinned_version}");
    }
}

/// Wraps a [`PackageErrorKind`] into [`SetupError::Bootstrap`] for exit-code classification.
fn bootstrap_error(kind: PackageErrorKind) -> SetupError {
    SetupError::Bootstrap(PmError::InstallFailed(vec![PackageError::new(
        ocx_oci::ocx_cli_identifier(),
        kind,
    )]))
}

/// Maps an [`UpdateCheckResult`] to a [`Decision`]; `already_installed` is whether `current` resolves.
fn decide(check: UpdateCheckResult, already_installed: bool) -> Decision {
    match check {
        UpdateCheckResult::AlreadyUpToDate => Decision::AlreadyPresent,
        UpdateCheckResult::UpdateAvailable(identifier) => match identifier.tag().map(str::to_owned) {
            Some(version) => Decision::Install { identifier, version },
            None => Decision::Fail(registry_unavailable("latest release identifier has no tag")),
        },
        UpdateCheckResult::Skipped(reason) => map_skipped(reason, already_installed),
    }
}

/// Maps a [`SkippedReason`] to a [`Decision`]: only `Offline` with `current` already
/// resolving succeeds.
fn map_skipped(reason: SkippedReason, already_installed: bool) -> Decision {
    match reason {
        SkippedReason::Offline if already_installed => {
            log::debug!("self-setup bootstrap: offline with a resolved `current` install (skip pull)");
            Decision::AlreadyPresent
        }
        SkippedReason::Offline => Decision::Fail(offline_blocked()),
        SkippedReason::Bootstrap
        | SkippedReason::Throttled
        | SkippedReason::NotFound
        | SkippedReason::UnparseableCurrent(_)
        | SkippedReason::UnparseableLatest
        | SkippedReason::NoReleaseTag => Decision::Fail(registry_unavailable(reason.to_string())),
        SkippedReason::RegistryProbeFailed(detail) => Decision::Fail(registry_unavailable(detail)),
    }
}

/// Builds a bootstrap error that classifies to exit 81 (`PolicyBlocked`).
fn offline_blocked() -> PmError {
    let identifier = ocx_oci::ocx_cli_identifier();
    PmError::InstallFailed(vec![PackageError::new(
        identifier,
        PackageErrorKind::Internal(ocx_package_manager::Error::OfflineMode),
    )])
}

/// Builds a bootstrap error that classifies to exit 69 (`Unavailable`).
fn registry_unavailable(detail: impl Into<String>) -> PmError {
    let identifier = ocx_oci::ocx_cli_identifier();
    let client_error = ocx_oci::client::error::ClientError::Registry(detail.into().into());
    PmError::InstallFailed(vec![PackageError::new(
        identifier,
        PackageErrorKind::Internal(ocx_package_manager::Error::OciClient(client_error)),
    )])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn latest_identifier(version: &str) -> ocx_oci::PackageRef {
        ocx_oci::ocx_cli_identifier().clone_with_tag(version.to_string())
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
    /// `Install` — the version is the tag, never `ocx_cli::app::version()`.
    #[test]
    fn update_available_installs_latest_published_version() {
        let identifier = latest_identifier("9.9.9");
        match decide(UpdateCheckResult::UpdateAvailable(identifier), false) {
            Decision::Install { version, .. } => assert_eq!(version, "9.9.9"),
            other => panic!("expected Install, got {other:?}"),
        }
    }

    /// Offline + `current` resolves (`already_installed = true`) → `AlreadyPresent`
    /// (re-runs work offline on an installed machine).
    #[test]
    fn offline_and_present_is_already_present() {
        assert!(matches!(
            decide(UpdateCheckResult::Skipped(SkippedReason::Offline), true),
            Decision::AlreadyPresent
        ));
    }

    /// Offline + empty CAS (`already_installed = false`) → exit 81. This is the
    /// reachable path now that presence is probed from the `current` symlink
    /// rather than inferred from `is_offline()` (which always agreed with the
    /// offline cause, making the exit-81 arm dead).
    #[test]
    fn offline_and_not_present_classifies_offline_blocked() {
        let Decision::Fail(_error) = decide(UpdateCheckResult::Skipped(SkippedReason::Offline), false) else {
            panic!("expected Fail for offline + empty CAS");
        };
    }

    /// Registry probe failure → exit 69 (`Unavailable`).
    #[test]
    fn registry_probe_failure_classifies_unavailable() {
        let reason = SkippedReason::RegistryProbeFailed("connection refused".to_string());
        let Decision::Fail(_error) = decide(UpdateCheckResult::Skipped(reason), false) else {
            panic!("expected Fail for registry probe failure");
        };
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
            let Decision::Fail(_error) = decide(UpdateCheckResult::Skipped(reason), false) else {
                panic!("expected Fail for non-offline skip");
            };
        }
    }
}
