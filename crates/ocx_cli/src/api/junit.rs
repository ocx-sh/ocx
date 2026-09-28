// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! JUnit XML sidecar for `ocx package test --script … --junit PATH`; the exit code stays the primary signal.
//!
//! One invocation writes one truncated file with one `<testcase>`, never reading an existing one,
//! so matrix jobs each write their own for a reporter to glob. `quick_junit::XmlString` also strips
//! XML 1.0's forbidden characters from script stdout, which escaping alone misses.

use std::path::Path;
use std::time::Duration;

use anyhow::Context as _;
use quick_junit::{NonSuccessKind, Report, TestCase, TestCaseStatus, TestSuite};

use crate::api::data::script_run::{ScriptRunReport, ScriptStatus};

/// Appended to a capture that hit the 10 MiB cap, so a reader sees the output stop early.
const TRUNCATION_MARKER: &str = "\n[output truncated by ocx]";

/// Where the report goes and what identifies the run inside it.
pub struct Target<'a> {
    /// Destination, exactly as `--junit` gave it.
    pub path: &'a Path,
    /// The suite name and the `classname`.
    pub identifier: &'a str,
    /// The `<testcase name=>`, so a per-platform glob merges into one suite.
    pub platform: &'a str,
}

/// Renders the run as a JUnit report.
pub fn build(target: &Target<'_>, report: &ScriptRunReport) -> Report {
    let time = Duration::from_millis(report.run.as_ref().map_or(0, |run| run.duration_ms));

    let mut case = TestCase::new(target.platform, case_status(report));
    case.set_classname(target.identifier).set_time(time);

    // GitLab links a failure to its source via `file`; `<stdin>` names nothing a reporter can open.
    if let Some(location) = report.assertion.as_ref().and_then(|a| a.location.as_ref())
        && location.file != "<stdin>"
    {
        case.extra.insert("file".into(), location.file.as_str().into());
        case.extra.insert("line".into(), location.line.to_string().into());
    }

    if let Some(run) = &report.run {
        case.set_system_out(capture(&run.stdout, run.truncated));
        case.set_system_err(capture(&run.stderr, run.truncated));
    }

    let suite_name = format!("ocx package test {}", target.identifier);
    let mut suite = TestSuite::new(suite_name.clone());
    suite.set_time(time).add_test_case(case);

    let mut report = Report::new(suite_name);
    report.set_time(time).add_test_suite(suite);
    report
}

/// Writes the report to `target.path`, creating parent directories and truncating an existing file.
///
/// # Errors
///
/// When the directory cannot be created or the file cannot be written.
pub async fn write(target: &Target<'_>, report: &ScriptRunReport) -> anyhow::Result<()> {
    let xml = build(target, report)
        .to_string()
        .context("failed to render the JUnit report")?;

    if let Some(parent) = target.path.parent().filter(|p| !p.as_os_str().is_empty()) {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| ocx_util::error::FileError::new(parent, e))
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }
    tokio::fs::write(target.path, xml)
        .await
        .map_err(|e| ocx_util::error::FileError::new(target.path, e))
        .with_context(|| format!("failed to write the JUnit report to {}", target.path.display()))
}

/// `Failed` is a test verdict (`<failure>`); any other non-pass never delivered one (`<error>`).
fn case_status(report: &ScriptRunReport) -> TestCaseStatus {
    let status_token = status_token(report.status);
    if matches!(report.status, ScriptStatus::Passed) {
        return TestCaseStatus::success();
    }

    let kind = match report.status {
        ScriptStatus::Failed => NonSuccessKind::Failure,
        _ => NonSuccessKind::Error,
    };
    let assertion = report.assertion.as_ref();
    let detail = assertion.map(|a| a.message.as_str()).unwrap_or(status_token);

    let mut status = TestCaseStatus::non_success(kind);
    status
        .set_message(first_line(detail))
        .set_type(assertion.map_or(status_token, |a| a.kind.as_str()))
        // Diagnostic alone: the captures already sit in `<system-out>`/`<system-err>`, up to 10 MiB each.
        .set_description(detail);
    status
}

/// Mirrors the JSON envelope's `status` field, so both surfaces name an outcome the same way.
fn status_token(status: ScriptStatus) -> &'static str {
    match status {
        ScriptStatus::Passed => "passed",
        ScriptStatus::Failed => "failed",
        ScriptStatus::Usage => "usage",
        ScriptStatus::ScriptError => "script_error",
        ScriptStatus::Io => "io",
        ScriptStatus::Timeout => "timeout",
    }
}

/// A captured stream, marked when the engine hit the capture cap.
fn capture(text: &str, truncated: bool) -> String {
    if truncated {
        format!("{text}{TRUNCATION_MARKER}")
    } else {
        text.to_string()
    }
}

/// First line of a diagnostic, for the `message=` attribute.
fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::data::script_run::{AssertionRecord, RunSummary, SourceLocation};

    fn target<'a>(path: &'a Path) -> Target<'a> {
        Target {
            path,
            identifier: "example.com/repo:1.2.3",
            platform: "linux/amd64",
        }
    }

    fn render(report: &ScriptRunReport) -> String {
        build(&target(Path::new("out.xml")), report)
            .to_string()
            .expect("the report must serialize")
    }

    fn run(stdout: &str, stderr: &str, truncated: bool) -> RunSummary {
        RunSummary {
            exit_code: 0,
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            duration_ms: 12,
            truncated,
        }
    }

    #[test]
    fn a_pass_is_one_suite_with_one_case_and_no_failure() {
        let xml = render(&ScriptRunReport::new(
            ScriptStatus::Passed,
            None,
            Some(run("v3.7.0", "", false)),
        ));
        assert!(
            xml.contains(r#"<testsuite name="ocx package test example.com/repo:1.2.3""#),
            "{xml}"
        );
        assert!(
            xml.contains(r#"<testcase name="linux/amd64" classname="example.com/repo:1.2.3""#),
            "the case name is the PLATFORM so a per-platform glob merges into one suite: {xml}"
        );
        assert!(!xml.contains("<failure"), "{xml}");
        assert!(!xml.contains("<error"), "{xml}");
        // The issue asks for system-out on success too.
        assert!(xml.contains("<system-out>v3.7.0</system-out>"), "{xml}");
    }

    #[test]
    fn duration_ms_renders_as_seconds_with_three_decimals() {
        let xml = render(&ScriptRunReport::new(
            ScriptStatus::Passed,
            None,
            Some(RunSummary {
                duration_ms: 1234,
                ..run("", "", false)
            }),
        ));
        assert!(xml.contains(r#"time="1.234""#), "{xml}");
    }

    #[test]
    fn a_run_less_outcome_reports_zero_time() {
        // D3: absent `run` → 0, never a missing `time` attribute (the JUnit
        // schema requires one on a testcase).
        let xml = render(&ScriptRunReport::new(ScriptStatus::Timeout, None, None));
        assert!(xml.contains(r#"time="0.000""#), "{xml}");
    }

    #[test]
    fn an_assertion_failure_is_a_failure_carrying_file_and_line() {
        let xml = render(&ScriptRunReport::new(
            ScriptStatus::Failed,
            Some(AssertionRecord {
                kind: "eq".into(),
                message: "expect.eq failed\n  left: 1\n  right: 2".into(),
                location: Some(SourceLocation {
                    file: "tests/smoke.star".into(),
                    line: 12,
                    column: 5,
                }),
            }),
            None,
        ));
        assert!(xml.contains(r#"file="tests/smoke.star""#), "{xml}");
        assert!(xml.contains(r#"line="12""#), "{xml}");
        assert!(xml.contains(r#"type="eq""#), "{xml}");
        assert!(
            xml.contains(r#"message="expect.eq failed""#),
            "the attribute is the FIRST LINE only: {xml}"
        );
        assert!(xml.contains("left: 1"), "the body carries the full diagnostic: {xml}");
    }

    #[test]
    fn the_failure_body_carries_the_diagnostic_without_re_appending_captures() {
        // H9: the captured stdout/stderr already live in
        // `<system-out>`/`<system-err>`. Re-appending them to the
        // `<failure>`/`<error>` body doubled a capture that can reach the
        // 10 MiB cap. Each distinctive marker must appear exactly once.
        let xml = render(&ScriptRunReport::new(
            ScriptStatus::Failed,
            Some(AssertionRecord {
                kind: "eq".into(),
                // Multi-line: `message=` carries only the first line
                // (`first_line`), so the second line appears ONLY in the body
                // (`set_description`) — asserting on it guards the body itself,
                // not the attribute.
                message: "expect.eq failed\n  left: 1\n  right: 2".into(),
                location: None,
            }),
            Some(run("DISTINCT_STDOUT_MARKER", "DISTINCT_STDERR_MARKER", false)),
        ));
        assert_eq!(
            xml.matches("DISTINCT_STDOUT_MARKER").count(),
            1,
            "stdout must appear once (in <system-out>), never re-appended to the body: {xml}"
        );
        assert_eq!(
            xml.matches("DISTINCT_STDERR_MARKER").count(),
            1,
            "stderr must appear once (in <system-err>), never re-appended to the body: {xml}"
        );
        // The body carries the full multi-line diagnostic. `left: 1` is on the
        // second line, so it is absent from `message=` and present only if
        // `set_description` wrote the body.
        assert!(
            xml.contains("left: 1"),
            "the <failure> body carries the full diagnostic, not just the first line: {xml}"
        );
    }

    #[test]
    fn a_stdin_script_emits_no_file_attribute() {
        // `<stdin>` names nothing a reporter can open.
        let xml = render(&ScriptRunReport::new(
            ScriptStatus::Failed,
            Some(AssertionRecord {
                kind: "ok".into(),
                message: "boom".into(),
                location: Some(SourceLocation {
                    file: "<stdin>".into(),
                    line: 3,
                    column: 1,
                }),
            }),
            None,
        ));
        assert!(!xml.contains("file="), "{xml}");
        assert!(!xml.contains("line="), "{xml}");
    }

    #[test]
    fn every_non_verdict_status_is_an_error_not_a_failure() {
        // D3: a broken or unrunnable script is not a test verdict. Timeout is
        // the case the acceptance suite cannot reach cheaply (the wall clock
        // is 300 s), so the whole table is pinned here.
        for status in [
            ScriptStatus::Usage,
            ScriptStatus::ScriptError,
            ScriptStatus::Io,
            ScriptStatus::Timeout,
        ] {
            let xml = render(&ScriptRunReport::new(status, None, None));
            assert!(xml.contains("<error"), "{status:?} must be an <error>: {xml}");
            assert!(!xml.contains("<failure"), "{status:?} must not be a <failure>: {xml}");
            assert!(
                xml.contains(&format!(r#"type="{}""#, status_token(status))),
                "a status with no assertion record types the element by its status: {xml}"
            );
            assert!(xml.contains(r#"errors="1""#), "{xml}");
        }
    }

    #[test]
    fn a_failed_status_is_a_failure_not_an_error() {
        let xml = render(&ScriptRunReport::new(ScriptStatus::Failed, None, None));
        assert!(xml.contains("<failure"), "{xml}");
        assert!(!xml.contains("<error"), "{xml}");
        assert!(xml.contains(r#"failures="1""#), "{xml}");
    }

    #[test]
    fn illegal_xml_characters_are_removed_and_markup_is_escaped() {
        // Script stdout is arbitrary bytes. `\x00`-`\x08` are illegal in XML
        // 1.0 outright — escaping them still yields a document no parser
        // accepts — and `<&>` must be escaped.
        let xml = render(&ScriptRunReport::new(
            ScriptStatus::Passed,
            None,
            Some(run("a\x00b\x08c<&>", "\x0bz", false)),
        ));
        assert!(!xml.contains('\x00'), "NUL must be removed: {xml:?}");
        assert!(!xml.contains('\x08'), "backspace must be removed: {xml:?}");
        assert!(!xml.contains('\x0b'), "vertical tab must be removed: {xml:?}");
        assert!(xml.contains("&lt;&amp;&gt;"), "markup must be escaped: {xml}");
    }

    #[test]
    fn a_truncated_capture_is_marked() {
        let xml = render(&ScriptRunReport::new(
            ScriptStatus::Passed,
            None,
            Some(run("partial", "", true)),
        ));
        assert!(xml.contains("[output truncated by ocx]"), "{xml}");
    }

    #[tokio::test]
    async fn write_creates_missing_parents_and_truncates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("build/junit/linux-amd64.xml");
        let target = Target {
            path: &path,
            identifier: "example.com/repo:1.2.3",
            platform: "linux/amd64",
        };

        write(&target, &ScriptRunReport::new(ScriptStatus::Failed, None, None))
            .await
            .expect("the parent directory must be created");
        assert!(tokio::fs::read_to_string(&path).await.unwrap().contains("<failure"));

        write(&target, &ScriptRunReport::new(ScriptStatus::Passed, None, None))
            .await
            .expect("a second run must overwrite");
        let second = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(
            !second.contains("<failure"),
            "the file is truncated, never merged: {second}"
        );
        assert_eq!(
            second.matches("<testcase").count(),
            1,
            "one invocation, one case: {second}"
        );
    }
}
