// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::{cli::Cell, publisher::PushOutcome};
use serde::Serialize;

use crate::api::Printable;

/// Result of a successful `ocx package push`.
///
/// Plain format: a one-row table — `Identifier`, `Digest`, `Tags` (the rolling
/// cascade tags, comma-joined) and `Canonical Tags` (how many were written).
/// Keeps a plain push from being silent; progress still surfaces on stderr via
/// the log layer. `status` is plain-omitted (it is the constant `"pushed"`) and
/// the canonical tags are counted rather than listed — each is a 71-column
/// `sha256.<hex>` and there is one per distinct platform manifest.
///
/// JSON format:
/// `{ "identifier", "status", "manifest_digest", "cascade_tags_written",
/// "canonical_tags_written" }`.
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
    /// Digest-named `sha256.<hex>` tags this push wrote, in push order, one
    /// per distinct platform manifest — platforms whose manifest is identical
    /// share a single tag, so this does not zip against the pushed platform
    /// list. Empty under `--no-canonical-tag`. Reports what reached the
    /// registry, not what was requested.
    pub canonical_tags_written: Vec<String>,
}

impl PushReport {
    /// Builds a `pushed` report for `identifier` from the publisher's outcome.
    ///
    /// Takes the whole [`PushOutcome`] rather than its fields: the cascade and
    /// canonical tags are both `Vec<String>`, so as adjacent positionals a
    /// swapped pair would type-check silently and publish `sha256.<hex>`
    /// values under `cascade_tags_written`.
    pub fn from_outcome(identifier: String, outcome: PushOutcome) -> Self {
        Self {
            identifier,
            status: "pushed".to_string(),
            manifest_digest: outcome.manifest_digest.to_string(),
            cascade_tags_written: outcome.cascade_tags,
            canonical_tags_written: outcome.canonical_tags,
        }
    }
}

impl Printable for PushReport {
    /// One-row table: identifier, digest, the rolling cascade tags, and the
    /// number of canonical tags written. Machine consumers should prefer
    /// `--format json`; this line keeps a plain push from emitting nothing.
    ///
    /// `status` has no column because it is always `"pushed"`, and the
    /// canonical tags are a count because listing them is 71 columns each.
    /// Both stay in the JSON contract, where `ocx-mirror` reads them.
    fn print_plain(&self, data: &ocx_lib::cli::DataInterface) {
        data.print_table(
            &[
                "Identifier".into(),
                "Digest".into(),
                "Tags".into(),
                "Canonical Tags".into(),
            ],
            &[
                vec![Cell::from(self.identifier.clone())],
                vec![Cell::from(self.manifest_digest.clone())],
                vec![Cell::from(self.cascade_tags_written.join(","))],
                vec![Cell::from(self.canonical_tags_written.len().to_string())],
            ],
        );
    }
}

#[cfg(test)]
mod tests {
    use ocx_lib::{
        cli::{DataInterface, Printer},
        oci,
        publisher::PushOutcome,
    };

    use super::PushReport;
    use crate::api::Printable as _;

    fn outcome(digest_hex: &str, cascade_tags: Vec<String>, canonical_tags: Vec<String>) -> PushOutcome {
        PushOutcome {
            manifest_digest: oci::Digest::try_from(format!("sha256:{}", digest_hex.repeat(64)).as_str())
                .expect("digest parses"),
            cascade_tags,
            canonical_tags,
        }
    }

    /// Pins the JSON wire format consumed by `ocx-mirror pipeline push`: the
    /// five keys (`identifier`, `status`, `manifest_digest`,
    /// `cascade_tags_written`, `canonical_tags_written`) and the constant
    /// `"pushed"` status. The mirror parser keys its go/no-go bookkeeping off
    /// these names.
    #[test]
    fn cascade_report_json_shape() {
        let report = PushReport::from_outcome(
            "registry.example/tool:3.28.1".to_string(),
            outcome(
                "c",
                vec!["3.28".to_string(), "3".to_string(), "latest".to_string()],
                vec![format!("sha256.{}", "a".repeat(64))],
            ),
        );
        let value = serde_json::to_value(&report).unwrap();

        assert_eq!(
            value.get("identifier").and_then(|v| v.as_str()),
            Some("registry.example/tool:3.28.1")
        );
        assert_eq!(value.get("status").and_then(|v| v.as_str()), Some("pushed"));
        assert_eq!(
            value.get("manifest_digest").and_then(|v| v.as_str()),
            Some(format!("sha256:{}", "c".repeat(64)).as_str())
        );
        assert_eq!(
            value.get("cascade_tags_written").and_then(|v| v.as_array()),
            Some(&vec!["3.28".into(), "3".into(), "latest".into()])
        );
        assert_eq!(
            value.get("canonical_tags_written").and_then(|v| v.as_array()),
            Some(&vec![format!("sha256.{}", "a".repeat(64)).into()])
        );
    }

    /// A non-cascade push with `--no-canonical-tag` writes neither tag family:
    /// both arrays must serialize as empty, never absent or null.
    #[test]
    fn non_cascade_report_has_empty_tags() {
        let report = PushReport::from_outcome("tool:1.0.0".to_string(), outcome("d", Vec::new(), Vec::new()));
        let value = serde_json::to_value(&report).unwrap();

        assert_eq!(
            value.get("cascade_tags_written").and_then(|v| v.as_array()),
            Some(&Vec::new())
        );
        assert_eq!(
            value.get("canonical_tags_written").and_then(|v| v.as_array()),
            Some(&Vec::new())
        );
    }

    /// `print_plain` emits the one-row table without panicking when colour is
    /// disabled — keeps a plain push from being silent.
    #[test]
    fn print_plain_smoke() {
        let report = PushReport::from_outcome(
            "tool:1.0.0".to_string(),
            outcome(
                "d",
                vec!["1".to_string(), "latest".to_string()],
                vec![format!("sha256.{}", "b".repeat(64))],
            ),
        );
        let data = DataInterface::new(Printer::new(false, false));
        report.print_plain(&data);
    }
}
