// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use ocx_lib::cli::ExitCode as OcxExitCode;
use ocx_lib::env;
use ocx_lib::setup::{self, ProfileOutcome, SetupOptions, SetupOutcome};

use crate::api::data::self_setup::SelfSetupData;

/// Create or refresh ocx shell integration.
///
/// Completes a bare-binary install: installs the latest published ocx into the
/// content store, writes the per-shell env shims into `$OCX_HOME`, and adds a
/// managed activation block to your shell profiles. Re-running is safe: the
/// shims and blocks are diff-gated, so an unchanged setup is a no-op.
///
/// The managed block is fenced (`# >>> ocx v1 <hash> >>>`). If you edit the
/// block by hand, that profile is reported as dirty and left untouched (exit
/// 82); pass `--force` to overwrite it. Pass `--no-modify-path` (or set
/// `OCX_NO_MODIFY_PATH` to a truthy value) to write the shims without touching
/// any profile (the opt-out is not remembered, so repeat it each run).
///
/// On Windows, a `Restricted` execution policy makes the profile block inert;
/// setup prints how to relax it but never changes the policy itself.
///
/// See https://ocx.sh/docs/user-guide#install-bare-binary for the full setup
/// walkthrough.
///
/// # Exit codes
///
/// | Outcome | Exit |
/// |---|---|
/// | completed / no-op / migrated | 0 |
/// | bootstrap blocked (offline, not installed) | 81 |
/// | a profile was dirty and skipped (no `--force`) | 82 |
#[derive(Parser)]
pub struct SelfSetup {
    /// Write the env shims but do not modify any shell profile.
    ///
    /// A truthy `OCX_NO_MODIFY_PATH` (`1`/`y`/`yes`/`on`/`true`) sets this too. The
    /// opt-out is not remembered between runs - repeat the flag (or keep the env
    /// var set) each invocation.
    #[arg(long, default_value_t = env::flag(env::keys::OCX_NO_MODIFY_PATH, false))]
    no_modify_path: bool,

    /// Target an explicit profile file. Repeatable. Default: auto-detect.
    ///
    /// Explicit targets are written with POSIX-fence semantics regardless of
    /// the file name.
    #[arg(long, value_name = "PATH")]
    profile: Vec<PathBuf>,

    /// Report the intended actions without writing anything.
    #[arg(long)]
    dry_run: bool,

    /// Overwrite a managed block that carries user edits (the dirty state).
    #[arg(long)]
    force: bool,
}

impl SelfSetup {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let options = SetupOptions {
            no_modify_path: self.no_modify_path,
            profiles: self.profile.clone(),
            dry_run: self.dry_run,
            force: self.force,
        };

        let outcome = setup::run(&options, context.manager(), context.file_structure()).await?;

        // Advisories go to stderr (human diagnostics), never the data stream.
        emit_advisories(&context, &outcome);

        // A dirty profile left untouched (no --force) is a non-error outcome;
        // the exit code is decided here by inspecting the outcomes (contract 4).
        // Dry-run never returns the dirty code — it only reports would-skip.
        let exit = exit_code_for(&outcome, self.force, self.dry_run);

        context.api().report(&SelfSetupData::from_outcome(&outcome))?;
        Ok(exit)
    }
}

/// Print the non-fatal advisories to stderr: the Windows exec-policy hint, a
/// shadowing-`ocx` warning, and the "re-source your profile" reload hint.
fn emit_advisories(context: &crate::app::Context, outcome: &SetupOutcome) {
    if let Some(warning) = &outcome.exec_policy_warning {
        context.ui().warn(warning);
    }
    if let Some(path) = &outcome.conflicting_ocx {
        context.ui().warn(format!(
            "another ocx at {} shadows the one ocx self setup just installed",
            path.display()
        ));
    }
    if outcome.reload_hint {
        context.ui().status(
            "Setup",
            "re-source your shell profile (or open a new shell) to activate ocx",
        );
    }
}

/// Decide the process exit code from the run outcome.
///
/// A profile left untouched because the user edited it (no `--force`) maps to
/// [`OcxExitCode::DirtyRcBlock`] (82) so a script can detect it. `--force`
/// rewrites the block (so no profile is `SkippedDirty`) and `dry_run` only
/// reports would-skip — neither returns 82.
fn exit_code_for(outcome: &SetupOutcome, force: bool, dry_run: bool) -> ExitCode {
    let any_dirty = outcome
        .profiles
        .iter()
        .any(|(_, profile_outcome)| *profile_outcome == ProfileOutcome::SkippedDirty);
    if any_dirty && !force && !dry_run {
        return OcxExitCode::DirtyRcBlock.into();
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ocx_lib::setup::{BootstrapOutcome, ProfileOutcome, SetupOutcome};

    use super::exit_code_for;

    fn outcome(profiles: Vec<(PathBuf, ProfileOutcome)>) -> SetupOutcome {
        SetupOutcome {
            bootstrap: BootstrapOutcome::AlreadyPresent,
            shims_written: Vec::new(),
            profiles,
            exec_policy_warning: None,
            conflicting_ocx: None,
            reload_hint: false,
        }
    }

    /// Round-trip an `ExitCode` through its Debug form to compare against a
    /// known numeric value (`ExitCode` is opaque, but `From<u8>` is stable).
    fn exit_code_equals(actual: std::process::ExitCode, expected: u8) -> bool {
        format!("{actual:?}") == format!("{:?}", std::process::ExitCode::from(expected))
    }

    /// A dirty profile without `--force` maps to exit 82.
    #[test]
    fn dirty_without_force_is_exit_82() {
        let base = outcome(vec![(PathBuf::from(".zshrc"), ProfileOutcome::SkippedDirty)]);
        assert!(exit_code_equals(exit_code_for(&base, false, false), 82));
    }

    /// `--force` rewrites the block (no SkippedDirty in the outcome), so it is
    /// exit 0 — but even a stray SkippedDirty under force stays 0.
    #[test]
    fn dirty_with_force_is_success() {
        let base = outcome(vec![(PathBuf::from(".zshrc"), ProfileOutcome::SkippedDirty)]);
        assert!(exit_code_equals(exit_code_for(&base, true, false), 0));
    }

    /// Dry-run never returns 82 even when a profile would be skipped dirty.
    #[test]
    fn dirty_dry_run_is_success() {
        let base = outcome(vec![(PathBuf::from(".zshrc"), ProfileOutcome::SkippedDirty)]);
        assert!(exit_code_equals(exit_code_for(&base, false, true), 0));
    }

    /// A clean run (completed / no-op profiles) is exit 0.
    #[test]
    fn clean_run_is_success() {
        let base = outcome(vec![
            (PathBuf::from(".bashrc"), ProfileOutcome::Completed),
            (PathBuf::from(".zshrc"), ProfileOutcome::NoOp),
        ]);
        assert!(exit_code_equals(exit_code_for(&base, false, false), 0));
    }
}
