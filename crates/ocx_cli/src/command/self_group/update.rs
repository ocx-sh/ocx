// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;
use ocx_lib::cli::ExitCode as OcxExitCode;
use ocx_lib::package_manager::{HandoffFailure, SelfUpdateResult, TagProbe, UpdateCheckResult};

use crate::api::data::self_update::{SelfUpdateData, UpdateCheckData};

/// Update OCX to the latest available version.
///
/// Without `--check`, downloads and installs the latest release if a newer
/// version exists. With `--check`, only reports whether an update is
/// available — no installation.
///
/// Version discovery lists tags live through the configured index chain
/// (`TagProbe::Remote`): the newest published tag is resolved from the source,
/// the same one the background auto-check uses — self-update exists to reach the
/// freshest upstream release, so it does not read the (possibly stale) local
/// index. Routing through the chain rather than a registry's tags API is what
/// makes the logical `ocx.sh/ocx/cli` name resolve to wherever the published
/// index currently points it. `--offline` still refuses (no client → skipped).
/// (User-facing copy of this lives on the
/// `SelfGroup::Update` variant, which is the surface clap renders; this struct
/// doc is rustdoc-only.)
///
/// Both forms always bypass the throttle (explicit user intent).
///
/// # Exit codes
///
/// | Outcome | Exit |
/// |---|---|
/// | `up_to_date` / `update_available` / `installed` | 0 |
/// | `pulled` (downloaded, nothing activated) | 75 (sysexits `EX_TEMPFAIL`) |
/// | `skipped` (any [`SkippedReason`](ocx_lib::package_manager::SkippedReason)) | 75 (sysexits `EX_TEMPFAIL`) |
///
/// `installed` stays 0 even when the hand-off reported a failure: the binary
/// the user asked for is the one `current` now names, and the advisory says
/// what is left to do.
///
/// Scripts can `case $?` on these without parsing JSON — `--check` returning
/// `update_available` deliberately stays 0 so a "found update" outcome can be
/// distinguished from a "couldn't determine" one (UX-W1).
#[derive(Parser)]
pub struct SelfUpdate {
    /// Check for a newer ocx version without installing it.
    ///
    /// Behaviour:
    ///
    /// * Looks up the latest published version live rather than from your local
    ///   index. Under `--offline` the check is skipped (exit 75).
    /// * Always bypasses the 24h auto-check throttle (explicit user intent).
    /// * Exit status: 0 if the lookup succeeded (whether or not a newer
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
            // Self-update discovers the newest published ocx, so it lists live
            // through the configured index chain (`TagProbe::Remote`) — the same
            // source the background auto-check uses, not the local index a stale
            // `ocx index update` snapshot would echo. `--offline` still refuses
            // (no client → skipped).
            let result = context
                .manager()
                .self_check_update(Some(Duration::ZERO), TagProbe::Remote)
                .await?;
            let exit = exit_code_for_check(&result);
            context.api().report(&UpdateCheckData::from_result(&result))?;
            Ok(exit)
        } else {
            let result = context.manager().self_update().await?;
            // The new binary already ran its own `ocx self setup` (and printed
            // whatever that had to say). All that is left here is the one line
            // about what the user should do next.
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
///
/// A separate type from the result so the policy — which outcome earns which
/// line — is decided by a pure function and unit-tested without a live
/// `Context`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum HandoffAdvisory {
    /// The update landed and its setup completed: one hint that the new
    /// environment needs a fresh shell.
    Reload,
    /// The update landed, but a managed block in a shell profile carries local
    /// edits and was left alone (the child's exit 82).
    DirtyProfile,
    /// The update landed, but the new binary's setup did not finish. Carries
    /// the rendered failure so the user can see how the child ended.
    SetupIncomplete(String),
    /// The release was downloaded but nothing was activated — `current` still
    /// names the old binary, so the machine is unchanged.
    NotActivated(String),
}

/// Decides the advisory for a finished `self update`.
///
/// The verdict already distinguishes swapped from not-swapped; this only
/// chooses the wording, and the one case it must get right is the swap that
/// carries a failure: that is a *successful* update with unfinished setup, not
/// a failed one.
fn advisory_for(result: &SelfUpdateResult) -> Option<HandoffAdvisory> {
    match result {
        SelfUpdateResult::Installed { handoff: None, .. } => Some(HandoffAdvisory::Reload),
        SelfUpdateResult::Installed {
            handoff: Some(failure), ..
        } => Some(match failure {
            // The child got past its select and refused to overwrite a profile
            // the user had edited. Name the flag that overrides it rather than
            // the generic re-run.
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
        // `update_available` deliberately stays SUCCESS — finding an update is
        // not a failure mode (matches rustup / cargo conventions).
        UpdateCheckResult::AlreadyUpToDate | UpdateCheckResult::UpdateAvailable(_) => ExitCode::SUCCESS,
        UpdateCheckResult::Skipped(_) => OcxExitCode::TempFail.into(),
    }
}

fn exit_code_for_update(result: &SelfUpdateResult) -> ExitCode {
    match result {
        // `Installed` is SUCCESS even with a hand-off failure attached: the
        // binary the user asked for is the one `current` now names.
        SelfUpdateResult::AlreadyUpToDate | SelfUpdateResult::Installed { .. } => ExitCode::SUCCESS,
        // `Pulled` joins `Skipped` at EX_TEMPFAIL — the operation could not be
        // completed and re-running it is meaningful.
        SelfUpdateResult::Pulled { .. } | SelfUpdateResult::Skipped(_) => OcxExitCode::TempFail.into(),
    }
}

#[cfg(test)]
mod tests {
    use ocx_lib::cli::ExitCode as OcxExitCode;
    use ocx_lib::package_manager::{SelfUpdateResult, SkippedReason, UpdateCheckResult};

    use super::{HandoffAdvisory, advisory_for, exit_code_for_check, exit_code_for_update};
    use ocx_lib::package_manager::HandoffFailure;
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
