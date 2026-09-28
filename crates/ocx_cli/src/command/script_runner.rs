// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Starlark-script runner shared by `ocx package test --script` and `ocx patch test --script`.

use std::process::ExitCode;

use ocx_config::env;
use ocx_script::{ScriptOutcome, ScriptOutcomeKind};

/// Runs a Starlark script in an already-composed environment and reports it.
///
/// Requires the multi-thread Tokio runtime; a host abort reports `Failed` and exits 1.
///
/// # Errors
///
/// Fails when the report or a requested JUnit sidecar cannot be written.
#[expect(
    clippy::too_many_arguments,
    reason = "eight parameters, one optional: bundling package_root/scratch_root into a struct \
              would name the same two paths twice for one call site each"
)]
pub async fn run_script_in_env(
    context: &crate::app::Context,
    source: &str,
    label: &str,
    package_root: &std::path::Path,
    scratch_root: &std::path::Path,
    platform: &ocx_oci::Platform,
    process_env: env::Env,
    junit: Option<&crate::api::junit::Target<'_>>,
) -> anyhow::Result<ExitCode> {
    debug_assert!(
        matches!(
            tokio::runtime::Handle::current().runtime_flavor(),
            tokio::runtime::RuntimeFlavor::MultiThread
        ),
        "run_script requires the multi-thread Tokio runtime (Handle::block_on in block_in_place)"
    );

    let limits = ocx_script::ScriptLimits {
        max_callstack_size: 50,
        wall_clock: std::time::Duration::from_secs(300),
    };

    let outcome_res = tokio::task::block_in_place(|| {
        ocx_script::run_script(source, label, package_root, scratch_root, platform, process_env, limits)
    });

    let outcome = match outcome_res {
        Ok(outcome) => outcome,
        // A host abort exits 1, never an invented code 2.
        Err(error) => {
            let report = crate::api::data::script_run::ScriptRunReport::new(
                crate::api::data::script_run::ScriptStatus::Failed,
                Some(crate::api::data::script_run::AssertionRecord {
                    kind: "other".to_string(),
                    message: format!("script host failure: {error}"),
                    location: None,
                }),
                None,
            );
            context.api().report(&report)?;
            if let Some(junit) = junit {
                crate::api::junit::write(junit, &report).await?;
            }
            return Ok(ocx_exit::ExitCode::Failure.into());
        }
    };

    let run_summary = ocx_script::last_run_summary();
    let report = crate::api::data::script_run::ScriptRunReport::from_outcome(&outcome, run_summary);
    context.api().report(&report)?;
    if let Some(junit) = junit {
        crate::api::junit::write(junit, &report).await?;
    }

    Ok(map_script_outcome_to_exit_code(outcome).into())
}

/// Maps a [`ScriptOutcome`] to the process exit code, bypassing `classify_error`.
pub fn map_script_outcome_to_exit_code(outcome: ScriptOutcome) -> ocx_exit::ExitCode {
    match outcome.kind {
        ScriptOutcomeKind::Passed => ocx_exit::ExitCode::Success,
        ScriptOutcomeKind::Failed { .. } => ocx_exit::ExitCode::Failure,
        ScriptOutcomeKind::Usage { .. } => ocx_exit::ExitCode::UsageError,
        ScriptOutcomeKind::ScriptError { .. } => ocx_exit::ExitCode::DataError,
        ScriptOutcomeKind::Io { .. } => ocx_exit::ExitCode::IoError,
        ScriptOutcomeKind::Timeout => ocx_exit::ExitCode::Failure,
        // `#[non_exhaustive]`: a new kind exits 1, never silently a new code.
        _ => ocx_exit::ExitCode::Failure,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── map_script_outcome_to_exit_code — every exit-code mapping table row ──
    //
    // See `adr_package_test_scripting.md` § Exit Code Scheme. Each assertion
    // quotes the canonical numeric exit value from the contract, not derived
    // by reading the match arm — if the mapping drifts, the test catches it.
    // The exit code is the primary machine signal, so these lock it.

    fn outcome(kind: ScriptOutcomeKind) -> ScriptOutcome {
        ScriptOutcome { kind }
    }

    #[test]
    fn passed_maps_to_success_0() {
        assert_eq!(
            map_script_outcome_to_exit_code(outcome(ScriptOutcomeKind::Passed)) as u8,
            0
        );
    }

    #[test]
    fn failed_maps_to_failure_1() {
        let o = outcome(ScriptOutcomeKind::Failed {
            kind: Some(ocx_script::AssertionKind::Ok),
            message: "assertion failed".into(),
            location: None,
        });
        assert_eq!(map_script_outcome_to_exit_code(o) as u8, 1);
    }

    #[test]
    fn usage_maps_to_usage_error_64() {
        let o = outcome(ScriptOutcomeKind::Usage {
            message: "unreadable script".into(),
        });
        assert_eq!(map_script_outcome_to_exit_code(o) as u8, 64);
    }

    #[test]
    fn script_error_maps_to_data_error_65() {
        let o = outcome(ScriptOutcomeKind::ScriptError {
            message: "syntax error".into(),
            location: None,
        });
        assert_eq!(map_script_outcome_to_exit_code(o) as u8, 65);
    }

    #[test]
    fn io_maps_to_io_error_74() {
        let o = outcome(ScriptOutcomeKind::Io {
            message: "scratch I/O failed".into(),
        });
        assert_eq!(map_script_outcome_to_exit_code(o) as u8, 74);
    }

    #[test]
    fn timeout_maps_to_failure_1() {
        // Timeout maps to Failure (1), not a dedicated code.
        assert_eq!(
            map_script_outcome_to_exit_code(outcome(ScriptOutcomeKind::Timeout)) as u8,
            1
        );
    }
}
