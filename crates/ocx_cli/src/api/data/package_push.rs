// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Report data for `ocx package push --cascade --format json`.
//!
//! Plain format: single-line status message with manifest digest.
//!
//! JSON format:
//! ```json
//! {
//!   "manifest_digest": "sha256:abc...",
//!   "cascade_tags_written": ["3.29.0", "3.29", "3", "latest"],
//!   "status": "pushed"
//! }
//! ```

use std::fmt;

use ocx_lib::cli::Printer;
use serde::Serialize;

use crate::api::Printable;

/// Outcome of a single `ocx package push` invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PushStatus {
    /// The package was successfully pushed (new or updated).
    Pushed,
    /// The package was already present at the registry; no bytes transferred.
    // Constructed by the push command when the registry reports a digest match.
    #[allow(dead_code)]
    SkippedExisting,
}

impl fmt::Display for PushStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pushed => write!(f, "pushed"),
            Self::SkippedExisting => write!(f, "skipped (already exists)"),
        }
    }
}

/// Structured result of `ocx package push`, including cascade tags written.
///
/// Consumed by `ocx-mirror push` via `--format json` to build `run-summary.json`.
#[derive(Debug, Clone, Serialize)]
pub struct PushReport {
    /// OCI manifest digest of the pushed or existing package (e.g. `sha256:abc…`).
    pub manifest_digest: String,
    /// All cascade tags written in addition to the primary version tag
    /// (e.g. `["3.29", "3", "latest"]`).
    pub cascade_tags_written: Vec<String>,
    /// Whether the push was a new upload or a no-op (already present).
    pub status: PushStatus,
}

impl PushReport {
    /// Build a [`PushReport`] from the raw cascade result.
    ///
    /// `manifest_digest` and `cascade_tags_written` come from
    /// `publisher::push_cascade`; `status` is set by the caller based on
    /// whether the push was a new upload or a skip.
    pub fn new(manifest_digest: String, cascade_tags_written: Vec<String>, status: PushStatus) -> Self {
        Self {
            manifest_digest,
            cascade_tags_written,
            status,
        }
    }
}

impl Printable for PushReport {
    fn print_plain(&self, printer: &Printer) {
        let tags = if self.cascade_tags_written.is_empty() {
            "(none)".to_string()
        } else {
            self.cascade_tags_written.join(", ")
        };
        printer.print_table(
            &["Digest", "Cascade Tags", "Status"],
            &[
                vec![self.manifest_digest.clone()],
                vec![tags],
                vec![self.status.to_string()],
            ],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // §3.2 S2: push JSON output — unit tests for PushReport schema

    #[test]
    fn push_report_serializes_manifest_digest() {
        // §3.2: Printable::print_json emits parseable JSON with manifest_digest field
        let report = PushReport::new(
            "sha256:abc123def456".to_string(),
            vec![
                "3.29.0".to_string(),
                "3.29".to_string(),
                "3".to_string(),
                "latest".to_string(),
            ],
            PushStatus::Pushed,
        );

        let value: serde_json::Value = serde_json::to_value(&report).unwrap();
        assert_eq!(
            value["manifest_digest"], "sha256:abc123def456",
            "manifest_digest must be present in JSON output"
        );
    }

    #[test]
    fn push_report_serializes_cascade_tags_written() {
        // §3.2: Schema: cascade_tags_written is array of strings
        let tags = vec![
            "3.29.0".to_string(),
            "3.29".to_string(),
            "3".to_string(),
            "latest".to_string(),
        ];
        let report = PushReport::new("sha256:abc123".to_string(), tags.clone(), PushStatus::Pushed);

        let value: serde_json::Value = serde_json::to_value(&report).unwrap();
        let arr = value["cascade_tags_written"].as_array().unwrap();
        assert_eq!(arr.len(), 4);
        assert_eq!(arr[0].as_str().unwrap(), "3.29.0");
        assert_eq!(arr[3].as_str().unwrap(), "latest");
    }

    #[test]
    fn push_report_empty_cascade_tags_for_non_cascade_push() {
        // §3.2: cascade_tags_written is empty array for non-cascade push
        let report = PushReport::new("sha256:def789".to_string(), vec![], PushStatus::Pushed);

        let value: serde_json::Value = serde_json::to_value(&report).unwrap();
        let arr = value["cascade_tags_written"].as_array().unwrap();
        assert!(
            arr.is_empty(),
            "cascade_tags_written must be empty array for non-cascade push"
        );
    }

    #[test]
    fn push_status_pushed_serializes_as_snake_case() {
        // §3.2: status is "pushed" (lowercase snake_case)
        let report = PushReport::new("sha256:abc".to_string(), vec![], PushStatus::Pushed);
        let value: serde_json::Value = serde_json::to_value(&report).unwrap();
        assert_eq!(
            value["status"].as_str().unwrap(),
            "pushed",
            "PushStatus::Pushed must serialize as 'pushed'"
        );
    }

    #[test]
    fn push_status_skipped_existing_serializes_as_snake_case() {
        // §3.2: status is "skipped_existing" (lowercase snake_case)
        let report = PushReport::new("sha256:abc".to_string(), vec![], PushStatus::SkippedExisting);
        let value: serde_json::Value = serde_json::to_value(&report).unwrap();
        assert_eq!(
            value["status"].as_str().unwrap(),
            "skipped_existing",
            "PushStatus::SkippedExisting must serialize as 'skipped_existing'"
        );
    }

    #[test]
    fn push_report_json_has_all_required_fields() {
        // §3.2: JSON output has all three required fields per design spec §2.4
        let report = PushReport::new(
            "sha256:000111222333".to_string(),
            vec!["1.0.0".to_string()],
            PushStatus::Pushed,
        );
        let value: serde_json::Value = serde_json::to_value(&report).unwrap();

        assert!(value.get("manifest_digest").is_some(), "missing manifest_digest field");
        assert!(
            value.get("cascade_tags_written").is_some(),
            "missing cascade_tags_written field"
        );
        assert!(value.get("status").is_some(), "missing status field");
    }
}
