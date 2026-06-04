// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;
use ocx_lib::cli::ExitCode as OcxExitCode;
use ocx_lib::package_manager::{SelfUpdateResult, UpdateCheckResult};
use ocx_lib::setup;

use crate::api::data::self_update::{SelfUpdateData, UpdateCheckData};

/// Update OCX to the latest available version.
///
/// Without `--check`, downloads and installs the latest release if a newer
/// version exists. With `--check`, only queries the registry and reports
/// whether an update is available — no installation.
///
/// Both forms always bypass the throttle (explicit user intent).
///
/// # Exit codes
///
/// | Outcome | Exit |
/// |---|---|
/// | `up_to_date` / `update_available` / `installed` | 0 |
/// | `skipped` (any [`SkippedReason`](ocx_lib::package_manager::SkippedReason)) | 75 (sysexits `EX_TEMPFAIL`) |
///
/// Scripts can `case $?` on these without parsing JSON — `--check` returning
/// `update_available` deliberately stays 0 so a "found update" outcome can be
/// distinguished from a "couldn't determine" one (UX-W1).
#[derive(Parser)]
pub struct SelfUpdate {
    /// Query the registry for a newer ocx version without installing it.
    ///
    /// Behaviour:
    ///
    /// * Always bypasses the 24h auto-check throttle (explicit user intent).
    /// * Exit status: 0 if the registry was reached (whether or not a newer
    ///   version was found); 75 (`EX_TEMPFAIL`) if the check was skipped.
    /// * Output: status, identifier (when an update is available), and
    ///   structured skip reason. JSON shape:
    ///   `{"status":"update_available","identifier":"ocx.sh/ocx/cli:1.2.3"}` or
    ///   `{"status":"skipped","skipped_reason":{"reason":"offline"}}`.
    ///
    /// Pair with `--format json` for programmatic consumption.
    #[arg(long)]
    check: bool,
}

impl SelfUpdate {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        if self.check {
            let result = context.manager().self_check_update(Some(Duration::ZERO)).await?;
            let exit = exit_code_for_check(&result);
            context.api().report(&UpdateCheckData::from_result(&result))?;
            Ok(exit)
        } else {
            let result = context.manager().self_update().await?;
            // After a successful binary swap, refresh the ocx-owned env.* shims
            // so a shim-contract change reaches existing users (Decision 4C).
            // This never touches user RC — only the diff-gated env.* files.
            if let SelfUpdateResult::Installed { .. } = result {
                refresh_shims_after_swap(&context).await;
            }
            let exit = exit_code_for_update(&result);
            context.api().report(&SelfUpdateData::from_result(&result))?;
            Ok(exit)
        }
    }
}

/// Refresh the ocx-owned `env.*` shims after `self update` swapped the binary
/// (Decision 4C).
///
/// Diff-gated and destructive-by-design for `$OCX_HOME/env.*` only — it
/// overwrites those files to the current canonical bytes without warning,
/// discarding any manual edits to them. User RC profiles are never touched.
///
/// Emits the `run 'ocx self setup'` advisory when EITHER (a) the refresh
/// actually rewrote a shim (the on-disk shim contract drifted from what this
/// binary now writes) OR (b) the refresh failed (the swap succeeded but the
/// shims could not be refreshed — the user should re-run setup). A refresh
/// failure is non-fatal to the update itself.
async fn refresh_shims_after_swap(context: &crate::app::Context) {
    // `refresh_shims` is blocking I/O — offload it off the async executor, as
    // `setup::run` does for `write_shims`.
    let ocx_home = context.file_structure().root().to_path_buf();
    let result = tokio::task::spawn_blocking(move || setup::shims::refresh_shims(&ocx_home)).await;

    match classify_refresh(&result) {
        RefreshAdvisory::None => {
            // Shims already current — nothing changed, no advisory.
        }
        RefreshAdvisory::Drift => {
            // The shims drifted (a contract change reached this user) — advise.
            advise_run_setup(context);
        }
        RefreshAdvisory::WriteFailed => {
            if let Ok(Err(error)) = &result {
                context.ui().warn(format!("failed to refresh ocx shims: {error}"));
            }
            advise_run_setup(context);
        }
        RefreshAdvisory::TaskPanicked => {
            if let Err(join) = &result {
                // The blocking task panicked; the swap still succeeded.
                context.ui().warn(format!("ocx shim refresh task failed: {join}"));
            }
            advise_run_setup(context);
        }
    }
}

/// Emit the "run `ocx self setup`" advisory once on stderr.
fn advise_run_setup(context: &crate::app::Context) {
    context
        .ui()
        .status("Setup", "run 'ocx self setup' to refresh shell integration");
}

/// What the post-swap shim refresh should tell the user (Decision 4C, D.1.1
/// item 10b: the advisory fires on drift AND on either failure arm).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefreshAdvisory {
    /// Shims already current — nothing changed, stay silent.
    None,
    /// One or more shims drifted and were rewritten — advise.
    Drift,
    /// A shim could not be written — warn + advise.
    WriteFailed,
    /// The blocking refresh task panicked — warn + advise.
    TaskPanicked,
}

/// Decide the advisory from the joined refresh result. Pure so the
/// advisory-on-refresh-error policy (item 10b) is unit-testable without a live
/// `Context`.
fn classify_refresh(
    result: &Result<Result<Vec<std::path::PathBuf>, setup::error::Error>, tokio::task::JoinError>,
) -> RefreshAdvisory {
    match result {
        Ok(Ok(rewritten)) if rewritten.is_empty() => RefreshAdvisory::None,
        Ok(Ok(_)) => RefreshAdvisory::Drift,
        Ok(Err(_)) => RefreshAdvisory::WriteFailed,
        Err(_) => RefreshAdvisory::TaskPanicked,
    }
}

fn exit_code_for_check(result: &UpdateCheckResult) -> ExitCode {
    match result {
        // `update_available` deliberately stays SUCCESS — finding an update is
        // not a failure mode (matches rustup / cargo conventions).
        UpdateCheckResult::AlreadyUpToDate | UpdateCheckResult::UpdateAvailable(_) => ExitCode::SUCCESS,
        UpdateCheckResult::Skipped(_) => OcxExitCode::TempFail.into(),
    }
}

fn exit_code_for_update(result: &SelfUpdateResult) -> ExitCode {
    match result {
        SelfUpdateResult::AlreadyUpToDate | SelfUpdateResult::Installed { .. } => ExitCode::SUCCESS,
        SelfUpdateResult::Skipped(_) => OcxExitCode::TempFail.into(),
    }
}

#[cfg(test)]
mod tests {
    use ocx_lib::cli::ExitCode as OcxExitCode;
    use ocx_lib::package_manager::{SelfUpdateResult, SkippedReason, UpdateCheckResult};

    use super::{RefreshAdvisory, classify_refresh, exit_code_for_check, exit_code_for_update};
    use std::path::PathBuf;
    use std::process::ExitCode;

    // ── 4C shim-refresh advisory classification (D.1.1 item 10b) ────────────

    #[test]
    fn classify_refresh_no_advisory_when_nothing_drifted() {
        let result = Ok(Ok(Vec::<PathBuf>::new()));
        assert_eq!(classify_refresh(&result), RefreshAdvisory::None);
    }

    #[test]
    fn classify_refresh_drift_advises() {
        let result = Ok(Ok(vec![PathBuf::from("/ocx/env.sh")]));
        assert_eq!(classify_refresh(&result), RefreshAdvisory::Drift);
    }

    #[test]
    fn classify_refresh_write_failure_advises() {
        // A write-permission failure (Ok(Err)) must still surface the advisory.
        let result = Ok(Err(ocx_lib::setup::error::Error::Io {
            path: PathBuf::from("/ocx/env.sh"),
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        }));
        assert_eq!(classify_refresh(&result), RefreshAdvisory::WriteFailed);
    }

    #[tokio::test]
    async fn classify_refresh_task_panic_advises() {
        // A blocking-task panic (Err(join)) must still surface the advisory.
        let join_error = tokio::task::spawn_blocking(|| -> Result<Vec<PathBuf>, ocx_lib::setup::error::Error> {
            panic!("simulated refresh panic")
        })
        .await
        .expect_err("the spawned task panics, so the join must fail");
        let result: Result<Result<Vec<PathBuf>, ocx_lib::setup::error::Error>, tokio::task::JoinError> =
            Err(join_error);
        assert_eq!(classify_refresh(&result), RefreshAdvisory::TaskPanicked);
    }

    /// `EX_TEMPFAIL` (sysexits 75) — the canonical numeric value reused across
    /// `Skipped` outcome tests.
    const EX_TEMPFAIL: u8 = OcxExitCode::TempFail as u8;

    /// Helper: ExitCode does not impl PartialEq, so compare via the numeric
    /// reporting form by writing through a stable channel.
    fn exit_code_equals(a: ExitCode, b: u8) -> bool {
        // Round-trip through the From<u8> impl to compare. ExitCode is opaque,
        // but two values constructed from the same u8 share the same Debug
        // representation.
        format!("{a:?}") == format!("{:?}", ExitCode::from(b))
    }

    #[test]
    fn check_already_up_to_date_is_success() {
        assert!(exit_code_equals(
            exit_code_for_check(&UpdateCheckResult::AlreadyUpToDate),
            0
        ));
    }

    #[test]
    fn check_update_available_is_success() {
        let identifier = ocx_lib::oci::Identifier::new_registry("ocx/cli", ocx_lib::oci::OCX_SH_REGISTRY)
            .clone_with_tag("1.2.3".to_string());
        assert!(exit_code_equals(
            exit_code_for_check(&UpdateCheckResult::UpdateAvailable(identifier)),
            0
        ));
    }

    #[test]
    fn check_skipped_is_tempfail() {
        for reason in [
            SkippedReason::Bootstrap,
            SkippedReason::Offline,
            SkippedReason::Throttled,
            SkippedReason::NotFound,
            SkippedReason::UnparseableLatest,
            SkippedReason::NoReleaseTag,
        ] {
            assert!(
                exit_code_equals(
                    exit_code_for_check(&UpdateCheckResult::Skipped(reason.clone())),
                    EX_TEMPFAIL
                ),
                "Skipped({reason:?}) must map to EX_TEMPFAIL"
            );
        }
    }

    #[test]
    fn update_installed_is_success() {
        assert!(exit_code_equals(
            exit_code_for_update(&SelfUpdateResult::Installed {
                from: Some("0.0.1".to_string()),
                to: "0.0.2".to_string(),
            }),
            0
        ));
    }

    #[test]
    fn update_skipped_is_tempfail() {
        assert!(exit_code_equals(
            exit_code_for_update(&SelfUpdateResult::Skipped(SkippedReason::Bootstrap)),
            EX_TEMPFAIL
        ));
    }
}
