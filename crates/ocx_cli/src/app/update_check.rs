// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_config::env::keys;
use ocx_config::refresh::RefreshPolicy;
use ocx_exit::ExitCode as OcxExitCode;
use ocx_oci::PackageRef;
use ocx_package_manager::{
    HandoffFailure, HandoffStdio, PackageManager, SelfUpdateResult, TagProbe, UpdateCheckResult,
};

use super::update_notice::NoticeRow;
use super::{Context, background_check};
use crate::api::data::{sanitize_error_chain, sanitize_for_terminal};

/// What a newer OCX release asks of the caller.
pub(crate) enum SelfUpdate {
    /// `notify`: a row for the update notice.
    Notice(NoticeRow),
    /// `apply`: the release to install once the command has finished.
    Apply(PackageRef),
}

/// Checks the remote registry for a newer OCX version.
///
/// Skipped under `--frozen`, like the toolchain drift check. Never fails the command: errors are logged at debug level.
pub(crate) async fn check_for_update(ctx: &Context) -> Option<SelfUpdate> {
    if let Some(reason) = background_check::skip_reason(keys::OCX_NO_UPDATE_CHECK, ctx.is_offline()) {
        log::debug!("Update check skipped: {reason}");
        return None;
    }
    // A frozen run neither discovers a new release nor replaces the binary under `apply`.
    if ctx.config_view().frozen {
        log::debug!("Update check skipped: --frozen");
        return None;
    }
    let policy = ctx.update_policy();
    // Before the probe, so `manual` never touches the throttle state file either.
    if policy.self_policy == RefreshPolicy::Manual {
        log::debug!("Update check skipped: [update] self = \"manual\"");
        return None;
    }

    // A live probe regardless of ChainMode: a local read would only echo a stale local index.
    match ctx
        .manager()
        .self_check_update(Some(policy.interval), TagProbe::Remote)
        .await
    {
        Ok(UpdateCheckResult::AlreadyUpToDate) => {
            log::debug!("Already up to date.");
        }
        Ok(UpdateCheckResult::Skipped(reason)) => {
            log::debug!("Update check skipped: {reason}");
        }
        Ok(UpdateCheckResult::UpdateAvailable(identifier)) if policy.self_policy == RefreshPolicy::Apply => {
            return Some(SelfUpdate::Apply(identifier));
        }
        Ok(UpdateCheckResult::UpdateAvailable(identifier)) => {
            return Some(SelfUpdate::Notice(NoticeRow::ocx(&identifier)));
        }
        Err(err) => {
            log::debug!("Update check failed: {err}");
        }
    }
    None
}

/// Installs `identifier` after the user's command has finished; never changes its outcome.
///
/// The hand-off child gets no stdin or stdout: the command's stdout is not ours to write.
pub(crate) async fn apply_pending(manager: &PackageManager, identifier: &PackageRef) {
    let outcome = manager.self_apply(identifier.clone(), HandoffStdio::Unattended).await;
    if let Err(err) = &outcome {
        log::debug!("Automatic ocx update failed: {err:?}");
    }
    if let Some(line) = apply_line(identifier, &outcome) {
        eprintln!("{line}");
    }
}

/// The one stderr line an unattended apply prints, or `None` when it stays silent.
fn apply_line(
    identifier: &PackageRef,
    outcome: &Result<SelfUpdateResult, ocx_package_manager::Error>,
) -> Option<String> {
    let failed = |version: &str, reason: String| {
        Some(format!(
            "Automatic ocx update to {} failed: {reason}. Run `ocx self update` to retry.",
            sanitize_for_terminal(version)
        ))
    };
    match outcome {
        Ok(SelfUpdateResult::Installed { to, handoff: None, .. }) => Some(format!(
            "ocx {} installed; it takes effect on the next run.",
            sanitize_for_terminal(to)
        )),
        Ok(SelfUpdateResult::Installed {
            to, handoff: Some(_), ..
        }) => Some(format!(
            "ocx {} installed; run `ocx self setup` to finish setup.",
            sanitize_for_terminal(to)
        )),
        Ok(SelfUpdateResult::Pulled { to, handoff, .. }) => failed(
            to,
            handoff.as_ref().map_or_else(
                || "the new version was not activated".to_owned(),
                |failure| sanitize_for_terminal(&failure.to_string()),
            ),
        ),
        // `Bootstrap`: an ocx that ocx did not install has nothing to update.
        Ok(SelfUpdateResult::Skipped(reason)) => {
            log::debug!("Automatic ocx update skipped: {reason}");
            None
        }
        Ok(SelfUpdateResult::AlreadyUpToDate) => None,
        Err(err) => failed(
            identifier.tag().unwrap_or("the latest version"),
            sanitize_error_chain(err),
        ),
    }
}

/// What `ocx self update` says on stderr once the hand-off has finished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HandoffAdvisory {
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
pub(crate) fn advisory_for(result: &SelfUpdateResult) -> Option<HandoffAdvisory> {
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
pub(crate) fn emit_advisory(context: &Context, advisory: &HandoffAdvisory) {
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

#[cfg(test)]
mod tests {
    use ocx_exit::ExitCode as OcxExitCode;
    use ocx_package_manager::{HandoffFailure, SelfUpdateResult, SkippedReason};

    use super::{HandoffAdvisory, advisory_for, apply_line};

    fn latest() -> ocx_oci::PackageRef {
        ocx_oci::ocx_cli_identifier().clone_with_tag("0.0.2".to_string())
    }

    // ── The unattended apply line ────────────────────────────────────────────

    #[test]
    fn a_clean_apply_says_it_takes_effect_on_the_next_run() {
        let outcome = Ok(SelfUpdateResult::Installed {
            from: Some("0.0.1".to_string()),
            to: "0.0.2".to_string(),
            handoff: None,
        });
        assert_eq!(
            apply_line(&latest(), &outcome).as_deref(),
            Some("ocx 0.0.2 installed; it takes effect on the next run.")
        );
    }

    #[test]
    fn an_apply_with_unfinished_setup_points_at_self_setup() {
        let outcome = Ok(SelfUpdateResult::Installed {
            from: None,
            to: "0.0.2".to_string(),
            handoff: Some(HandoffFailure::Exited(74)),
        });
        assert_eq!(
            apply_line(&latest(), &outcome).as_deref(),
            Some("ocx 0.0.2 installed; run `ocx self setup` to finish setup.")
        );
    }

    /// `Pulled` activated nothing, so it is a failure, naming how the hand-off ended.
    #[test]
    fn a_pulled_apply_is_a_failed_line_naming_the_handoff() {
        let outcome = Ok(SelfUpdateResult::Pulled {
            from: None,
            to: "0.0.2".to_string(),
            handoff: Some(HandoffFailure::Exited(70)),
        });
        assert_eq!(
            apply_line(&latest(), &outcome).as_deref(),
            Some("Automatic ocx update to 0.0.2 failed: setup exited 70. Run `ocx self update` to retry.")
        );
    }

    /// An ocx not installed by ocx, and a no-op, print nothing.
    #[test]
    fn skipped_and_up_to_date_are_silent() {
        assert_eq!(
            apply_line(&latest(), &Ok(SelfUpdateResult::Skipped(SkippedReason::Bootstrap))),
            None
        );
        assert_eq!(apply_line(&latest(), &Ok(SelfUpdateResult::AlreadyUpToDate)), None);
    }

    /// Registry-controlled text reaches the operator's terminal: ESC and bidi controls are dropped.
    #[test]
    fn a_failed_apply_neutralizes_terminal_controls_in_the_reason() {
        let outcome = Err(ocx_package_manager::Error::UnsupportedMediaType(
            "evil\u{1b}[2J\u{202e}type".to_string(),
            &[],
        ));
        let line = apply_line(&latest(), &outcome).expect("a failure prints a line");
        assert!(!line.contains('\u{1b}') && !line.contains('\u{202e}'), "{line:?}");
        assert!(line.starts_with("Automatic ocx update to 0.0.2 failed: "), "{line:?}");
        assert!(line.contains("evil[2Jtype"), "{line:?}");
        assert!(line.ends_with(". Run `ocx self update` to retry."), "{line:?}");
    }

    // ── The hand-off advisory ────────────────────────────────────────────────
    //
    // `self update` re-executes the pulled binary as `ocx self setup … --handoff`,
    // which writes every surface itself. Only the wording is left here, and it
    // must never call a completed update a failure.

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
}
