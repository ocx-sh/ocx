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

use crate::file_structure::FileStructure;
use crate::package_manager::concurrency::Concurrency;
use crate::package_manager::error::{Error as PmError, PackageError, PackageErrorKind};
use crate::package_manager::{PackageManager, SkippedReason, TagProbe, UpdateCheckResult};
use crate::setup::error::Error as SetupError;
use crate::setup::version_spec::VersionSpec;
use crate::{log, oci};

/// Status discriminant for a bootstrap run (flat variant of former enum payloads).
///
/// Mirrors the `BootstrapStatus` in `api/data/self_setup.rs` — lib carries the
/// typed enum; the API layer stringifies it at the serialization boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootstrapStatus {
    /// The installed `current` already points at the requested version.
    AlreadyPresent,
    /// The requested version was pulled and selected.
    Pulled,
    /// Dry-run: the requested version would be pulled.
    WouldPull,
}

/// Outcome of ensuring `ocx.sh/ocx/cli` is installed (flat struct, plan D7).
///
/// Replaces the former `BootstrapOutcome` enum with variant payloads so the
/// fields map 1:1 to the API `BootstrapEntry` without per-variant ambiguity.
///
/// - `version`: the tag when known (pinned tag or latest-resolved). `None` for
///   digest-only pins.
/// - `digest`: `Some` whenever resolution produced a digest (includes pinned
///   `AlreadyPresent` and `WouldPull`). `None` on the unpinned fast path and
///   on policy-skip offline paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapOutcome {
    /// Bootstrap status discriminant.
    pub status: BootstrapStatus,
    /// Tag string when known; `None` for digest-only pins.
    pub version: Option<String>,
    /// Resolved digest when available; `None` on the unpinned fast path.
    pub digest: Option<oci::Digest>,
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
    Fail(PmError),
}

/// Ensures the specified (or latest published) `ocx.sh/ocx/cli` release is installed.
///
/// When `version` is `None`, resolves the latest published version through the
/// throttle-bypassing update-check path and installs it with
/// `candidate=false, select=true` (mirroring `self_update`). The version
/// reported in [`BootstrapOutcome`] is always the **latest published** version,
/// never the running binary's (Decision 2A unchanged).
///
/// When `version` is `Some(spec)`, takes the pinned path: applies the spec to
/// the base identifier, resolves the digest via the index (ChainMode applies),
/// cross-checks `tag@digest` consistency, and installs only if not already
/// current. Resolved digest is always set in the returned outcome.
///
/// # Errors
///
/// - Offline and not already installed → an error classifying to exit 81
///   (`PolicyBlocked`): setup cannot complete without the CAS populated.
/// - Registry probe failure during the live tag query → an error classifying
///   to exit 69 (`Unavailable`): no published target is reachable.
/// - `tag@digest` mismatch → [`crate::setup::error::Error::PinDigestMismatch`]
///   (exit 65 `DataError`), fail-closed per plan D9.
/// - Install failure → the underlying [`Error`] from `install_all`, classified
///   by the existing ladder.
///
/// Offline **and** already installed returns
/// `BootstrapOutcome { status: AlreadyPresent, .. }` so re-runs on an
/// installed machine work offline.
pub async fn ensure_self_installed(
    manager: &PackageManager,
    file_structure: &FileStructure,
    dry_run: bool,
    version: Option<&VersionSpec>,
) -> Result<BootstrapOutcome, crate::setup::error::Error> {
    if let Some(spec) = version {
        return ensure_pinned(manager, spec, dry_run).await;
    }

    // ── Unpinned path (Decision 2A — latest published, unchanged) ────────────
    let identifier = oci::ocx_cli_identifier();

    // `Some(Duration::ZERO)` bypasses the throttle and performs a live tag
    // probe ([`TagProbe::Remote`]). Bootstrap deliberately stays remote-only:
    // its offline-tolerance contract (`map_skipped`) keys on `Skipped(Offline)`
    // so a re-run on an already-installed machine works offline and an empty
    // CAS surfaces as exit 81. The offline case surfaces as `Skipped(Offline)`
    // (not an error), which `decide` maps based on whether the package is
    // already present. (The *pinned* `self setup X` path resolves through the
    // index/ChainMode separately — see `ensure_pinned`.)
    let check = manager
        .check_update(&identifier, Some(Duration::ZERO), TagProbe::Remote)
        .await
        .map_err(|kind| PmError::InstallFailed(vec![PackageError::new(identifier.clone(), kind)]))?;

    // Probe whether the `current` install actually resolves. `try_exists`
    // follows the symlink, so a dangling or absent `current` reads as `false`.
    // This is the only signal that can distinguish offline-and-installed (re-run
    // works offline) from offline-and-empty-CAS (exit 81): `check_update`
    // collapses both to `Skipped(Offline)`.
    let current = file_structure.symlinks.current(&identifier);
    let already_installed = crate::utility::fs::path_exists_lossy(&current).await;

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
            // Mirror `self_update`: no candidate symlink, update `current`.
            let candidate = false;
            let select = true;
            // skip_discovery=true: bootstrap installs ocx itself via `ocx self setup`.
            // Patch discovery is not applicable here — the ocx binary is not a
            // user-requested tool and looking up its patch descriptor could abort the
            // bootstrap with a spurious required-companion error.
            let skip_discovery = true;
            manager
                .install_all(
                    vec![identifier],
                    oci::Platform::current().unwrap_or_else(oci::Platform::any),
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

/// Pinned bootstrap path (plan D1–D11, `Some(spec)` branch).
///
/// Applies the spec to the canonical self identifier, resolves it to a digest
/// through the index (ChainMode applies, so `--frozen`/`--offline` block an
/// unpinned-tag resolution → exit 81), cross-checks a `tag@digest` pin
/// (fail-closed on mismatch → exit 65), then compares the resolved digest
/// against the installed `current` digest:
///
/// - equal → [`BootstrapStatus::AlreadyPresent`] (re-pin is a no-op)
/// - differ/absent → optional downgrade warn (D10), then
///   `install_all(candidate=false, select=true)` → [`BootstrapStatus::Pulled`]
///
/// Dry-run still resolves (read-only) and reports [`BootstrapStatus::WouldPull`]
/// with the resolved digest; nothing is persisted.
async fn ensure_pinned(
    manager: &PackageManager,
    spec: &VersionSpec,
    dry_run: bool,
) -> Result<BootstrapOutcome, SetupError> {
    let id = spec.apply(oci::ocx_cli_identifier());

    // ── Resolve to a digest through the index (ChainMode applies) ─────────────
    let resolved = resolve_pinned_digest(manager, spec, &id).await?;

    // ── tag@digest cross-check (fail-closed immutability assertion, D9) ───────
    // guarded by acceptance test test_tag_digest_mismatch_exits_65
    if let (Some(tag), Some(pinned)) = (spec.tag(), spec.digest())
        && *pinned != resolved
    {
        return Err(SetupError::PinDigestMismatch {
            tag: tag.to_string(),
            expected: pinned.clone(),
            resolved,
            // ChainMode is not cleanly observable at this seam, so the stale-
            // index hint is attached unconditionally (acceptable per plan D9):
            // under `--frozen` the resolution came from the local index and a
            // refresh is the likely fix; online it is harmless advice.
            hint: Some("if you froze resolution to a stale local index, run `ocx index update`".to_string()),
        });
    }

    // ── Compare against the installed `current` digest (D6) ───────────────────
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

    // ── Dry-run: resolution complete; nothing persisted (dry-run contract) ──
    if dry_run {
        return Ok(BootstrapOutcome {
            status: BootstrapStatus::WouldPull,
            version: spec.tag().map(str::to_owned),
            digest: Some(resolved),
        });
    }

    // ── Downgrade warn (D10): warn-only when both sides parse as semver and the
    //    pinned tag is older than the installed `current` version ──────────────
    maybe_warn_downgrade(manager, spec, &id, installed.as_ref()).await;

    // ── Install the resolved version, selecting `current` (mirrors unpinned) ──
    let install_id = id.clone_with_digest(resolved.clone());
    let candidate = false;
    let select = true;
    // skip_discovery=true: pinned bootstrap installs ocx itself via `ocx self setup`.
    // Patch discovery is not applicable here — same rationale as ensure_latest above.
    let skip_discovery = true;
    manager
        .install_all(
            vec![install_id],
            oci::Platform::current().unwrap_or_else(oci::Platform::any),
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

/// Resolves the pinned spec to the **platform-selected content digest** through
/// the index — the same digest `install_all` materializes and the one the
/// `current` package's `digest` file holds, so the satisfied-check (D6) can
/// compare like with like.
///
/// The package-root `digest` file always holds a platform image-manifest digest
/// (never an image-index digest), because `install_all` writes the platform-
/// selected manifest digest there. Consequently, a digest-only re-pin resolves
/// through the flat `Manifest::Image` arm and returns the same pinned digest
/// unchanged — which is the invariant that makes `bootstrap.digest` round-trip
/// stable as a pin.
///
/// Resolution always goes through [`PackageManager::resolve`] so ChainMode
/// governs the lookup (a `--frozen`/`--offline` unpinned-tag miss surfaces as
/// `PolicyResolutionBlocked` → exit 81) and the platform manifest is selected.
///
/// - Tag present (tag-only or `tag@digest`): resolves via the **tag** (digest
///   dropped) so the index performs a real tag → digest resolution; a genuinely
///   unknown tag (a source was consulted) surfaces as `NotFound` → exit 79.
/// - Digest-only pin: resolves the pinned digest itself (digest fast path —
///   `resolve_top_manifest` fetches the manifest by digest, then platform-selects).
async fn resolve_pinned_digest(
    manager: &PackageManager,
    spec: &VersionSpec,
    id: &oci::Identifier,
) -> Result<oci::Digest, SetupError> {
    // For a `tag@digest` pin, resolve via the TAG (digest dropped) so the index
    // performs a real tag → digest resolution and the cross-check compares the
    // tag's resolved digest against the pinned one. A tag-only or digest-only
    // pin resolves its sole component.
    let resolve_id = if spec.tag().is_some() {
        id.without_digest()
    } else {
        id.clone()
    };

    let chain = manager
        .resolve(&resolve_id, oci::Platform::current().unwrap_or_else(oci::Platform::any))
        .await
        .map_err(|kind| match kind {
            // A tag/digest that genuinely does not exist (a source was consulted)
            // → NotFound (exit 79), per the error taxonomy.
            PackageErrorKind::NotFound => bootstrap_error(PackageErrorKind::NotFound),
            other => bootstrap_error(other),
        })?;

    Ok(chain.pinned.digest())
}

/// Emits a single-line stderr warning when the pinned tag is semver-older than
/// the currently installed `current` version (plan D10, TUF rollback signal).
///
/// Best-effort: silently skips when either side is unknown or not semver-
/// parseable (digest-only pins, dev builds, a fresh machine). Reuses
/// [`Version::parse`](crate::package::version::Version::parse) — the same
/// semver-ish parser `tasks/update_check.rs::find_latest_version` uses — and the
/// `query_installed_self_version` subprocess seam for the installed version.
///
/// Pass `installed` from a previously computed `installed_current_digest` call to
/// avoid a redundant subprocess when nothing is installed (`None` returns early
/// before the subprocess is invoked).
async fn maybe_warn_downgrade(
    manager: &PackageManager,
    spec: &VersionSpec,
    id: &oci::Identifier,
    installed: Option<&oci::Digest>,
) {
    use crate::package::version::Version;

    // Fast exit: nothing is installed, so there is no installed version to compare
    // against and the subprocess would return None anyway.
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

/// Wraps a [`PackageErrorKind`] into the bootstrap error ladder so it composes
/// through [`SetupError::Bootstrap`] and classifies via the existing exit-code
/// mapping (NotFound → 79, PolicyResolutionBlocked → 81, …).
fn bootstrap_error(kind: PackageErrorKind) -> SetupError {
    SetupError::Bootstrap(PmError::InstallFailed(vec![PackageError::new(
        oci::ocx_cli_identifier(),
        kind,
    )]))
}

/// Maps an [`UpdateCheckResult`] to a [`Decision`] without side effects.
///
/// `already_installed` is whether the `current` install symlink resolves on
/// disk — used only to distinguish the two offline skip cases (already present
/// vs. cannot proceed).
fn decide(check: UpdateCheckResult, already_installed: bool) -> Decision {
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
        UpdateCheckResult::Skipped(reason) => map_skipped(reason, already_installed),
    }
}

/// Maps a [`SkippedReason`] to a [`Decision`].
///
/// `Offline` is the only reason that is offline-tolerant: if the `current`
/// install actually resolves (`already_installed`), setup can re-run offline.
/// An offline machine with an empty CAS cannot proceed → exit 81. Every other
/// skip reason means no published target could be resolved, so bootstrap fails.
fn map_skipped(reason: SkippedReason, already_installed: bool) -> Decision {
    match reason {
        // Offline: `check_update` returns this when there is no client. The
        // local CAS may already hold an install — `already_installed` is the
        // probe of the `current` symlink. Offline & present → AlreadyPresent
        // (re-runs work offline); offline & empty CAS → exit 81 (setup cannot
        // wire shims at a non-existent `current`).
        SkippedReason::Offline if already_installed => {
            log::debug!("self-setup bootstrap: offline with a resolved `current` install (skip pull)");
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

/// Builds a bootstrap error that classifies to exit 81 (`PolicyBlocked`).
///
/// Wraps [`crate::Error::OfflineMode`] (which classifies to `PolicyBlocked`)
/// in the package-manager error ladder so it composes through
/// [`crate::setup::error::Error::Bootstrap`].
fn offline_blocked() -> PmError {
    let identifier = oci::ocx_cli_identifier();
    PmError::InstallFailed(vec![PackageError::new(
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
fn registry_unavailable(detail: impl Into<String>) -> PmError {
    let identifier = oci::ocx_cli_identifier();
    let client_error = crate::oci::client::error::ClientError::Registry(detail.into().into());
    PmError::InstallFailed(vec![PackageError::new(
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
        let Decision::Fail(error) = decide(UpdateCheckResult::Skipped(SkippedReason::Offline), false) else {
            panic!("expected Fail for offline + empty CAS");
        };
        assert_eq!(classify_error(&error), ExitCode::PolicyBlocked);
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
