// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;
use ocx_exit::ExitCode as OcxExitCode;
use ocx_package_manager::{HandoffFailure, SelfUpdateResult, TagProbe, UpdateCheckResult};

use crate::api::data::self_update::{SelfUpdateData, UpdateCheckData};

/// Arguments of `ocx self update`; its help text lives on `SelfGroup::Update`.
///
/// Exits 0 for `up_to_date`, `update_available` and `installed` (even with a hand-off failure),
/// 75 for `pulled` and `skipped`.
#[derive(Parser)]
pub struct SelfUpdate {
    /// Check for a newer ocx version without installing it.
    #[arg(long_help = "\
        Check for a newer ocx version without installing it.\n\n\
        Behaviour:\n\n\
        * Looks up the latest published version live rather than from your local index. Under \
        `--offline` the check is skipped (exit 75). * Always bypasses the 24h auto-check throttle \
        (explicit user intent). * Exit status: 0 if the lookup succeeded (whether or not a newer \
        version was found); 75 (`EX_TEMPFAIL`) if the check was skipped. * Output: status, \
        identifier (when an update is available), and structured skip reason. JSON shape: \
        `{\"status\":\"update_available\",\"identifier\":\"ocx.sh/ocx/cli:1.2.3\"}` or \
        `{\"status\":\"skipped\",\"skipped_reason\":{\"reason\":\"offline\"}}`.\n\n\
        Pair with `--format json` for programmatic consumption.")]
    #[arg(long)]
    check: bool,
}

impl SelfUpdate {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        if self.check {
            // Live through the index chain: the local index may be stale, and only the chain routes `ocx.sh/ocx/cli`.
            let result = context
                .manager()
                .self_check_update(Some(Duration::ZERO), TagProbe::Remote)
                .await?;
            let exit = exit_code_for_check(&result);
            context.api().report(&UpdateCheckData::from_result(&result))?;
            Ok(exit)
        } else {
            let result = context.manager().self_update().await?;
            // The new binary already ran its own `ocx self setup`; only the next-step line is left.
            if let Some(advisory) = advisory_for(&result) {
                emit_advisory(&context, &advisory);
            }
            let exit = exit_code_for_update(&result);
            context.api().report(&SelfUpdateData::from_result(&result))?;
            Ok(exit)
        }
    }
}

/// What `ocx self update` says on stderr once the hand-off has finished.
#[derive(Debug, Clone, PartialEq, Eq)]
enum HandoffAdvisory {
    /// The update landed and its setup completed.
    Reload,
    /// The update landed, but a managed profile block with local edits was left alone (the child's exit 82).
    DirtyProfile,
    /// The update landed, but the new binary's setup did not finish.
    SetupIncomplete(String),
    /// Downloaded but not activated: `current` still names the old binary.
    NotActivated(String),
}

/// Decides the advisory; a swap carrying a failure is a successful update with unfinished setup, not a failed one.
fn advisory_for(result: &SelfUpdateResult) -> Option<HandoffAdvisory> {
    match result {
        SelfUpdateResult::Installed { handoff: None, .. } => Some(HandoffAdvisory::Reload),
        SelfUpdateResult::Installed {
            handoff: Some(failure), ..
        } => Some(match failure {
            HandoffFailure::Exited(code) if *code == OcxExitCode::DirtyRcBlock as i32 => HandoffAdvisory::DirtyProfile,
            failure => HandoffAdvisory::SetupIncomplete(failure.to_string()),
        }),
        SelfUpdateResult::Pulled { handoff, .. } => Some(HandoffAdvisory::NotActivated(
            handoff
                .as_ref()
                .map_or_else(|| "its setup reported success".to_owned(), HandoffFailure::to_string),
        )),
        SelfUpdateResult::AlreadyUpToDate | SelfUpdateResult::Skipped(_) => None,
    }
}

/// Writes the advisory to stderr. Diagnostics never touch the data stream.
fn emit_advisory(context: &crate::app::Context, advisory: &HandoffAdvisory) {
    match advisory {
        HandoffAdvisory::Reload => context.ui().status(
            "Setup",
            "shell integration refreshed; re-source your profile or restart your shell",
        ),
        HandoffAdvisory::DirtyProfile => context.ui().warn(
            "a shell profile has local edits in the ocx block; run 'ocx self setup --force' to update it".to_owned(),
        ),
        HandoffAdvisory::SetupIncomplete(detail) => context.ui().warn(format!(
            "ocx was updated but its setup did not finish ({detail}); run 'ocx self setup'"
        )),
        HandoffAdvisory::NotActivated(detail) => context.ui().warn(format!(
            "the new ocx was downloaded but not activated ({detail}); run 'ocx self setup'"
        )),
    }
}

fn exit_code_for_check(result: &UpdateCheckResult) -> ExitCode {
    match result {
        // Finding an update is not a failure, as with rustup and cargo.
        UpdateCheckResult::AlreadyUpToDate | UpdateCheckResult::UpdateAvailable(_) => ExitCode::SUCCESS,
        UpdateCheckResult::Skipped(_) => OcxExitCode::TempFail.into(),
    }
}

fn exit_code_for_update(result: &SelfUpdateResult) -> ExitCode {
    match result {
        // Even with a hand-off failure: `current` now names the binary the user asked for.
        SelfUpdateResult::AlreadyUpToDate | SelfUpdateResult::Installed { .. } => ExitCode::SUCCESS,
        // `Pulled` activated nothing, so re-running is meaningful.
        SelfUpdateResult::Pulled { .. } | SelfUpdateResult::Skipped(_) => OcxExitCode::TempFail.into(),
    }
}

#[cfg(test)]
mod tests {
    use ocx_exit::ExitCode as OcxExitCode;
    use ocx_package_manager::{SelfUpdateResult, SkippedReason, UpdateCheckResult};

    use super::{HandoffAdvisory, advisory_for, exit_code_for_check, exit_code_for_update};
    use ocx_package_manager::HandoffFailure;
    use std::process::ExitCode;

    // ── The hand-off advisory ────────────────────────────────────────────────
    //
    // `self update` no longer refreshes anything itself: it re-executes the
    // newly pulled binary as `ocx self setup … --handoff`, which writes every
    // surface with its own code. What is left here is the wording, and the one
    // thing it must not do is call a completed update a failure.

    /// A swap that landed and whose setup finished gets the reload hint and
    /// nothing else.
    #[test]
    fn a_clean_update_only_hints_at_a_reload() {
        let result = SelfUpdateResult::Installed {
            from: Some("0.6.0".to_string()),
            to: "0.6.1".to_string(),
            handoff: None,
        };
        assert_eq!(advisory_for(&result), Some(HandoffAdvisory::Reload));
    }

    /// Exit 82 is the child refusing to overwrite a user-edited profile — it
    /// happens *after* the select, so the update stands and the advice names
    /// `--force` rather than a bare re-run.
    #[test]
    fn a_dirty_profile_advises_the_force_flag() {
        let result = SelfUpdateResult::Installed {
            from: None,
            to: "0.6.1".to_string(),
            handoff: Some(HandoffFailure::Exited(OcxExitCode::DirtyRcBlock as i32)),
        };
        assert_eq!(advisory_for(&result), Some(HandoffAdvisory::DirtyProfile));
    }

    /// Any other non-zero child on a completed swap names how it ended and
    /// advises a plain setup re-run.
    #[test]
    fn an_unfinished_setup_on_a_completed_swap_names_the_exit() {
        let result = SelfUpdateResult::Installed {
            from: None,
            to: "0.6.1".to_string(),
            handoff: Some(HandoffFailure::Exited(74)),
        };
        match advisory_for(&result) {
            Some(HandoffAdvisory::SetupIncomplete(detail)) => {
                assert!(
                    detail.contains("74"),
                    "the advisory must name the child's exit; got {detail:?}"
                );
            }
            other => panic!("expected SetupIncomplete; got {other:?}"),
        }
    }

    /// Nothing activated: the wording says so, and says what to run.
    #[test]
    fn a_pulled_outcome_says_nothing_was_activated() {
        let result = SelfUpdateResult::Pulled {
            from: Some("0.6.0".to_string()),
            to: "0.6.1".to_string(),
            handoff: Some(HandoffFailure::SpawnFailed("permission denied".to_string())),
        };
        match advisory_for(&result) {
            Some(HandoffAdvisory::NotActivated(detail)) => {
                assert!(
                    detail.contains("permission denied"),
                    "the advisory must carry the failure; got {detail:?}"
                );
            }
            other => panic!("expected NotActivated; got {other:?}"),
        }
    }

    /// The two outcomes that changed nothing say nothing.
    #[test]
    fn outcomes_that_changed_nothing_advise_nothing() {
        assert_eq!(advisory_for(&SelfUpdateResult::AlreadyUpToDate), None);
        assert_eq!(advisory_for(&SelfUpdateResult::Skipped(SkippedReason::Offline)), None);
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
        let identifier =
            ocx_oci::PackageRef::new_registry("ocx/cli", ocx_oci::OCX_SH_REGISTRY).clone_with_tag("1.2.3".to_string());
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

    /// A completed swap is SUCCESS whether or not the hand-off finished — the
    /// binary the user asked for is the one `current` names.
    #[test]
    fn update_installed_is_success() {
        for handoff in [None, Some(HandoffFailure::Exited(OcxExitCode::DirtyRcBlock as i32))] {
            assert!(
                exit_code_equals(
                    exit_code_for_update(&SelfUpdateResult::Installed {
                        from: Some("0.0.1".to_string()),
                        to: "0.0.2".to_string(),
                        handoff: handoff.clone(),
                    }),
                    0
                ),
                "Installed must exit 0; handoff = {handoff:?}"
            );
        }
    }

    /// Pulled-but-not-activated is `EX_TEMPFAIL`: re-running is meaningful.
    #[test]
    fn update_pulled_is_tempfail() {
        assert!(exit_code_equals(
            exit_code_for_update(&SelfUpdateResult::Pulled {
                from: None,
                to: "0.0.2".to_string(),
                handoff: Some(HandoffFailure::Exited(64)),
            }),
            EX_TEMPFAIL
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
