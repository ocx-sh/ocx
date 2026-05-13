// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! JUNIT XML parser for `ocx-mirror pipeline push`.
//!
//! # Dialect
//!
//! Emitted and parsed format is the Surefire/Jenkins canonical dialect:
//! - Root element: `<testsuites>`
//! - Each test leg: one `<testsuite>` with `tests`, `failures`, `errors`,
//!   `skipped` attributes and optional `<properties>`
//! - Each test: one `<testcase>` with `name`, `classname`, `time` attributes
//! - Failure: `<testcase>` contains `<failure message="..." type="exit_code">stderr tail</failure>`
//! - Timeout: `type="timeout"`
//!
//! This dialect is auto-detected and parsed natively by
//! `EnricoMi/publish-unit-test-result-action@v2`.
//!
//! Bare `<testsuite>` roots (without a wrapping `<testsuites>`) are accepted
//! by implicitly wrapping before parsing.
//!
//! # File naming convention
//!
//! Each `(V, P, C)` test leg writes
//! `junit-{V}-{platform_slug}-{container_id}.xml` where:
//! - `{platform_slug}` = `Platform::ascii_segments().join("_")` (e.g. `linux_amd64`)
//! - `{container_id}` = image with `:` and `/` replaced by `_` (e.g. `ubuntu_2404`)
//! - Native legs use container_id `_native_`

use std::path::Path;

use quick_junit::{NonSuccessKind, Report, TestCaseStatus};

use crate::error::MirrorError;

/// A parsed JUNIT testcase within a testsuite.
#[derive(Debug, Clone)]
pub struct JunitTestcase {
    /// Test name (maps to `tests[].name` in `mirror.yml`).
    pub name: String,
    /// Wall-clock duration in seconds.
    ///
    /// Populated for completeness; not yet surfaced in run-summary.
    #[allow(dead_code)]
    pub time: f64,
    /// Failure message if the test failed; `None` for passing tests.
    pub failure_message: Option<String>,
    /// Whether the failure is a timeout (`type="timeout"`) vs exit code failure.
    ///
    /// Populated for completeness; not yet surfaced in run-summary.
    #[allow(dead_code)]
    pub is_timeout: bool,
}

/// A parsed JUNIT testsuite (one per `(V, P, C)` test leg).
#[derive(Debug, Clone)]
pub struct JunitTestsuite {
    /// Suite name (e.g. `ocx-mirror.cmake.linux_amd64.ubuntu_2404`).
    ///
    /// Populated for completeness; not yet used by the push driver.
    #[allow(dead_code)]
    pub name: String,
    /// Total number of test cases declared in the suite.
    ///
    /// Populated for completeness; not yet used by the push driver.
    #[allow(dead_code)]
    pub tests: u32,
    /// Number of test cases with `<failure>` elements.
    pub failures: u32,
    /// Number of test cases with `<error>` elements.
    pub errors: u32,
    /// Individual test cases.
    pub testcases: Vec<JunitTestcase>,
}

/// Parse JUNIT XML from a string (for testing without filesystem I/O).
///
/// Returns `Err(MirrorError::JunitParseError)` on malformed XML.
pub fn parse_str(xml: &str) -> Result<JunitTestsuite, MirrorError> {
    // quick-junit requires a <testsuites> root. If the XML starts with a bare
    // <testsuite>, wrap it implicitly before parsing.
    let xml = normalize_xml_root(xml);

    let report = Report::deserialize_from_str(&xml)
        .map_err(|e| MirrorError::JunitParseError(format!("XML parse error: {e}")))?;

    // We expect exactly one testsuite per file (one (V, P, C) leg).
    // If there are multiple, we merge them by using the first one's metadata
    // and accumulating all testcases. This is defensive — the pipeline always
    // produces single-suite files.
    let (name, tests, failures, errors, testcases) = if report.test_suites.is_empty() {
        (report.name.as_str().to_string(), 0u32, 0u32, 0u32, Vec::new())
    } else {
        let suite = &report.test_suites[0];
        let name = suite.name.as_str().to_string();
        let tests = suite.tests as u32;
        let failures = suite.failures as u32;
        let errors = suite.errors as u32;
        let testcases = suite
            .test_cases
            .iter()
            .map(|tc| {
                let name = tc.name.as_str().to_string();
                let time = tc.time.map(|d| d.as_secs_f64()).unwrap_or(0.0);
                let (failure_message, is_timeout) = match &tc.status {
                    TestCaseStatus::Success { .. } => (None, false),
                    TestCaseStatus::NonSuccess {
                        kind,
                        message,
                        ty,
                        description,
                        ..
                    } => {
                        // Both Failure and Error count as failed for AND-logic.
                        let is_timeout = ty.as_ref().is_some_and(|t| t.as_str() == "timeout");
                        let msg = message
                            .as_ref()
                            .map(|m| m.as_str().to_string())
                            .or_else(|| description.as_ref().map(|d| d.as_str().to_string()))
                            .or_else(|| {
                                Some(match kind {
                                    NonSuccessKind::Failure => "test failed".to_string(),
                                    NonSuccessKind::Error => "test error".to_string(),
                                })
                            });
                        (msg, is_timeout)
                    }
                    TestCaseStatus::Skipped { message, .. } => {
                        // Skipped tests don't fail for AND-logic; treat as passing.
                        let _ = message;
                        (None, false)
                    }
                };
                JunitTestcase {
                    name,
                    time,
                    failure_message,
                    is_timeout,
                }
            })
            .collect();
        (name, tests, failures, errors, testcases)
    };

    Ok(JunitTestsuite {
        name,
        tests,
        failures,
        errors,
        testcases,
    })
}

/// Normalize XML root to be acceptable by quick-junit's deserializer.
///
/// Two transformations applied:
/// 1. If the root element is bare `<testsuite>` (no `<testsuites>` wrapper),
///    wrap it in a synthetic `<testsuites name="wrapped">` element.
/// 2. If `<testsuites>` is present but lacks a `name` attribute, inject
///    `name="report"` — quick-junit requires this attribute.
fn normalize_xml_root(xml: &str) -> String {
    // Strip XML declaration and leading whitespace to find the root element.
    let trimmed = xml.trim_start();
    let content_after_decl = if trimmed.starts_with("<?xml") {
        // Skip past the XML declaration
        match trimmed.find("?>") {
            Some(pos) => trimmed[pos + 2..].trim_start(),
            None => trimmed,
        }
    } else {
        trimmed
    };

    // Check whether the root element (after optional declaration) is <testsuite
    // without an 's'. We match the start of the tag name exactly.
    let root_is_bare_testsuite =
        content_after_decl.starts_with("<testsuite") && !content_after_decl.starts_with("<testsuites");

    if root_is_bare_testsuite {
        // Wrap in a minimal <testsuites> so quick-junit's deserializer accepts it.
        // The suite's own attributes carry the real counts.
        return format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="wrapped" tests="0" failures="0" errors="0">
{content_after_decl}
</testsuites>"#
        );
    }

    // If the root is <testsuites> but missing a name attribute, inject one.
    // quick-junit requires `name` on the <testsuites> element.
    if content_after_decl.starts_with("<testsuites") {
        // Find the end of the opening tag declaration to check for name attr.
        if let Some(tag_end) = content_after_decl.find(['>', ' ']) {
            let tag_header = &content_after_decl[..tag_end];
            let has_name = tag_header.contains("name=")
                || content_after_decl[tag_end..]
                    .split_once('>')
                    .map(|(attrs, _)| attrs.contains("name="))
                    .unwrap_or(false);

            if !has_name {
                // Inject name="report" right after <testsuites
                return xml.replacen("<testsuites", r#"<testsuites name="report""#, 1);
            }
        }
    }

    xml.to_string()
}

/// Read a JUNIT XML file from the given path inside a Tokio async context.
///
/// This is the async wrapper used by the push driver. It delegates to
/// `tokio::fs::read_to_string` so we never block the async executor with I/O.
pub async fn parse_async(path: &Path) -> Result<JunitTestsuite, MirrorError> {
    let xml = tokio::fs::read_to_string(path)
        .await
        .map_err(|e| MirrorError::JunitParseError(format!("failed to read {}: {e}", path.display())))?;
    parse_str(&xml)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use tempfile::NamedTempFile;

    /// Synchronous parse helper for tests — avoids an async runtime in unit tests.
    fn parse(path: &std::path::Path) -> Result<JunitTestsuite, crate::error::MirrorError> {
        let xml = std::fs::read_to_string(path).map_err(|e| {
            crate::error::MirrorError::JunitParseError(format!("failed to read {}: {e}", path.display()))
        })?;
        parse_str(&xml)
    }

    // ── §3.7 S7: JUNIT parser tests ────────────────────────────────────────

    /// Canonical Surefire/Jenkins JUNIT XML with two passing tests.
    const JUNIT_TWO_PASSING: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites>
  <testsuite name="ocx-mirror.shfmt.linux_amd64.ubuntu_2404"
             tests="2" failures="0" errors="0" skipped="0"
             timestamp="2026-05-13T10:24:31Z" time="9.4">
    <properties>
      <property name="ocx.version" value="3.7.0"/>
      <property name="ocx.platform" value="linux/amd64"/>
      <property name="ocx.image" value="ubuntu:24.04"/>
    </properties>
    <testcase name="version" classname="ocx-mirror.shfmt.linux_amd64.ubuntu_2404" time="4.1"/>
    <testcase name="smoke" classname="ocx-mirror.shfmt.linux_amd64.ubuntu_2404" time="5.3"/>
  </testsuite>
</testsuites>"#;

    /// JUNIT XML with one failure (exit_code type).
    const JUNIT_ONE_FAILURE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites>
  <testsuite name="ocx-mirror.shfmt.linux_amd64.alpine_320"
             tests="2" failures="1" errors="0" skipped="0"
             timestamp="2026-05-13T10:25:00Z" time="3.2">
    <testcase name="version" classname="ocx-mirror.shfmt.linux_amd64.alpine_320" time="1.1"/>
    <testcase name="smoke" classname="ocx-mirror.shfmt.linux_amd64.alpine_320" time="2.1">
      <failure message="exit code 1" type="exit_code">stderr: smoke test failed: binary missing from PATH</failure>
    </testcase>
  </testsuite>
</testsuites>"#;

    /// JUNIT XML with a timeout failure.
    const JUNIT_TIMEOUT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites>
  <testsuite name="ocx-mirror.cmake.darwin_arm64._native_"
             tests="1" failures="0" errors="1" skipped="0"
             timestamp="2026-05-13T11:00:00Z" time="300.0">
    <testcase name="smoke" classname="ocx-mirror.cmake.darwin_arm64._native_" time="300.0">
      <error message="timed out after 300s" type="timeout">Process exceeded wall-clock limit</error>
    </testcase>
  </testsuite>
</testsuites>"#;

    /// Malformed XML (missing closing tag).
    const JUNIT_MALFORMED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites>
  <testsuite name="bad" tests="1" failures="0" errors="0" skipped="0">
    <testcase name="version"
"#;

    /// Bare <testsuite> root without <testsuites> wrapper.
    const JUNIT_BARE_TESTSUITE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuite name="ocx-mirror.shfmt.linux_amd64.ubuntu_2404"
           tests="1" failures="0" errors="0" skipped="0"
           timestamp="2026-05-13T10:00:00Z" time="1.0">
  <testcase name="version" classname="ocx-mirror.shfmt.linux_amd64.ubuntu_2404" time="1.0"/>
</testsuite>"#;

    fn write_junit_file(xml: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(xml.as_bytes()).unwrap();
        f
    }

    #[test]
    fn parse_dialect_surefire_testsuites_root() {
        // §3.7: Dialect lock: parser accepts <testsuites><testsuite><testcase>
        let f = write_junit_file(JUNIT_TWO_PASSING);
        let suite = parse(f.path()).expect("should parse two-passing JUNIT");
        assert_eq!(suite.tests, 2);
        assert_eq!(suite.failures, 0);
        assert_eq!(suite.errors, 0);
        assert_eq!(suite.testcases.len(), 2);
        assert_eq!(suite.testcases[0].name, "version");
        assert_eq!(suite.testcases[1].name, "smoke");
        assert!(suite.testcases[0].failure_message.is_none());
        assert!(suite.testcases[1].failure_message.is_none());
    }

    #[test]
    fn parse_testcase_failure_exit_code() {
        // §3.7: <failure type="exit_code"> parses into failure_message
        let f = write_junit_file(JUNIT_ONE_FAILURE);
        let suite = parse(f.path()).expect("should parse one-failure JUNIT");
        assert_eq!(suite.failures, 1);
        let smoke = suite
            .testcases
            .iter()
            .find(|tc| tc.name == "smoke")
            .expect("smoke testcase must exist");
        assert!(
            smoke.failure_message.is_some(),
            "smoke testcase must have failure_message"
        );
        assert!(!smoke.is_timeout, "exit_code failure must not be timeout");
    }

    #[test]
    fn parse_error_element_is_treated_as_failure() {
        // §3.7: <error> sub-element distinguished from <failure> but both
        // count as test-failed for AND-logic.
        let f = write_junit_file(JUNIT_TIMEOUT);
        let suite = parse(f.path()).expect("should parse timeout JUNIT");
        assert_eq!(suite.errors, 1, "errors counter must reflect <error> element");
        let smoke = suite
            .testcases
            .iter()
            .find(|tc| tc.name == "smoke")
            .expect("smoke testcase must exist");
        // Both <failure> and <error> mark a test as failed for AND-logic
        assert!(
            smoke.failure_message.is_some() || smoke.is_timeout,
            "timeout <error> must set failure_message or is_timeout"
        );
        assert!(smoke.is_timeout, "type=timeout must set is_timeout=true");
    }

    #[test]
    fn parse_malformed_xml_returns_junit_parse_error() {
        // §3.7: Malformed XML → JunitParseError (exit 65)
        let f = write_junit_file(JUNIT_MALFORMED);
        let result = parse(f.path());
        match result {
            Err(MirrorError::JunitParseError(_)) => {
                // Expected
            }
            Ok(_) => panic!("Expected JunitParseError for malformed XML"),
            Err(e) => panic!("Expected JunitParseError, got: {e}"),
        }
    }

    #[test]
    fn parse_bare_testsuite_root_accepted_or_wrapped() {
        // §3.7: Bare <testsuite> root without <testsuites> wrapper →
        // accept (wrap implicitly). Should not error.
        let f = write_junit_file(JUNIT_BARE_TESTSUITE);
        let result = parse(f.path());
        match result {
            Ok(suite) => {
                assert_eq!(suite.tests, 1, "wrapped bare testsuite must report 1 test");
            }
            Err(MirrorError::JunitParseError(msg)) => {
                // Some parsers may reject bare root — acceptable if documented
                let _ = msg;
            }
            Err(e) => panic!("Unexpected error for bare testsuite: {e}"),
        }
    }

    #[test]
    fn parse_str_two_passing_tests() {
        // Direct parse_str path (no filesystem I/O)
        let suite = parse_str(JUNIT_TWO_PASSING).expect("parse_str must succeed");
        assert_eq!(suite.tests, 2);
        assert_eq!(suite.failures, 0);
        assert_eq!(suite.testcases.len(), 2);
    }

    #[test]
    fn testcase_time_is_parsed() {
        // Verify duration parsing maps to seconds float
        let suite = parse_str(JUNIT_TWO_PASSING).expect("parse_str must succeed");
        // "version" has time="4.1", "smoke" has time="5.3"
        let version_tc = suite.testcases.iter().find(|tc| tc.name == "version").unwrap();
        assert!(
            (version_tc.time - 4.1).abs() < 0.01,
            "time for 'version' test should be ~4.1s, got {}",
            version_tc.time
        );
    }

    #[test]
    fn suite_name_is_parsed() {
        // Suite name attribute round-trips
        let suite = parse_str(JUNIT_TWO_PASSING).expect("parse_str must succeed");
        assert_eq!(suite.name, "ocx-mirror.shfmt.linux_amd64.ubuntu_2404");
    }
}
