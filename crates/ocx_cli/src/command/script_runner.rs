// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Shared Starlark-script runner for the `test` commands.
//!
//! Both `ocx package test --script` and `ocx patch test --script` compose an
//! environment, then interpret a Starlark script in it and map the structured
//! outcome to a process exit code. This module owns that shared tail so the two
//! commands cannot drift: [`run_script_in_env`] runs the engine and emits the
//! report, and [`map_script_outcome_to_exit_code`] performs the outcome→exit
//! mapping. Each caller keeps its own source-resolution and tempdir lifecycle.

use std::process::ExitCode;

use ocx_config::env;
use ocx_script::{ScriptOutcome, ScriptOutcomeKind};

/// Run a Starlark script in an already-composed environment and report it.
///
/// The engine runs on the current Tokio worker via `block_in_place` (the
/// evaluator is `!Send`), so the caller MUST be on the multi-thread runtime.
/// `package_root` is the read-only package the script inspects; `scratch_root`
/// is the read-write sandbox the engine may write to (both created by the
/// caller). `process_env` is consumed as the script's process environment.
///
/// Returns `Ok(ExitCode)` so the command bypasses `main.rs::classify_error`
/// (ADR Exit Code Scheme): a host setup/abort failure is reported as a `Failed`
/// outcome and mapped to `Failure` (1). The structured `ScriptRunReport` is
/// emitted through the context `Api` in every non-host-failure case.
///
/// `junit` requests a JUnit XML sidecar beside the JSON envelope — written on
/// every outcome, including the host-setup failure. `ocx patch test` passes
/// `None`.
///
/// # Errors
///
/// Returns an error if emitting the structured report fails, or if a requested
/// JUnit report cannot be written.
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
    // PRECONDITION: run_script is sync and the ocx.run host fn uses
    // Handle::block_on inside block_in_place — correct ONLY on Tokio's
    // multi-thread runtime. main.rs uses the default multi-thread
    // #[tokio::main]; assert it so a future switch to current_thread fails
    // loudly in debug builds.
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

    // run_script is SYNC (Evaluator is !Send): invoke via block_in_place, NOT
    // .await / spawn_blocking.
    let outcome_res = tokio::task::block_in_place(|| {
        ocx_script::run_script(source, label, package_root, scratch_root, platform, process_env, limits)
    });

    let outcome = match outcome_res {
        Ok(outcome) => outcome,
        // Host setup/abort (unrecoverable) → Failure(1); never code 2.
        Err(error) => {
            let report = crate::api::data::script_run::ScriptRunReport::new(
                crate::api::data::script_run::ScriptStatus::Failed,
                Some(crate::api::data::script_run::AssertionRecord {
                    // Pre/post-engine host abort: not an `expect.*` assertion —
                    // categorize as the non-assertion kind.
                    kind: "other".to_string(),
                    message: format!("script host failure: {error}"),
                    // No Starlark error stands behind a host setup failure.
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

    // Emit the structured envelope through the existing report path; the exit
    // code stays authoritative (both emitted).
    let run_summary = ocx_script::last_run_summary();
    let report = crate::api::data::script_run::ScriptRunReport::from_outcome(&outcome, run_summary);
    context.api().report(&report)?;
    if let Some(junit) = junit {
        crate::api::junit::write(junit, &report).await?;
    }

    Ok(map_script_outcome_to_exit_code(outcome).into())
}

/// Maps an engine-neutral [`ScriptOutcome`] to the process exit code.
///
/// The returned `ocx_exit::ExitCode` bypasses `main.rs::classify_error` (ADR Exit
/// Code Scheme). Engine-internal errors map to `Failure` (1) — no code `2` is
/// invented.
pub fn map_script_outcome_to_exit_code(outcome: ScriptOutcome) -> ocx_exit::ExitCode {
    match outcome.kind {
        ScriptOutcomeKind::Passed => ocx_exit::ExitCode::Success,
        ScriptOutcomeKind::Failed { .. } => ocx_exit::ExitCode::Failure,
        ScriptOutcomeKind::Usage { .. } => ocx_exit::ExitCode::UsageError,
        ScriptOutcomeKind::ScriptError { .. } => ocx_exit::ExitCode::DataError,
        ScriptOutcomeKind::Io { .. } => ocx_exit::ExitCode::IoError,
        ScriptOutcomeKind::Timeout => ocx_exit::ExitCode::Failure,
        // `ScriptOutcomeKind` is `#[non_exhaustive]` — unmapped kinds fall to
        // generic failure (locked here, never silently to a new code).
        _ => ocx_exit::ExitCode::Failure,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── map_script_outcome_to_exit_code — every C8 table row ─────────────────
    //
    // Spec source: plan_package_test_scripting.md C8 exit-code mapping table +
    // Error Taxonomy. Each assertion quotes the canonical numeric exit value
    // from the contract, NOT derived by reading the match arm — if the mapping
    // drifts, the test catches it. The fn is pure; these lock the published
    // exit-code contract (the PRIMARY machine signal per R3).

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
        // C8: Timeout → Failure (1), NOT a dedicated code.
        assert_eq!(
            map_script_outcome_to_exit_code(outcome(ScriptOutcomeKind::Timeout)) as u8,
            1
        );
    }
}
