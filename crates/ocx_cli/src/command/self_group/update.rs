// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;
use ocx_lib::cli::ExitCode as OcxExitCode;
use ocx_lib::package_manager::{SelfUpdateResult, UpdateCheckResult};

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
            let exit = exit_code_for_update(&result);
            context.api().report(&SelfUpdateData::from_result(&result))?;
            Ok(exit)
        }
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

    use super::{exit_code_for_check, exit_code_for_update};
    use std::process::ExitCode;

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
