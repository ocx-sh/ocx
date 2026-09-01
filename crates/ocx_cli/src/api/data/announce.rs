// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::announce::{AnnounceOutcome, AnnounceStatus};
use ocx_lib::cli::Cell;
use serde::Serialize;

use crate::api::Printable;

/// Result of a successful `ocx package announce`.
///
/// Plain format: a one-row table (`Package`, `Status`, `Pull Request`, `Fork`,
/// `Written Paths`) — a dash marks a field the mode did not produce, and
/// `Written Paths` is how many were written, not the list (it is unbounded by
/// construction: one path per tag). Dropped reserved tags have no column; the
/// command warns about them on stderr when there are any.
///
/// JSON format:
/// `{ "package", "status", "desc_status", "pull_request_url",
/// "pull_request_number", "fork", "written_paths", "reserved_tags_dropped" }`.
/// `status` and `desc_status` are each exactly `"unchanged"` or `"updated"`;
/// `desc_status` is JSON-only, the plain table being at its column budget.
/// `pull_request_url` /
/// `pull_request_number` / `fork` are always `null` for `--out`; in `--fork`
/// mode they are `null` only when the run made no pull request — an unchanged
/// run whose announce branch is ahead of the index base still ensures, and
/// therefore reports, one. `written_paths` is empty outside `--out`.
/// `reserved_tags_dropped` is an array, empty rather than absent.
#[derive(Serialize, schemars::JsonSchema)]
pub struct AnnounceReport {
    /// The announced `<namespace>/<package>` identifier.
    pub package: String,
    /// `"unchanged"` when the rebuilt root was byte-identical to the committed
    /// one, so nothing was committed; `"updated"` otherwise. An unchanged
    /// `--fork` run still ensures a pull request when its announce branch is
    /// ahead of the index base.
    pub status: String,
    /// `"updated"` when the package's `__ocx.desc` artifact moved, so the
    /// root's `desc` object was rebuilt and its readme (and logo) written as
    /// new content-addressed objects; `"unchanged"` when the description sits
    /// where the committed root already recorded it, or there is none.
    pub desc_status: String,
    /// The opened/updated pull request's web URL.
    pub pull_request_url: Option<String>,
    /// The opened/updated pull request's number.
    pub pull_request_number: Option<u64>,
    /// The verified fork, as `owner/repo`.
    pub fork: Option<String>,
    /// The relative paths written under the `--out` directory.
    pub written_paths: Vec<String>,
    /// Tags dropped from the curated set because they are reserved: the
    /// OCX-internal `__ocx` namespace (which carries the keep tag) and the
    /// frozen legacy `<algorithm>.<hex>` keep tags.
    /// Neither names a version, so announce drops them and reports them here
    /// rather than failing the run.
    pub reserved_tags_dropped: Vec<String>,
}

impl AnnounceReport {
    /// Builds a report from an [`AnnounceOutcome`].
    pub fn from_outcome(outcome: AnnounceOutcome) -> Self {
        Self {
            package: outcome.package,
            status: status_label(outcome.status).to_string(),
            desc_status: status_label(outcome.desc_status).to_string(),
            pull_request_url: outcome.pull_request.as_ref().map(|pr| pr.html_url.clone()),
            pull_request_number: outcome.pull_request.as_ref().map(|pr| pr.number),
            fork: outcome.fork.as_ref().map(|fork| fork.full_path.clone()),
            written_paths: outcome.written_paths,
            reserved_tags_dropped: outcome.reserved_tags_dropped,
        }
    }
}

/// The wire spelling of a status — the same two words for the root and for the
/// description, so a consumer parses one vocabulary.
fn status_label(status: AnnounceStatus) -> &'static str {
    match status {
        AnnounceStatus::Unchanged => "unchanged",
        AnnounceStatus::Updated => "updated",
    }
}

impl Printable for AnnounceReport {
    fn print_plain(&self, data: &ocx_lib::cli::DataInterface) {
        data.print_table(
            &[
                "Package".into(),
                "Status".into(),
                "Pull Request".into(),
                "Fork".into(),
                "Written Paths".into(),
            ],
            &[
                vec![Cell::from(self.package.clone())],
                vec![Cell::from(self.status.clone())],
                vec![Cell::from(
                    self.pull_request_url.clone().unwrap_or_else(|| "-".to_string()),
                )],
                vec![Cell::from(self.fork.clone().unwrap_or_else(|| "-".to_string()))],
                vec![Cell::from(if self.written_paths.is_empty() {
                    "-".to_string()
                } else {
                    self.written_paths.len().to_string()
                })],
            ],
        );
    }
}

#[cfg(test)]
mod tests {
    use ocx_lib::announce::{AnnounceOutcome, AnnounceStatus};
    use ocx_lib::cli::{DataInterface, Printer};
    use ocx_lib::forge::{ForkIdentity, PullRequest};

    use super::AnnounceReport;
    use crate::api::Printable as _;

    fn outcome_updated() -> AnnounceOutcome {
        AnnounceOutcome {
            package: "acme/widget".to_string(),
            status: AnnounceStatus::Updated,
            pull_request: Some(PullRequest {
                number: 42,
                html_url: "https://github.com/ocx-sh/index/pull/42".to_string(),
                updated: false,
            }),
            fork: Some(ForkIdentity {
                full_path: "forkuser/index".to_string(),
                namespace: "forkuser".to_string(),
                project: "index".to_string(),
                id: None,
            }),
            written_paths: Vec::new(),
            reserved_tags_dropped: Vec::new(),
            desc_status: AnnounceStatus::Unchanged,
        }
    }

    fn outcome_unchanged() -> AnnounceOutcome {
        AnnounceOutcome {
            package: "acme/widget".to_string(),
            status: AnnounceStatus::Unchanged,
            pull_request: None,
            fork: None,
            written_paths: Vec::new(),
            reserved_tags_dropped: Vec::new(),
            desc_status: AnnounceStatus::Unchanged,
        }
    }

    #[test]
    fn json_shape_carries_every_key_and_the_updated_status() {
        let report = AnnounceReport::from_outcome(outcome_updated());
        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(value.get("package").and_then(|v| v.as_str()), Some("acme/widget"));
        assert_eq!(value.get("status").and_then(|v| v.as_str()), Some("updated"));
        assert_eq!(
            value.get("pull_request_url").and_then(|v| v.as_str()),
            Some("https://github.com/ocx-sh/index/pull/42")
        );
        assert_eq!(value.get("pull_request_number").and_then(|v| v.as_u64()), Some(42));
        assert_eq!(value.get("fork").and_then(|v| v.as_str()), Some("forkuser/index"));
        assert_eq!(value.get("written_paths").and_then(|v| v.as_array()), Some(&vec![]));
        assert_eq!(
            value.get("desc_status").and_then(|v| v.as_str()),
            Some("unchanged"),
            "the description status is reported on every run, not only when it moved"
        );
        assert_eq!(
            value.get("reserved_tags_dropped").and_then(|v| v.as_array()),
            Some(&vec![]),
            "an empty drop list is an empty array, never absent"
        );
    }

    /// D7 drops are a reported fact of a successful run, not a failure: the
    /// report carries them alongside an ordinary `updated` status.
    #[test]
    fn dropped_reserved_tags_are_reported_on_a_successful_run() {
        let outcome = AnnounceOutcome {
            reserved_tags_dropped: vec![
                "__ocx.desc".to_string(),
                format!("__ocx.keep.sha256-{}", "a".repeat(64)),
            ],
            ..outcome_updated()
        };
        let report = AnnounceReport::from_outcome(outcome);
        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(value.get("status").and_then(|v| v.as_str()), Some("updated"));
        assert_eq!(
            value
                .get("reserved_tags_dropped")
                .and_then(|v| v.as_array())
                .map(Vec::len),
            Some(2)
        );
    }

    #[test]
    fn unchanged_no_op_has_null_pull_request_fields() {
        let report = AnnounceReport::from_outcome(outcome_unchanged());
        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(value.get("status").and_then(|v| v.as_str()), Some("unchanged"));
        assert!(value.get("pull_request_url").unwrap().is_null());
        assert!(value.get("pull_request_number").unwrap().is_null());
        assert!(value.get("fork").unwrap().is_null());
    }

    /// `unchanged` does NOT imply "no pull request": a run whose announce branch
    /// is ahead of the index base ensures one even though it committed nothing,
    /// and the report must carry it (design register C6 amendment).
    #[test]
    fn unchanged_status_reports_an_ensured_pull_request() {
        let outcome = AnnounceOutcome {
            status: AnnounceStatus::Unchanged,
            ..outcome_updated()
        };
        let report = AnnounceReport::from_outcome(outcome);
        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(value.get("status").and_then(|v| v.as_str()), Some("unchanged"));
        assert_eq!(
            value.get("pull_request_url").and_then(|v| v.as_str()),
            Some("https://github.com/ocx-sh/index/pull/42")
        );
        assert_eq!(value.get("pull_request_number").and_then(|v| v.as_u64()), Some(42));
        assert_eq!(value.get("fork").and_then(|v| v.as_str()), Some("forkuser/index"));
    }

    #[test]
    fn out_mode_has_null_pull_request_url_and_lists_written_paths() {
        let outcome = AnnounceOutcome {
            package: "acme/widget".to_string(),
            status: AnnounceStatus::Updated,
            pull_request: None,
            fork: None,
            written_paths: vec!["p/acme/widget.json".to_string()],
            reserved_tags_dropped: Vec::new(),
            desc_status: AnnounceStatus::Unchanged,
        };
        let report = AnnounceReport::from_outcome(outcome);
        let value = serde_json::to_value(&report).unwrap();
        assert!(value.get("pull_request_url").unwrap().is_null());
        assert_eq!(
            value.get("written_paths").and_then(|v| v.as_array()),
            Some(&vec![serde_json::Value::String("p/acme/widget.json".to_string())])
        );
    }

    /// The description moves on its own axis: a run can rewrite `desc` (and its
    /// blobs) while the root's own status says `updated` for either reason, so
    /// the two statuses are reported independently.
    #[test]
    fn a_moved_description_reports_its_own_updated_status() {
        let outcome = AnnounceOutcome {
            desc_status: AnnounceStatus::Updated,
            ..outcome_updated()
        };
        let report = AnnounceReport::from_outcome(outcome);
        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(value.get("desc_status").and_then(|v| v.as_str()), Some("updated"));
    }

    #[test]
    fn print_plain_smoke() {
        let data = DataInterface::new(Printer::new(false, false));
        AnnounceReport::from_outcome(outcome_updated()).print_plain(&data);
        AnnounceReport::from_outcome(outcome_unchanged()).print_plain(&data);
    }
}
