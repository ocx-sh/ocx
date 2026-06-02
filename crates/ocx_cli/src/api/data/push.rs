// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::cli::Cell;
use serde::Serialize;

use crate::api::Printable;

/// Result of a successful `ocx package push`.
///
/// Plain format: a one-row table mirroring the JSON — `Identifier`, `Status`,
/// `Digest`, and `Tags` (the rolling cascade tags, comma-joined). Keeps a plain
/// push from being silent; progress still surfaces on stderr via the log layer.
///
/// JSON format:
/// `{ "identifier", "status", "manifest_digest", "cascade_tags_written" }`.
/// This is the machine-readable contract consumed by `ocx-mirror pipeline
/// push`, which keys its go/no-go bookkeeping off `status` and records
/// `cascade_tags_written` in the run summary.
#[derive(Serialize)]
pub struct PushReport {
    /// The pushed package identifier (`registry/repository:tag`).
    pub identifier: String,
    /// Outcome of the push. Always `"pushed"`: the command performs the push
    /// unconditionally (the registry merge is idempotent).
    pub status: String,
    /// Digest of the pushed multi-platform image index (`sha256:...`).
    pub manifest_digest: String,
    /// Rolling cascade tags written in addition to the primary version tag
    /// (e.g. `3.28`, `3`, `latest`). Empty for a non-cascade push.
    pub cascade_tags_written: Vec<String>,
}

impl PushReport {
    /// Builds a `pushed` report from the identifier, digest, and cascade tags
    /// returned by the publisher.
    pub fn new(identifier: String, manifest_digest: String, cascade_tags_written: Vec<String>) -> Self {
        Self {
            identifier,
            status: "pushed".to_string(),
            manifest_digest,
            cascade_tags_written,
        }
    }
}

impl Printable for PushReport {
    /// One-row table mirroring the JSON: identifier, status, digest, and the
    /// rolling cascade tags (comma-joined). Machine consumers should prefer
    /// `--format json`; this line keeps a plain push from emitting nothing.
    fn print_plain(&self, data: &ocx_lib::cli::DataInterface) {
        data.print_table(
            &["Identifier".into(), "Status".into(), "Digest".into(), "Tags".into()],
            &[
                vec![Cell::from(self.identifier.clone())],
                vec![Cell::from(self.status.clone())],
                vec![Cell::from(self.manifest_digest.clone())],
                vec![Cell::from(self.cascade_tags_written.join(","))],
            ],
        );
    }
}

#[cfg(test)]
mod tests {
    use ocx_lib::cli::{DataInterface, Printer};

    use super::PushReport;
    use crate::api::Printable as _;

    /// Pins the JSON wire format consumed by `ocx-mirror pipeline push`: the
    /// four keys (`identifier`, `status`, `manifest_digest`,
    /// `cascade_tags_written`) and the constant `"pushed"` status. The mirror
    /// parser keys its go/no-go bookkeeping off these names.
    #[test]
    fn cascade_report_json_shape() {
        let report = PushReport::new(
            "registry.example/tool:3.28.1".to_string(),
            "sha256:abc".to_string(),
            vec!["3.28".to_string(), "3".to_string(), "latest".to_string()],
        );
        let value = serde_json::to_value(&report).unwrap();

        assert_eq!(
            value.get("identifier").and_then(|v| v.as_str()),
            Some("registry.example/tool:3.28.1")
        );
        assert_eq!(value.get("status").and_then(|v| v.as_str()), Some("pushed"));
        assert_eq!(
            value.get("manifest_digest").and_then(|v| v.as_str()),
            Some("sha256:abc")
        );
        assert_eq!(
            value.get("cascade_tags_written").and_then(|v| v.as_array()),
            Some(&vec!["3.28".into(), "3".into(), "latest".into()])
        );
    }

    /// A non-cascade push writes no rolling tags: `cascade_tags_written` must
    /// serialize as an empty array, not be absent or null.
    #[test]
    fn non_cascade_report_has_empty_tags() {
        let report = PushReport::new("tool:1.0.0".to_string(), "sha256:def".to_string(), Vec::new());
        let value = serde_json::to_value(&report).unwrap();

        assert_eq!(
            value.get("cascade_tags_written").and_then(|v| v.as_array()),
            Some(&Vec::new())
        );
    }

    /// `print_plain` emits the one-row table without panicking when colour is
    /// disabled — keeps a plain push from being silent.
    #[test]
    fn print_plain_smoke() {
        let report = PushReport::new(
            "tool:1.0.0".to_string(),
            "sha256:def".to_string(),
            vec!["1".to_string(), "latest".to_string()],
        );
        let data = DataInterface::new(Printer::new(false, false));
        report.print_plain(&data);
    }
}
