// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::cli::Cell;
use ocx_lib::oci::LayerCounts;
use serde::Serialize;

use crate::api::Printable;

/// Result of a successful `ocx package push`.
///
/// Plain format: a one-row table mirroring the JSON — `Identifier`, `Status`,
/// `Digest`, `Tags` (the rolling cascade tags, comma-joined), and `Layers`
/// (mounted/uploaded/verified counts). Keeps a plain push from being silent;
/// progress still surfaces on stderr via the log layer.
///
/// JSON format:
/// `{ "identifier", "status", "manifest_digest", "cascade_tags_written",
/// "layers": { "mounted", "uploaded", "verified" } }`. The top-level keys are
/// the machine-readable contract consumed by `ocx-mirror pipeline push`,
/// which keys its go/no-go bookkeeping off `status` and records
/// `cascade_tags_written` in the run summary; `layers` is additive.
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
    /// Counts of layer-push outcomes (mounted/uploaded/verified). Layer
    /// blobs only — the config blob and manifest are not layers.
    pub layers: LayerCounts,
}

impl PushReport {
    /// Builds a `pushed` report from the identifier, digest, cascade tags,
    /// and layer-push counts returned by the publisher.
    pub fn new(
        identifier: String,
        manifest_digest: String,
        cascade_tags_written: Vec<String>,
        layers: LayerCounts,
    ) -> Self {
        Self {
            identifier,
            status: "pushed".to_string(),
            manifest_digest,
            cascade_tags_written,
            layers,
        }
    }
}

impl Printable for PushReport {
    /// One-row table mirroring the JSON: identifier, status, digest, the
    /// rolling cascade tags (comma-joined), and the layer-push counter
    /// breakdown. Machine consumers should prefer `--format json`; this line
    /// keeps a plain push from emitting nothing.
    fn print_plain(&self, data: &ocx_lib::cli::DataInterface) {
        data.print_table(
            &[
                "Identifier".into(),
                "Status".into(),
                "Digest".into(),
                "Tags".into(),
                "Layers".into(),
            ],
            &[
                vec![Cell::from(self.identifier.clone())],
                vec![Cell::from(self.status.clone())],
                vec![Cell::from(self.manifest_digest.clone())],
                vec![Cell::from(self.cascade_tags_written.join(","))],
                vec![Cell::from(format!(
                    "mounted={},uploaded={},verified={}",
                    self.layers.mounted, self.layers.uploaded, self.layers.verified
                ))],
            ],
        );
    }
}

#[cfg(test)]
mod tests {
    use ocx_lib::cli::{DataInterface, Printer};
    use ocx_lib::oci::LayerCounts;

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
            LayerCounts::default(),
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
        let report = PushReport::new(
            "tool:1.0.0".to_string(),
            "sha256:def".to_string(),
            Vec::new(),
            LayerCounts::default(),
        );
        let value = serde_json::to_value(&report).unwrap();

        assert_eq!(
            value.get("cascade_tags_written").and_then(|v| v.as_array()),
            Some(&Vec::new())
        );
    }

    /// `layers` serializes as an additive keyed object with the three
    /// mount/upload/verify counts.
    #[test]
    fn layers_json_shape() {
        let report = PushReport::new(
            "tool:1.0.0".to_string(),
            "sha256:def".to_string(),
            Vec::new(),
            LayerCounts {
                mounted: 2,
                uploaded: 1,
                verified: 3,
            },
        );
        let value = serde_json::to_value(&report).unwrap();

        let layers = value.get("layers").expect("layers key present");
        assert_eq!(layers.get("mounted").and_then(|v| v.as_u64()), Some(2));
        assert_eq!(layers.get("uploaded").and_then(|v| v.as_u64()), Some(1));
        assert_eq!(layers.get("verified").and_then(|v| v.as_u64()), Some(3));
    }

    /// `print_plain` emits the one-row table without panicking when colour is
    /// disabled — keeps a plain push from being silent.
    #[test]
    fn print_plain_smoke() {
        let report = PushReport::new(
            "tool:1.0.0".to_string(),
            "sha256:def".to_string(),
            vec!["1".to_string(), "latest".to_string()],
            LayerCounts {
                mounted: 1,
                uploaded: 0,
                verified: 0,
            },
        );
        let data = DataInterface::new(Printer::new(false, false));
        report.print_plain(&data);
    }
}
