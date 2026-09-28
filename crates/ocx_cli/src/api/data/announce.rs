// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The `ocx package announce` report; keys shared with the claim report render through
//! [`forge_report`](super::forge_report). It carries no `owners` or `author`: announce records no governance.

use ocx_announce::announce::{AnnounceOutcome, AnnounceStatus};
use ocx_announce::forge::{ForgeCredentials, ForgeKind, WriteTransport};
use ocx_console::{Cell, Column};
use serde::Serialize;

use super::forge_report::{
    CapabilityCheckEntry, CredentialKind, PushCredentialKind, credential_kind, push_credential_kind, serialize_display,
};
use crate::api::Printable;

/// Result of a successful `ocx package announce`.
///
/// Plain format: a one-row table (`Package`, `Status`, `Pull Request`, `Fork`,
/// `Written Paths`); a dash marks a field the mode did not produce, and
/// `Written Paths` is a count, not the list. Every other key is JSON-only; the
/// command warns about dropped reserved tags on stderr when there are any.
///
/// JSON format: an object with one key per field below, in that order; every
/// value vocabulary is closed as its field states.
// The forge keys are JSON-only: the plain table is at its five-column budget, and scripts read its shape.
#[derive(Serialize, schemars::JsonSchema)]
pub struct AnnounceReport {
    /// The announced `<namespace>/<package>` identifier.
    pub package: String,
    /// `"unchanged"` when the rebuilt root was byte-identical to the committed
    /// one, so nothing was committed; `"updated"` otherwise. An unchanged
    /// `--fork` run still ensures a pull request when its announce branch is
    /// ahead of the index base, and still reports one when the branch has
    /// diverged from the index base but its open pull request can still
    /// merge.
    // `status`/`desc_status` stay `String` (`AnnounceStatus` via `status_label`); every other closed
    // vocabulary here is typed, since a `String` would publish an open set.
    pub status: String,
    /// `"updated"` when the package's `__ocx.desc` artifact moved, so the
    /// root's `desc` object was rebuilt and its readme (and logo) written as
    /// new content-addressed objects; `"unchanged"` when the description sits
    /// where the committed root already recorded it, or there is none.
    pub desc_status: String,
    /// The resolved forge kind, after `--forge`: `"github"` or `"gitlab"`.
    #[serde(serialize_with = "serialize_display")]
    #[schemars(with = "String")]
    pub forge: ForgeKind,
    /// The selected write transport: `"api"` or `"git"`.
    #[serde(serialize_with = "serialize_display")]
    #[schemars(with = "String")]
    pub transport: WriteTransport,
    /// The API credential's kind. `"job-token"` only when the credential is
    /// this environment's own `CI_JOB_TOKEN` — ocx cannot tell a personal from
    /// a project, group or OAuth token, so it reports no kind it cannot
    /// observe.
    pub credential_kind: CredentialKind,
    /// The push credential's kind, or `null` under the `api` transport, which
    /// pushes nothing. `"git-helper"` means nothing was injected and git's own
    /// credential helpers authenticated the push.
    pub push_credential_kind: Option<PushCredentialKind>,
    /// The announce branch, or `null` under `--out`, which opens no request and
    /// so has no branch.
    pub branch: Option<String>,
    /// The opened or updated pull request's web URL.
    ///
    /// Always `null` for `--out`. In `--fork` mode `null` only when the run made
    /// no pull request: an unchanged run whose announce branch is ahead of the
    /// index base still ensures, and therefore reports, one. So does an unchanged
    /// run whose branch has diverged from the index base but still holds an open,
    /// mergeable pull request.
    pub pull_request_url: Option<String>,
    /// The opened or updated pull request's number; `null` under the same
    /// conditions as `pull_request_url`.
    pub pull_request_number: Option<u64>,
    /// The verified fork, as `owner/repo`; `null` for `--out`, and in `--fork`
    /// mode only when the run made no pull request.
    pub fork: Option<String>,
    /// The relative paths written under the `--out` directory; empty outside `--out`.
    pub written_paths: Vec<String>,
    /// Every preflight row, including the ones that did not apply.
    ///
    /// Non-empty on every run, so a pipeline can assert the preflight ran rather
    /// than trusting a bare success. Inapplicable rows carry `"skipped"`; rows
    /// follow a fixed capability order, so the array is stable across runs.
    pub capability_checks: Vec<CapabilityCheckEntry>,
    /// Tags dropped from the curated set because they are reserved.
    ///
    /// The OCX-internal `__ocx` namespace (which carries the keep tag) and the
    /// frozen legacy `<algorithm>.<hex>` keep tags. Neither names a version, so
    /// announce drops them and reports them here rather than failing the run.
    /// Always an array, empty rather than absent.
    pub reserved_tags_dropped: Vec<String>,
}

impl AnnounceReport {
    /// Builds the report from an announce outcome plus the forge, transport and credentials the CLI
    /// boundary resolved. Credential kinds render off the ladder's credential, never a direct
    /// `OCX_ANNOUNCE_TOKEN` read, which misses the job-token rung and reports `none`.
    #[must_use]
    pub fn from_outcome(
        outcome: AnnounceOutcome,
        forge: ForgeKind,
        transport: WriteTransport,
        credentials: &ForgeCredentials,
    ) -> Self {
        Self {
            package: outcome.package,
            status: status_label(outcome.status).to_string(),
            desc_status: status_label(outcome.desc_status).to_string(),
            forge,
            transport,
            credential_kind: credential_kind(credentials),
            // Takes the transport: the credential alone reports a push kind for a REST run that pushed nothing.
            push_credential_kind: push_credential_kind(credentials, transport),
            // `--out` leaves `branch` empty, and `""` would read as a branch name that failed to render.
            branch: (!outcome.branch.is_empty()).then_some(outcome.branch),
            pull_request_url: outcome
                .pull_request
                .as_ref()
                .map(|pull_request| pull_request.html_url.clone()),
            pull_request_number: outcome.pull_request.as_ref().map(|pull_request| pull_request.number),
            fork: outcome.fork.map(|fork| fork.full_path),
            written_paths: outcome.written_paths,
            // Every row, `Skipped` included: the report contract forbids filtering them out.
            capability_checks: CapabilityCheckEntry::from_checks(outcome.capability_checks.checks()),
            reserved_tags_dropped: outcome.reserved_tags_dropped,
        }
    }

    /// The plain table's headers and single row as paired text: the seam tests assert on, since
    /// `print_table` writes real stdout and neither [`Column`] nor [`Cell`] exposes its text.
    fn plain_table(&self) -> (Vec<&'static str>, Vec<String>) {
        (
            vec!["Package", "Status", "Pull Request", "Fork", "Written Paths"],
            vec![
                self.package.clone(),
                self.status.clone(),
                // A dash, never an empty cell.
                self.pull_request_url.clone().unwrap_or_else(|| "-".to_string()),
                self.fork.clone().unwrap_or_else(|| "-".to_string()),
                // The count, not the list: one path per tag makes it unbounded.
                if self.written_paths.is_empty() {
                    "-".to_string()
                } else {
                    self.written_paths.len().to_string()
                },
            ],
        )
    }
}

/// The wire spelling of a status, shared by the root and the description.
fn status_label(status: AnnounceStatus) -> &'static str {
    match status {
        AnnounceStatus::Unchanged => "unchanged",
        AnnounceStatus::Updated => "updated",
    }
}

impl Printable for AnnounceReport {
    fn print_plain(&self, data: &ocx_console::DataInterface) {
        // `print_table` is column-major, so a one-row table is one cell per column.
        let (headers, cells) = self.plain_table();
        let columns: Vec<Column> = headers.into_iter().map(Column::from).collect();
        let rows: Vec<Vec<Cell>> = cells.into_iter().map(|cell| vec![Cell::from(cell)]).collect();
        data.print_table(&columns, &rows);
    }
}

#[cfg(test)]
mod tests {
    use ocx_announce::announce::{AnnounceOutcome, AnnounceStatus};
    use ocx_announce::forge::{
        ForgeCredentials, ForgeKind, ForgeToken, ForkIdentity, PullRequest, PushAccess, WriteTransport,
    };
    use ocx_console::{DataInterface, Printer};

    use super::AnnounceReport;
    use crate::api::Printable as _;

    /// The three boundary-resolved values `from_outcome` now takes, as an
    /// ordinary `api` run against github.com resolves them.
    ///
    /// `ForgeCredentials::new` derives `api_is_job_token` from `CI_JOB_TOKEN`,
    /// unset in a unit-test process, so this is the "a token, but not this
    /// job's" state.
    fn boundary() -> (ForgeKind, WriteTransport, ForgeCredentials) {
        (
            ForgeKind::GitHub,
            WriteTransport::Api,
            ForgeCredentials::new(ForgeToken::new("not-a-job-token".to_string())),
        )
    }

    /// `AnnounceReport::from_outcome` with that boundary, so each test names
    /// only the outcome it is about.
    fn report(outcome: AnnounceOutcome) -> AnnounceReport {
        let (forge, transport, credentials) = boundary();
        AnnounceReport::from_outcome(outcome, forge, transport, &credentials)
    }

    /// The same credential over a **chosen** forge and transport.
    ///
    /// Needed because `push_credential_kind` is `null` for every run on the
    /// default `api` transport (C-061), so a six-keys test built only from
    /// [`report`] asserts `null` against a field that is `null` in every state —
    /// green in a world where the mapping does not exist (DX-68).
    fn report_over(outcome: AnnounceOutcome, forge: ForgeKind, transport: WriteTransport) -> AnnounceReport {
        AnnounceReport::from_outcome(outcome, forge, transport, &boundary().2)
    }

    /// An `--out` run: paths written, no request, and an **empty** branch —
    /// the shape `AnnounceOutcome`'s `Out` arm produces (`announce.rs:194`).
    fn outcome_out() -> AnnounceOutcome {
        AnnounceOutcome {
            pull_request: None,
            fork: None,
            written_paths: vec!["p/acme/widget.json".to_string()],
            branch: String::new(),
            ..outcome_updated()
        }
    }

    /// The four preflight rows a run that checked nothing reports, as JSON.
    fn skipped_capability_rows() -> serde_json::Value {
        serde_json::json!([
            { "name": "git-version", "status": "skipped", "detail": null },
            { "name": "push-access", "status": "skipped", "detail": null },
            { "name": "job-token-push", "status": "skipped", "detail": null },
            { "name": "job-token-allowlist", "status": "skipped", "detail": null },
        ])
    }

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
            branch: "indexbot-announce-acme-widget".to_string(),
            capability_checks: PushAccess::skipped_all(),
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
            branch: "indexbot-announce-acme-widget".to_string(),
            capability_checks: PushAccess::skipped_all(),
        }
    }

    #[test]
    fn json_shape_carries_every_key_and_the_updated_status() {
        let report = report(outcome_updated());
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
        let report = report(outcome);
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
        let report = report(outcome_unchanged());
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
        let report = report(outcome);
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
            branch: String::new(),
            capability_checks: PushAccess::skipped_all(),
        };
        let report = report(outcome);
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
        let report = report(outcome);
        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(value.get("desc_status").and_then(|v| v.as_str()), Some("updated"));
    }

    #[test]
    fn print_plain_smoke() {
        let data = DataInterface::new(Printer::new(false, false));
        report(outcome_updated()).print_plain(&data);
        report(outcome_unchanged()).print_plain(&data);
    }

    // ── C-061: the six new keys ───────────────────────────────────────────

    /// C-061: the report gains `forge`, `transport`, `credential_kind`,
    /// `push_credential_kind`, `branch` and `capability_checks`, and gains
    /// **nothing else**.
    ///
    /// Asserted as the exact serialized **key set**, not as six `get()`s
    /// (E-07): a presence check cannot see `owners` or `author` leaking across
    /// from the claim report — which C-061 forbids by name — and cannot see a
    /// key silently dropped either. The claim report's sibling
    /// (`api/data/claim.rs::claim_report_json_key_set`) is the same shape.
    ///
    /// The four `capability_checks` rows are pinned as a document rather than
    /// counted, which also carries E-13: a row filtered out, a row reordered, or
    /// a fifth `CapabilityName` that announce failed to project all red here.
    /// `len() > 0` would not — C-069 makes non-emptiness
    /// unrepresentable-otherwise, so a non-emptiness assertion is green in every
    /// state.
    ///
    /// E-09, recorded where it will be read: the six keys are **JSON-only**.
    /// The plain table is at its five-column budget and C-061 contracts keys,
    /// not columns; `announce_plain_table_pairs_five_headers_with_five_cells`
    /// pins that the five did not change to make room.
    ///
    /// Red at the stub: `from_outcome` is `unimplemented!()`.
    /// Mutation once implemented: add `pub owners: Vec<String>` to
    /// `AnnounceReport` (a presence-only check stays green, the key set reds);
    /// or resolve `credential_kind` from a direct `OCX_ANNOUNCE_TOKEN` read
    /// instead of from the resolved credential.
    #[test]
    fn announce_report_gains_six_keys() {
        let value = serde_json::to_value(report(outcome_updated())).expect("the report serializes");
        let object = value.as_object().expect("the report is a JSON object");
        let keys: Vec<&str> = object.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            vec![
                "package",
                "status",
                "desc_status",
                "forge",
                "transport",
                "credential_kind",
                "push_credential_kind",
                "branch",
                "pull_request_url",
                "pull_request_number",
                "fork",
                "written_paths",
                "capability_checks",
                "reserved_tags_dropped",
            ],
            "C-061's key set, in the contract's order — and no `owners`/`author`, whose absence C-061 names"
        );

        assert_eq!(
            value["forge"], "github",
            "the forge `validate` resolved, not a second guess"
        );
        assert_eq!(value["transport"], "api");
        assert_eq!(
            value["credential_kind"], "token",
            "a credential the ladder resolved that is not this job's own"
        );
        assert!(
            value["push_credential_kind"].is_null(),
            "the api transport pushes nothing: {value}"
        );
        assert_eq!(value["branch"], "indexbot-announce-acme-widget");
        assert_eq!(
            value["capability_checks"],
            skipped_capability_rows(),
            "every preflight row is reported, in CapabilityName declaration order, skipped ones included"
        );
    }

    /// DX-68 / E-12: the `git` transport is what makes `push_credential_kind`
    /// discriminate.
    ///
    /// `push_credential_kind` is `null` on **every** default-transport run, so
    /// the row in `announce_report_gains_six_keys` asserts `null` against a
    /// field that is `null` in every state — including one where the mapping was
    /// never wired at all. This is the case that can tell those apart.
    ///
    /// `git-helper` rather than `token`: nothing was injected here, and
    /// `git-helper` is the statement that a push happened and git's own
    /// credential helpers authenticated it. The `job-token` rung needs the
    /// process environment and is covered at library scope and end to end.
    ///
    /// Red at the stub: `from_outcome` is `unimplemented!()`.
    /// Mutation once implemented: pass `WriteTransport::Api` to
    /// `push_credential_kind` regardless of the selected transport — the
    /// `api` case stays green and this reds.
    #[test]
    fn announce_report_under_the_git_transport_reports_a_push_credential_kind() {
        let value = serde_json::to_value(report_over(outcome_updated(), ForgeKind::GitLab, WriteTransport::Git))
            .expect("the report serializes");
        assert_eq!(value["forge"], "gitlab");
        assert_eq!(value["transport"], "git");
        assert_eq!(
            value["push_credential_kind"], "git-helper",
            "nothing injected under the git transport is `git-helper`, never null: {value}"
        );
    }

    /// DX-40.3 / E-08: `branch` is `null` under `--out`, never `""`.
    ///
    /// [`AnnounceOutcome::branch`] is a `String` that the `Out` arm leaves
    /// empty, so the naive projection ships `"branch": ""` — a value C-060's
    /// vocabulary has no meaning for, and one a consumer cannot tell from a
    /// branch name that failed to render. Held as `Option<String>` on the report
    /// for exactly that reason.
    ///
    /// Red at the stub: `from_outcome` is `unimplemented!()`.
    /// Mutation once implemented: project `branch: Some(outcome.branch)` — it
    /// compiles, the request-mode rows stay green, and this reds on `""`.
    #[test]
    fn announce_report_branch_is_null_under_out() {
        let value = serde_json::to_value(report(outcome_out())).expect("the report serializes");
        assert!(
            value["branch"].is_null(),
            "an --out run opens no request and so has no branch: {value}"
        );
        assert_eq!(
            value["written_paths"],
            serde_json::json!(["p/acme/widget.json"]),
            "the mode that has no branch is the one that writes paths"
        );

        let request_mode = serde_json::to_value(report(outcome_updated())).expect("serializes");
        assert_eq!(
            request_mode["branch"], "indexbot-announce-acme-widget",
            "a run that opens a request still reports its branch — `null` is the --out statement, not the default"
        );
    }

    /// E-10: the plain table's five headers pair with five cells, in one order.
    ///
    /// [`ocx_console::DataInterface::print_table`] writes the real stdout and
    /// neither [`Column`] nor [`Cell`] exposes its text, so before the
    /// [`AnnounceReport::plain_table`] seam existed a test calling
    /// [`Printable::print_plain`] could assert **nothing at all** — this report
    /// was the workspace's counter-example, named as such in the claim report's
    /// own doc comment. `print_plain` is a pure adapter over the seam, so the
    /// sequence asserted here is the one an operator sees.
    ///
    /// This is also E-09's control: the six C-061 keys are JSON-only, and the
    /// five headers below are what pins that the plain shape was not widened to
    /// make room for `Transport` or `Branch` — an uncontracted CLI break, since
    /// the column budget is five.
    ///
    /// Red at the stub: `from_outcome` is `unimplemented!()`.
    /// Mutation once implemented: swap two cells in `plain_table` (the headers
    /// still pass, the pairing reds); or add a sixth header.
    #[test]
    fn announce_plain_table_pairs_five_headers_with_five_cells() {
        let (headers, cells) = report(outcome_updated()).plain_table();
        assert_eq!(
            headers,
            vec!["Package", "Status", "Pull Request", "Fork", "Written Paths"],
            "the five-column plain budget"
        );
        assert_eq!(
            cells,
            vec![
                "acme/widget".to_string(),
                "updated".to_string(),
                "https://github.com/ocx-sh/index/pull/42".to_string(),
                "forkuser/index".to_string(),
                "-".to_string(),
            ],
            "each cell under its own header, a dash where the mode produced nothing"
        );

        let (_, out_cells) = report(outcome_out()).plain_table();
        assert_eq!(
            out_cells,
            vec![
                "acme/widget".to_string(),
                "updated".to_string(),
                "-".to_string(),
                "-".to_string(),
                "1".to_string(),
            ],
            "--out reports how many paths it wrote, never the list: one path per tag is unbounded"
        );
    }
}
