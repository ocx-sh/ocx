// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Result envelope for `ocx package test --script`.
//!
//! Reported through the existing global `--format json|plain` path
//! (`Printable` + `Api::report`) — same as every other command, NOT a parallel
//! reporting path. The exit code remains the PRIMARY machine signal (R3); this
//! JSON envelope is the structured detail emitted alongside, not a substitute.
//!
//! Plain format: short human status line(s) on stdout.
//!
//! JSON format: `{"status": "...", "assertion": {...}|null, "run": {...}|null}`
//! on stdout. The envelope FIELDS are stable v1 contract; only the exact
//! human-readable *prose* of an assertion-failure message is non-stable
//! (tooling parses fields, never prose).

use serde::Serialize;

use crate::api::Printable;

/// Overall outcome of a scripted test run. Mirrors
/// `ocx_lib::script::ScriptOutcomeKind` at the OCX-facing level.
#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum ScriptStatus {
    /// Script ran to completion; all assertions passed.
    Passed,
    /// An assertion failed, `expect.fail`/`fail()`, or a host-fn failure.
    Failed,
    /// The script could not be used as given (unreadable script file).
    Usage,
    /// Syntax / arity / type error in the script source.
    ScriptError,
    /// A sandboxed filesystem operation failed for I/O reasons.
    Io,
    /// The wall-clock budget elapsed before the script finished.
    Timeout,
}

/// The terminating assertion record (when the run failed on an assertion).
///
/// `message` prose is non-stable; its presence and field shape are contractual.
#[derive(Serialize)]
pub struct AssertionRecord {
    /// Assertion kind (e.g. `ok`, `eq`, `contains`).
    pub kind: String,
    /// Failure detail. For `expect.ok`, auto-embeds the child stderr.
    pub message: String,
}

/// Surfaced `RunResult` fields for the script's terminal/top-level `ocx.run`.
#[derive(Serialize)]
pub struct RunSummary {
    /// Child exit code (or `128 + signal` when signal-killed).
    pub exit_code: i32,
    /// Captured stdout (possibly truncated).
    pub stdout: String,
    /// Captured stderr (possibly truncated).
    pub stderr: String,
    /// Wall-clock duration in milliseconds.
    pub duration_ms: u64,
    /// `true` iff stdout or stderr hit the capture cap.
    pub truncated: bool,
}

/// The `--script` result envelope.
///
/// Plain format: one human status line on stdout.
///
/// JSON format: `{"status", "assertion", "run"}` — `assertion`/`run` are
/// `null` when not applicable.
#[derive(Serialize)]
pub struct ScriptRunReport {
    /// Overall status (the structured mirror of the exit code).
    pub status: ScriptStatus,
    /// The terminating assertion record, if the run failed on an assertion.
    pub assertion: Option<AssertionRecord>,
    /// Surfaced `RunResult` fields, when a terminal `ocx.run` is reported.
    pub run: Option<RunSummary>,
}

impl ScriptRunReport {
    /// Builds the envelope from its parts.
    pub fn new(status: ScriptStatus, assertion: Option<AssertionRecord>, run: Option<RunSummary>) -> Self {
        Self { status, assertion, run }
    }

    /// Builds the envelope from an engine-neutral [`ScriptOutcome`] plus the
    /// surfaced terminal `ocx.run` result (if any).
    pub fn from_outcome(outcome: &ocx_lib::script::ScriptOutcome, run: Option<ocx_lib::script::RunSummary>) -> Self {
        use ocx_lib::script::ScriptOutcomeKind as K;
        let (status, assertion) = match &outcome.kind {
            K::Passed => (ScriptStatus::Passed, None),
            K::Failed { kind, message } => (
                ScriptStatus::Failed,
                Some(AssertionRecord {
                    // Plan C5 stable contract: the failing `expect.*` (or
                    // `fail()`) identity, surfaced verbatim so tooling can
                    // branch on *which* assertion failed without parsing the
                    // (non-stable) prose message. `None` (unattributable
                    // terminal error such as a stack overflow) → `unknown`.
                    kind: kind.map_or("unknown", |k| k.as_str()).to_string(),
                    message: message.clone(),
                }),
            ),
            K::Usage { message } => (
                ScriptStatus::Usage,
                Some(AssertionRecord {
                    kind: "usage".to_string(),
                    message: message.clone(),
                }),
            ),
            K::ScriptError { message } => (
                ScriptStatus::ScriptError,
                Some(AssertionRecord {
                    kind: "script_error".to_string(),
                    message: message.clone(),
                }),
            ),
            K::Io { message } => (
                ScriptStatus::Io,
                Some(AssertionRecord {
                    kind: "io".to_string(),
                    message: message.clone(),
                }),
            ),
            K::Timeout => (ScriptStatus::Timeout, None),
            // `ScriptOutcomeKind` is `#[non_exhaustive]` — unknown → failed.
            _ => (ScriptStatus::Failed, None),
        };
        let run = run.map(|r| RunSummary {
            exit_code: r.exit_code,
            stdout: r.stdout,
            stderr: r.stderr,
            duration_ms: r.duration_ms,
            truncated: r.truncated,
        });
        Self::new(status, assertion, run)
    }
}

impl Printable for ScriptRunReport {
    fn print_plain(&self, data: &ocx_lib::cli::DataInterface) {
        // Single-table rule: one table, status + detail columns.
        let status = match self.status {
            ScriptStatus::Passed => "passed",
            ScriptStatus::Failed => "failed",
            ScriptStatus::Usage => "usage",
            ScriptStatus::ScriptError => "script_error",
            ScriptStatus::Io => "io",
            ScriptStatus::Timeout => "timeout",
        };
        let detail = self.assertion.as_ref().map(|a| a.message.clone()).unwrap_or_default();
        data.print_table(
            &["Status".into(), "Detail".into()],
            &[vec![status.to_string().into(), detail.into()]],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── ScriptRunReport — STABLE JSON envelope shape (C6 / R3 / U21 / U22) ───
    //
    // Spec source: plan_package_test_scripting.md C6 "R3 — result envelope is
    // Printable" + U21/U22 + script_run.rs module doc. The contract is the
    // FIELD SHAPE: top-level `status` / `assertion` / `run`, the
    // `AssertionRecord` `{kind, message}` shape, and `RunSummary` fields. The
    // exact human-readable assertion *prose* is explicitly NON-stable and is
    // NOT asserted. Built from `pub` struct literals (NOT the `new()` stub) so
    // these lock the serde contract independently of the unimplemented
    // constructor; the plain renderer smoke proves the stub still panics.

    #[test]
    fn json_envelope_has_status_assertion_run_keys() {
        // U21: passed run → top-level envelope with all three keys present.
        let report = ScriptRunReport {
            status: ScriptStatus::Passed,
            assertion: None,
            run: None,
        };
        let v = serde_json::to_value(&report).expect("ScriptRunReport must serialize");
        let obj = v.as_object().expect("envelope must be a JSON object");
        assert!(obj.contains_key("status"), "envelope must carry `status`");
        assert!(obj.contains_key("assertion"), "envelope must carry `assertion`");
        assert!(obj.contains_key("run"), "envelope must carry `run`");
        assert_eq!(obj["status"], serde_json::json!("passed"));
        assert!(obj["assertion"].is_null());
        assert!(obj["run"].is_null());
    }

    #[test]
    fn status_serializes_snake_case() {
        // C6: status mirrors ScriptOutcomeKind in snake_case.
        let cases = [
            (ScriptStatus::Passed, "passed"),
            (ScriptStatus::Failed, "failed"),
            (ScriptStatus::Usage, "usage"),
            (ScriptStatus::ScriptError, "script_error"),
            (ScriptStatus::Io, "io"),
            (ScriptStatus::Timeout, "timeout"),
        ];
        for (status, expected) in cases {
            let report = ScriptRunReport {
                status,
                assertion: None,
                run: None,
            };
            let v = serde_json::to_value(&report).unwrap();
            assert_eq!(v["status"], serde_json::json!(expected));
        }
    }

    #[test]
    fn assertion_record_field_shape_is_stable() {
        // U22: failed-on-assertion envelope carries a structured assertion
        // record with `kind` + `message` fields (presence/shape stable; prose
        // deliberately not asserted verbatim).
        let report = ScriptRunReport {
            status: ScriptStatus::Failed,
            assertion: Some(AssertionRecord {
                kind: "ok".into(),
                message: "<non-stable prose>".into(),
            }),
            run: None,
        };
        let v = serde_json::to_value(&report).unwrap();
        let a = v["assertion"].as_object().expect("assertion record must be an object");
        assert!(a.contains_key("kind"), "assertion record must carry `kind`");
        assert!(a.contains_key("message"), "assertion record must carry `message`");
        assert_eq!(a["kind"], serde_json::json!("ok"));
    }

    #[test]
    fn from_outcome_threads_the_failing_assertion_kind() {
        // W7 / plan C5: `kind` reflects the actual failing `expect.*` fn, not a
        // constant "failed". `from_outcome` must carry `AssertionKind` through
        // `ScriptOutcomeKind::Failed` into the stable JSON `kind` field.
        use ocx_lib::script::{AssertionKind, ScriptOutcome, ScriptOutcomeKind};
        let cases = [
            (AssertionKind::Ok, "ok"),
            (AssertionKind::Eq, "eq"),
            (AssertionKind::Contains, "contains"),
            (AssertionKind::Matches, "matches"),
            (AssertionKind::Fail, "fail"),
            (AssertionKind::Other, "other"),
        ];
        for (kind, expected) in cases {
            let outcome = ScriptOutcome {
                kind: ScriptOutcomeKind::Failed {
                    kind: Some(kind),
                    message: "<non-stable prose>".into(),
                },
            };
            let report = ScriptRunReport::from_outcome(&outcome, None);
            let v = serde_json::to_value(&report).unwrap();
            assert_eq!(
                v["assertion"]["kind"],
                serde_json::json!(expected),
                "kind must reflect the failing expect.* fn"
            );
            assert_eq!(v["status"], serde_json::json!("failed"));
        }
    }

    #[test]
    fn from_outcome_unattributable_failure_is_unknown_kind() {
        // W7 edge: a terminal failure the engine cannot attribute to a single
        // assertion (e.g. stack overflow) → stable `kind: "unknown"`, never a
        // misleading concrete assertion name.
        use ocx_lib::script::{ScriptOutcome, ScriptOutcomeKind};
        let outcome = ScriptOutcome {
            kind: ScriptOutcomeKind::Failed {
                kind: None,
                message: "stack overflow".into(),
            },
        };
        let report = ScriptRunReport::from_outcome(&outcome, None);
        let v = serde_json::to_value(&report).unwrap();
        assert_eq!(v["assertion"]["kind"], serde_json::json!("unknown"));
    }

    #[test]
    fn run_summary_field_shape_is_stable() {
        // C6: surfaced RunResult fields — exit_code/stdout/stderr/duration_ms/
        // truncated — all present in the `run` object.
        let report = ScriptRunReport {
            status: ScriptStatus::Passed,
            assertion: None,
            run: Some(RunSummary {
                exit_code: 0,
                stdout: "v3.7.0".into(),
                stderr: String::new(),
                duration_ms: 12,
                truncated: false,
            }),
        };
        let v = serde_json::to_value(&report).unwrap();
        let r = v["run"].as_object().expect("run summary must be an object");
        for key in ["exit_code", "stdout", "stderr", "duration_ms", "truncated"] {
            assert!(r.contains_key(key), "run summary must carry `{key}`");
        }
        assert_eq!(r["exit_code"], serde_json::json!(0));
        assert_eq!(r["truncated"], serde_json::json!(false));
    }

    #[test]
    fn new_constructor_round_trips_fields() {
        // Phase 4 (Living Design Record): the Specification-phase guard
        // `new_constructor_is_a_stub` asserted the stub still panicked. Once
        // implemented that guard inverts to its real contract: `new` carries
        // its parts verbatim into the stable envelope.
        let report = ScriptRunReport::new(
            ScriptStatus::Failed,
            Some(AssertionRecord {
                kind: "failed".into(),
                message: "boom".into(),
            }),
            Some(RunSummary {
                exit_code: 2,
                stdout: "o".into(),
                stderr: "e".into(),
                duration_ms: 5,
                truncated: false,
            }),
        );
        let v = serde_json::to_value(&report).unwrap();
        assert_eq!(v["status"], serde_json::json!("failed"));
        assert_eq!(v["assertion"]["message"], serde_json::json!("boom"));
        assert_eq!(v["run"]["exit_code"], serde_json::json!(2));
    }
}
