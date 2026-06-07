// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `run-summary.json` schema — the inter-job message between `push` and `notify`.
//!
//! The summary is produced by `ocx-mirror pipeline push` and consumed by
//! `ocx-mirror pipeline notify`. It is also uploaded as a GHA workflow artifact
//! for post-run inspection.

use serde::{Deserialize, Serialize};

/// Per-version status per D12 status table.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VersionStatus {
    /// All declared platforms pushed; `latest` written iff this is newest version in run.
    Published,
    /// Some pushed, some failed; `latest` NOT written even if newest.
    Partial,
    /// None pushed.
    Failed,
    /// All declared platforms already present in the registry.
    SkippedExisting,
    /// Phase-2 placeholder: no executor for declared platform.
    SkippedExecutor,
}

/// A test failure within a `(V, P)` pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestFailure {
    /// Version string.
    pub version: String,
    /// Platform slug (e.g. `linux/amd64`).
    pub platform: String,
    /// Container ID (e.g. `ubuntu_2404` or `_native_`).
    pub container: String,
    /// Test name from `mirror.yml tests[].name`.
    pub test: String,
    /// Failure message (last N lines of stderr or timeout reason).
    pub message: String,
}

/// A platform that failed within a version's push attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformFailure {
    /// Platform slug (e.g. `darwin/amd64`).
    pub platform: String,
    /// Reason category (`test_failed`, `missing_junit`, `push_error`).
    pub reason: String,
    /// Individual test failures for this platform, if any.
    pub failed_tests: Vec<TestFailure>,
    /// URL of the GHA matrix job that produced this failure.
    ///
    /// Set when the reason originates from the per-`(version, platform, container)`
    /// test matrix (`test_failed`, `missing_junit`). Absent for failures detected
    /// in the push job itself (`push_error`, `missing_bundle`) — those are covered
    /// by the run-level `run_url`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_url: Option<String>,
}

/// A platform deliberately excluded for a version via a `broken` exclude entry.
///
/// Surfaced as a 🔒 row in the Discord notification so a known-broken
/// `(version, platform)` hole is visible rather than silently absent. Only
/// `broken`-severity excludes are recorded; `skip` excludes stay silent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExcludedPlatform {
    /// Platform slug (e.g. `windows/arm64`).
    pub platform: String,
    /// Optional reason from the spec's exclude entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Per-version outcome in the run summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionSummary {
    /// Upstream version string.
    pub version: String,
    /// Aggregate status for this version.
    pub status: VersionStatus,
    /// Platform slugs that were pushed successfully.
    pub platforms_pushed: Vec<String>,
    /// Platforms that could not be pushed with failure details.
    pub platforms_failed: Vec<PlatformFailure>,
    /// Cascade tags written for this version (e.g. `["3.29.0", "3.29", "3", "latest"]`).
    pub cascade_tags_written: Vec<String>,
    /// All test failures across all platforms for this version.
    pub test_failures: Vec<TestFailure>,
    /// Declared platforms deliberately excluded (severity `broken`) for this
    /// version — surfaced as 🔒 rows. Defaulted for backward-compatible reads.
    #[serde(default)]
    pub platforms_excluded: Vec<ExcludedPlatform>,
}

/// Top-level `run-summary.json` schema (schema_version 1).
///
/// Produced by `ocx-mirror pipeline push`, consumed by `ocx-mirror pipeline notify`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunSummary {
    /// Schema version for forward-compat detection (currently `1`).
    pub schema_version: u32,
    /// Mirror name from `mirror.yml` (e.g. `cmake`).
    pub mirror: String,
    /// Full OCI repository identifier (e.g. `ocx.sh/cmake`).
    pub target: String,
    /// URL of the GHA workflow run that produced this summary.
    pub run_url: String,
    /// URL of the push job that wrote this summary.
    ///
    /// Sourced from the `OCX_MIRROR_JOB_URL` env var the workflow exports
    /// before invoking `pipeline push`. Used to render markdown links on green
    /// rows + push-tier failures (`push_error`, `missing_bundle`) in the
    /// Discord embed. Absent when invoked outside GitHub Actions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push_job_url: Option<String>,
    /// Upstream source homepage (e.g. `https://github.com/mvdan/sh`).
    ///
    /// Derived by `pipeline push` from `mirror.yml`'s `source:` block — for
    /// `github_release` sources this is `https://github.com/{owner}/{repo}`.
    /// Used by `pipeline notify` to render a clickable `author` link on the
    /// Discord embed so readers can jump to the upstream project page.
    /// Absent for `url_index` sources (no canonical homepage to infer).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    /// Public HTTPS URL of the mirror repo's `logo.png` pinned to the commit
    /// that produced this run. Built by `pipeline push` from `GITHUB_REPOSITORY`
    /// and `GITHUB_SHA`; absent outside GHA. `pipeline notify` renders it as
    /// the embed thumbnail. Commit-pinning avoids 404s when the convention
    /// file hasn't landed on `main` yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logo_url: Option<String>,
    /// Per-version outcomes, oldest first.
    pub versions: Vec<VersionSummary>,
    /// `true` when any version status is `failed` or `partial` with test failures.
    pub any_red: bool,
    /// `true` when at least one version was pushed (status `published` or `partial`).
    pub any_new_green: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── §3.7 S7: run-summary.json schema validation ────────────────────────

    fn make_all_green_summary() -> RunSummary {
        RunSummary {
            schema_version: 1,
            mirror: "cmake".to_string(),
            target: "ocx.sh/cmake".to_string(),
            run_url: "https://github.com/ocx-sh/mirror-cmake/actions/runs/12345".to_string(),
            push_job_url: None,
            source_url: None,
            logo_url: None,
            versions: vec![VersionSummary {
                version: "3.29.0".to_string(),
                status: VersionStatus::Published,
                platforms_pushed: vec![
                    "linux/amd64".to_string(),
                    "linux/arm64".to_string(),
                    "darwin/arm64".to_string(),
                ],
                platforms_failed: vec![],
                cascade_tags_written: vec![
                    "3.29.0".to_string(),
                    "3.29".to_string(),
                    "3".to_string(),
                    "latest".to_string(),
                ],
                test_failures: vec![],
                platforms_excluded: vec![],
            }],
            any_red: false,
            any_new_green: true,
        }
    }

    fn make_partial_summary() -> RunSummary {
        RunSummary {
            schema_version: 1,
            mirror: "cmake".to_string(),
            target: "ocx.sh/cmake".to_string(),
            run_url: "https://github.com/ocx-sh/mirror-cmake/actions/runs/12345".to_string(),
            push_job_url: None,
            source_url: None,
            logo_url: None,
            versions: vec![VersionSummary {
                version: "3.28.5".to_string(),
                status: VersionStatus::Partial,
                platforms_pushed: vec!["linux/amd64".to_string()],
                platforms_failed: vec![PlatformFailure {
                    platform: "darwin/amd64".to_string(),
                    reason: "test_failed".to_string(),
                    failed_tests: vec![TestFailure {
                        version: "3.28.5".to_string(),
                        platform: "darwin/amd64".to_string(),
                        container: "_native_".to_string(),
                        test: "smoke".to_string(),
                        message: "arch -x86_64: binary not found".to_string(),
                    }],
                    job_url: None,
                }],
                cascade_tags_written: vec!["3.28.5".to_string()],
                test_failures: vec![TestFailure {
                    version: "3.28.5".to_string(),
                    platform: "darwin/amd64".to_string(),
                    container: "_native_".to_string(),
                    test: "smoke".to_string(),
                    message: "arch -x86_64: binary not found".to_string(),
                }],
                platforms_excluded: vec![],
            }],
            any_red: true,
            any_new_green: true,
        }
    }

    #[test]
    fn run_summary_schema_version_is_1() {
        // §3.7: run-summary.json schema_version must be 1
        let summary = make_all_green_summary();
        let value: serde_json::Value = serde_json::to_value(&summary).unwrap();
        assert_eq!(value["schema_version"].as_u64().unwrap(), 1);
    }

    #[test]
    fn run_summary_published_status_serializes_as_snake_case() {
        // §3.7: D12 status table: "published" in JSON
        let summary = make_all_green_summary();
        let value: serde_json::Value = serde_json::to_value(&summary).unwrap();
        assert_eq!(value["versions"][0]["status"].as_str().unwrap(), "published");
    }

    #[test]
    fn run_summary_partial_status_serializes_as_snake_case() {
        // §3.7: D12 status table: "partial" in JSON
        let summary = make_partial_summary();
        let value: serde_json::Value = serde_json::to_value(&summary).unwrap();
        assert_eq!(value["versions"][0]["status"].as_str().unwrap(), "partial");
    }

    #[test]
    fn run_summary_version_status_failed_serializes() {
        // §3.7: D12 status table: "failed" in JSON
        let status = VersionStatus::Failed;
        let value: serde_json::Value = serde_json::to_value(&status).unwrap();
        assert_eq!(value.as_str().unwrap(), "failed");
    }

    #[test]
    fn run_summary_version_status_skipped_existing_serializes() {
        // §3.7: D12 status table: "skipped_existing" in JSON
        let status = VersionStatus::SkippedExisting;
        let value: serde_json::Value = serde_json::to_value(&status).unwrap();
        assert_eq!(value.as_str().unwrap(), "skipped_existing");
    }

    #[test]
    fn run_summary_version_status_skipped_executor_serializes() {
        // §3.7: D12 status table: "skipped_executor" in JSON (phase-2 placeholder)
        let status = VersionStatus::SkippedExecutor;
        let value: serde_json::Value = serde_json::to_value(&status).unwrap();
        assert_eq!(value.as_str().unwrap(), "skipped_executor");
    }

    #[test]
    fn run_summary_any_red_any_new_green_flags_correct_for_all_green() {
        // §3.7: All green → any_red: false, any_new_green: true
        let summary = make_all_green_summary();
        let value: serde_json::Value = serde_json::to_value(&summary).unwrap();
        assert!(!value["any_red"].as_bool().unwrap());
        assert!(value["any_new_green"].as_bool().unwrap());
    }

    #[test]
    fn run_summary_any_red_true_for_partial() {
        // §3.7: Partial push → any_red: true, any_new_green: true
        let summary = make_partial_summary();
        let value: serde_json::Value = serde_json::to_value(&summary).unwrap();
        assert!(value["any_red"].as_bool().unwrap());
        assert!(value["any_new_green"].as_bool().unwrap());
    }

    #[test]
    fn run_summary_cascade_tags_latest_present_for_fully_green() {
        // §3.7: status=published with all platforms → cascade_tags_written includes "latest"
        let summary = make_all_green_summary();
        let value: serde_json::Value = serde_json::to_value(&summary).unwrap();
        let tags = value["versions"][0]["cascade_tags_written"].as_array().unwrap();
        assert!(
            tags.iter().any(|t| t.as_str() == Some("latest")),
            "published version must include 'latest' in cascade_tags_written"
        );
    }

    #[test]
    fn run_summary_cascade_tags_no_latest_for_partial() {
        // §3.7: status=partial → "latest" NOT in cascade_tags_written
        let summary = make_partial_summary();
        let value: serde_json::Value = serde_json::to_value(&summary).unwrap();
        let tags = value["versions"][0]["cascade_tags_written"].as_array().unwrap();
        assert!(
            !tags.iter().any(|t| t.as_str() == Some("latest")),
            "partial push must NOT include 'latest' in cascade_tags_written"
        );
    }

    #[test]
    fn run_summary_all_required_fields_present() {
        // §3.7: run-summary.json schema validates all required top-level fields
        let summary = make_all_green_summary();
        let value: serde_json::Value = serde_json::to_value(&summary).unwrap();

        for field in &[
            "schema_version",
            "mirror",
            "target",
            "run_url",
            "versions",
            "any_red",
            "any_new_green",
        ] {
            assert!(
                value.get(field).is_some(),
                "run-summary.json missing required field: {field}"
            );
        }
    }

    #[test]
    fn platform_failure_serde_roundtrip_omits_job_url_when_none() {
        let pf = PlatformFailure {
            platform: "linux/amd64".to_string(),
            reason: "push_error".to_string(),
            failed_tests: vec![],
            job_url: None,
        };
        let value: serde_json::Value = serde_json::to_value(&pf).unwrap();
        assert!(
            value.get("job_url").is_none(),
            "job_url must be omitted (not null) when None: {value}"
        );
    }

    #[test]
    fn platform_failure_serde_roundtrip_preserves_job_url() {
        let url = "https://github.com/ocx-sh/mirror-shfmt/actions/runs/9/job/22";
        let pf = PlatformFailure {
            platform: "linux/amd64".to_string(),
            reason: "test_failed".to_string(),
            failed_tests: vec![],
            job_url: Some(url.to_string()),
        };
        let value: serde_json::Value = serde_json::to_value(&pf).unwrap();
        assert_eq!(value["job_url"].as_str(), Some(url));

        let parsed: PlatformFailure = serde_json::from_value(value).unwrap();
        assert_eq!(parsed.job_url.as_deref(), Some(url));
    }

    #[test]
    fn platforms_excluded_defaults_to_empty_for_legacy_summaries() {
        // A run-summary.json written before this field existed must still parse;
        // `platforms_excluded` defaults to an empty vec.
        let legacy = r#"{
            "version": "3.7.0",
            "status": "published",
            "platforms_pushed": ["linux/amd64"],
            "platforms_failed": [],
            "cascade_tags_written": ["3.7.0"],
            "test_failures": []
        }"#;
        let parsed: VersionSummary = serde_json::from_str(legacy).unwrap();
        assert!(parsed.platforms_excluded.is_empty());
    }

    #[test]
    fn excluded_platform_roundtrips_with_and_without_reason() {
        let with_reason = ExcludedPlatform {
            platform: "windows/arm64".to_string(),
            reason: Some("aarch64-windows build-exe segfault".to_string()),
        };
        let value: serde_json::Value = serde_json::to_value(&with_reason).unwrap();
        assert_eq!(value["platform"].as_str(), Some("windows/arm64"));
        assert_eq!(value["reason"].as_str(), Some("aarch64-windows build-exe segfault"));
        let back: ExcludedPlatform = serde_json::from_value(value).unwrap();
        assert_eq!(back.reason.as_deref(), Some("aarch64-windows build-exe segfault"));

        let no_reason = ExcludedPlatform {
            platform: "darwin/amd64".to_string(),
            reason: None,
        };
        let value: serde_json::Value = serde_json::to_value(&no_reason).unwrap();
        assert!(
            value.get("reason").is_none(),
            "reason must be omitted when None: {value}"
        );
    }

    #[test]
    fn version_summary_all_required_fields_present() {
        // §3.7: per-version entry validates all required fields from design spec §2.4
        let summary = make_all_green_summary();
        let value: serde_json::Value = serde_json::to_value(&summary).unwrap();
        let v = &value["versions"][0];

        for field in &[
            "version",
            "status",
            "platforms_pushed",
            "platforms_failed",
            "cascade_tags_written",
            "test_failures",
        ] {
            assert!(v.get(field).is_some(), "version entry missing required field: {field}");
        }
    }
}
