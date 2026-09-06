// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The `ocx package claim` report (C-060).
//!
//! Six of its keys — `forge`, `transport`, `credential_kind`,
//! `push_credential_kind`, `branch` and `capability_checks` — are the same keys
//! and the same value vocabularies the announce report gains under C-061. Their
//! renderers therefore live in the neutral
//! [`forge_report`](super::forge_report) module (DX-59), not here: one
//! vocabulary across two commands means one renderer, and putting it in the
//! newer, more specific report would point the older, more general one at it
//! for the life of the branch.

use ocx_lib::claim::{ClaimOutcome, ClaimStatus, OwnerIdentitySource, ResolvedOwner};
use ocx_lib::cli::{Cell, Column};
use ocx_lib::forge::{ForgeCredentials, ForgeKind, WriteTransport};
use serde::Serialize;

use super::forge_report::{
    CapabilityCheckEntry, CredentialKind, PushCredentialKind, serialize_display, serialize_optional_display,
};
use crate::api::Printable;

/// One forge account, as the claim root spells it.
///
/// The same `{login, id}` shape the root file writes, so a consumer comparing
/// the report against the committed entry compares like with like. Never a bare
/// login: an id is what survives a rename.
#[derive(Serialize, schemars::JsonSchema)]
pub struct OwnerEntry {
    /// The forge's canonical login spelling.
    pub login: String,
    /// The forge's immutable numeric account id.
    pub id: u64,
}

impl OwnerEntry {
    /// Project one library-side owner into its wire shape.
    ///
    /// A projection rather than a re-export of [`ResolvedOwner`]: the wire shape
    /// is this layer's contract, and the library type carries no `Serialize`.
    fn from_resolved(owner: ResolvedOwner) -> Self {
        Self {
            login: owner.login,
            id: owner.id,
        }
    }
}

/// Result of a successful `ocx package claim`.
///
/// Plain format: a one-row table (`Package`, `Status`, `Transport`, `Branch`,
/// `Pull Request`) — a dash marks a field the run did not produce. `owners`,
/// `author`, `author_identity_source` and `capability_checks` are JSON-only,
/// the plain table being at its five-column budget.
///
/// JSON format: `{ "package", "name", "status", "forge", "transport",
/// "credential_kind", "push_credential_kind", "author",
/// "author_identity_source", "owners", "owner_identity_source", "branch",
/// "pull_request_url", "pull_request_number", "fork", "written_paths",
/// "capability_checks" }`, in that order. The value vocabularies are contracted
/// as tightly as the keys: `status` is `"unchanged"` or `"updated"`;
/// `credential_kind` is
/// `"job-token"`, `"token"` or `"none"`; `push_credential_kind` is
/// `"job-token"`, `"token"`, `"git-helper"` or `null`, and always `null` under
/// the `api` transport; `owner_identity_source` is `"resolved"`, `"asserted"`
/// or `"ci-environment"`; `author_identity_source` is `"resolved"`,
/// `"ci-environment"` or `null`, never `"asserted"`. `capability_checks` is
/// non-empty on every run,
/// inapplicable rows carrying `"skipped"`, ordered by `CapabilityName`'s
/// declaration order so the array is stable across runs.
///
/// `status` uses announce's two words over a **different subject**: claim
/// compares against the open claim branch, not the committed root, because a
/// committed root has already exited 65. A claim `--out` run is therefore
/// always `"updated"`, where an announce `--out` run can report `"unchanged"`.
///
/// **Every closed vocabulary above is held as its own enum, never as a
/// `String`.** The report is a published schema (`report_roots!`), so a
/// stringly-typed field would promise an open string where C-060 contracts a
/// closed set — and would let this layer spell a word the library cannot
/// produce. The two credential kinds are this crate's own enums and carry
/// `Serialize`; the four `ocx_lib` owns render through
/// [`serialize_display`](super::forge_report::serialize_display), which is why
/// their JSON bytes are unchanged from the `.to_string()` form they replaced.
#[derive(Serialize, schemars::JsonSchema)]
pub struct ClaimReport {
    /// The claimed `<namespace>/<package>` identifier, as given.
    pub package: String,
    /// The logical name written into the root.
    pub name: String,
    /// `"unchanged"` when the open claim branch already carries a
    /// byte-identical root; `"updated"` otherwise, which `--out` always is.
    #[serde(serialize_with = "serialize_display")]
    #[schemars(with = "String")]
    pub status: ClaimStatus,
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
    /// The identity that authored the request — the token identity, else the
    /// CI-environment identity. `null` when neither is available. Distinct from
    /// [`Self::owners`] by construction: authorship is not ownership.
    ///
    /// **Not attested.** The second rung is an ordinary environment read —
    /// `GITLAB_USER_LOGIN`/`GITLAB_USER_ID`, else `GITHUB_ACTOR`/`GITHUB_ACTOR_ID`
    /// — and an earlier pipeline step can set those to anything. Only the first
    /// rung is the forge's own answer about the credential. A consumer must not
    /// treat this key as an attestation of who ran the command; which rung
    /// answered is [`Self::author_identity_source`].
    pub author: Option<OwnerEntry>,
    /// Which rule produced [`Self::author`]: `"resolved"` when the forge's own
    /// answer about the credential did, `"ci-environment"` when the CI pair did.
    /// `null` exactly when `author` is.
    ///
    /// The sibling of [`Self::owner_identity_source`] over a different subject,
    /// and the key that makes `author` usable: the login alone cannot say
    /// whether the forge asserted it or a pipeline step wrote it into an
    /// environment variable, and a governance consumer needs that difference.
    /// `"asserted"` is unreachable here — no operator word is ever taken for
    /// the author.
    #[serde(serialize_with = "serialize_optional_display")]
    #[schemars(with = "Option<String>")]
    pub author_identity_source: Option<OwnerIdentitySource>,
    /// The resolved owner list written into the root, in the order given.
    pub owners: Vec<OwnerEntry>,
    /// Which rule produced [`Self::owners`]: `"resolved"`, `"asserted"` or
    /// `"ci-environment"`. The same word appears on stderr and in the request
    /// body.
    #[serde(serialize_with = "serialize_display")]
    #[schemars(with = "String")]
    pub owner_identity_source: OwnerIdentitySource,
    /// The claim branch, so a script need not re-derive the naming convention.
    /// Always present, `--out` included: the name is derived from the package,
    /// not read from the forge, so a run that opens no request still reports
    /// the branch a later run would use.
    pub branch: String,
    /// The opened or updated request's web URL.
    pub pull_request_url: Option<String>,
    /// The opened or updated request's number.
    pub pull_request_number: Option<u64>,
    /// The verified fork, as `namespace/project`; `null` on the direct path and
    /// under the `git` transport.
    pub fork: Option<String>,
    /// The relative paths written under the `--out` directory; empty otherwise.
    pub written_paths: Vec<String>,
    /// Every preflight row, including the ones that did not apply. Non-empty on
    /// every run, so a pipeline can assert the preflight ran rather than
    /// trusting a bare success.
    pub capability_checks: Vec<CapabilityCheckEntry>,
}

impl ClaimReport {
    /// Build the report from a claim outcome and the boundary-resolved values
    /// the outcome does not carry.
    ///
    /// `forge`, `transport` and `credentials` are passed in because all three
    /// are decided at the CLI boundary — the forge never reads the environment
    /// for itself — and the outcome describes only what the claim did.
    #[must_use]
    pub fn from_outcome(
        outcome: ClaimOutcome,
        forge: ForgeKind,
        transport: WriteTransport,
        credentials: &ForgeCredentials,
    ) -> Self {
        // `capability_checks` comes from `outcome.push_access.checks()`, which
        // is a `PushAccess` and not a `Vec` (DX-40.1), and every row is
        // projected — filtering the `skipped` ones is what S-011 forbids.
        // Read before the outcome's owned fields move out of it.
        let capability_checks = CapabilityCheckEntry::from_checks(outcome.push_access.checks());
        Self {
            package: outcome.package,
            name: outcome.name,
            // The outcome's own status, never a literal of this layer's: claim
            // compares against the open claim branch, so `--out` is `updated`.
            status: outcome.status,
            // Both resolved at the CLI boundary, neither carried on the
            // outcome — a second resolution here would be free to disagree.
            forge,
            transport,
            credential_kind: super::forge_report::credential_kind(credentials),
            push_credential_kind: super::forge_report::push_credential_kind(credentials, transport),
            // A straight projection: the identity ladder runs inside
            // `claim::claim`, so this layer must not re-derive it (DX-51) —
            // the provenance word least of all, which is unrecoverable from
            // the login it describes.
            author: outcome.author.map(OwnerEntry::from_resolved),
            author_identity_source: outcome.author_identity_source,
            owners: outcome.owners.into_iter().map(OwnerEntry::from_resolved).collect(),
            owner_identity_source: outcome.owner_identity_source,
            // Derived from the package, so it is populated on every path,
            // `--out` included — never blanked to `null` (R-27).
            branch: outcome.branch,
            pull_request_url: outcome.pull_request.as_ref().map(|request| request.html_url.clone()),
            pull_request_number: outcome.pull_request.as_ref().map(|request| request.number),
            fork: outcome.fork.as_ref().map(|fork| fork.full_path.clone()),
            written_paths: outcome.written_paths,
            capability_checks,
        }
    }

    /// The plain table's headers and its single row's cells, as text.
    ///
    /// A seam, and the reason it exists is testability rather than reuse:
    /// [`ocx_lib::cli::DataInterface::print_table`] writes to the real stdout
    /// and neither [`Column`] nor [`Cell`] exposes its text, so a test that
    /// calls [`Printable::print_plain`] can assert nothing at all — a green
    /// indistinguishable from the check never having run
    /// (`quality-core.md` § Unchecked Green). The live precedent in the sibling
    /// announce report is exactly that shape and asserts nothing.
    ///
    /// [`Printable::print_plain`] below is a pure adapter over this function, so
    /// the sequence asserted by `claim_report_plain_is_five_columns` is the one
    /// an operator sees. Rendering the columns inline instead would leave this
    /// function unused, and the test measuring nothing.
    ///
    /// Both halves are returned together because the contract is their
    /// **pairing**: five headers and five cells, in one order.
    fn plain_table(&self) -> (Vec<&'static str>, Vec<String>) {
        (
            vec!["Package", "Status", "Transport", "Branch", "Pull Request"],
            vec![
                self.package.clone(),
                // The same `Display` the JSON renders through, so plain and
                // JSON cannot spell one value two ways.
                self.status.to_string(),
                self.transport.to_string(),
                self.branch.clone(),
                // A dash, never an empty cell (DX-40.3's idiom).
                self.pull_request_url.clone().unwrap_or_else(|| "-".to_string()),
            ],
        )
    }
}

impl Printable for ClaimReport {
    fn print_plain(&self, data: &ocx_lib::cli::DataInterface) {
        // One `print_table` call, never two (single-table rule). `print_table`
        // reads its rows column-major — one `Vec<Cell>` per column — so a
        // one-row table is one cell per column.
        let (headers, cells) = self.plain_table();
        let columns: Vec<Column> = headers.into_iter().map(Column::from).collect();
        let rows: Vec<Vec<Cell>> = cells.into_iter().map(|cell| vec![Cell::from(cell)]).collect();
        data.print_table(&columns, &rows);
    }
}

#[cfg(test)]
mod tests {
    use ocx_lib::claim::{ClaimOutcome, ClaimStatus, OwnerIdentitySource, ResolvedOwner};
    use ocx_lib::forge::{ForgeCredentials, ForgeKind, ForgeToken, PullRequest, PushAccess, WriteTransport};

    use super::ClaimReport;

    /// The `--out` shape: the tree was written, no request was opened, and the
    /// forge was still read for the C-050 refusal and for owner resolution.
    fn out_outcome() -> ClaimOutcome {
        ClaimOutcome {
            package: "acme/widget".to_string(),
            name: "acme/widget".to_string(),
            status: ClaimStatus::Updated,
            owners: Vec::new(),
            owner_identity_source: OwnerIdentitySource::Resolved,
            author: None,
            author_identity_source: None,
            branch: "indexbot-claim-acme-widget".to_string(),
            pull_request: None,
            fork: None,
            written_paths: vec!["p/acme/widget.json".to_string()],
            push_access: PushAccess::skipped_all(),
        }
    }

    /// The ordinary REST shape: a request was opened.
    fn request_outcome() -> ClaimOutcome {
        ClaimOutcome {
            pull_request: Some(PullRequest {
                number: 42,
                html_url: "https://github.com/ocx-sh/index/pull/42".to_string(),
                updated: false,
            }),
            written_paths: Vec::new(),
            ..out_outcome()
        }
    }

    fn credentials() -> ForgeCredentials {
        ForgeCredentials::new(ForgeToken::new("not-a-job-token".to_string()))
    }

    fn report(outcome: ClaimOutcome, forge: ForgeKind, transport: WriteTransport) -> ClaimReport {
        ClaimReport::from_outcome(outcome, forge, transport, &credentials())
    }

    /// C-060: the whole seventeen-key set, in contract order.
    ///
    /// Asserted as the **set of keys**, not as seventeen individual `get()`s —
    /// a missing key otherwise passes every one of the sixteen lookups that
    /// remain. Asserted at unit scope and not only at WP-16's live run because a
    /// `serde` field typo should not need a registry, a fake forge and a rebuilt
    /// binary to surface.
    ///
    /// Red at the stub: `from_outcome` is `unimplemented!()`.
    /// Mutation once implemented: rename any field, or add a `serde(rename)`.
    #[test]
    fn claim_report_json_key_set() {
        let value = serde_json::to_value(report(request_outcome(), ForgeKind::GitHub, WriteTransport::Api))
            .expect("the report serializes");
        let object = value.as_object().expect("the report is a JSON object");
        let keys: Vec<&str> = object.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            vec![
                "package",
                "name",
                "status",
                "forge",
                "transport",
                "credential_kind",
                "push_credential_kind",
                "author",
                "author_identity_source",
                "owners",
                "owner_identity_source",
                "branch",
                "pull_request_url",
                "pull_request_number",
                "fork",
                "written_paths",
                "capability_checks",
            ],
            "C-060's seventeen keys, in the contract's order"
        );
    }

    /// C-060: the whole document, byte for byte, for two runs that between them
    /// exercise every closed vocabulary on the report.
    ///
    /// The key-set test above cannot see a **value** change, and the six
    /// vocabulary fields are held as enums rather than as `String`s — so this is
    /// the check that the typing bought no wire change. It is a golden
    /// document, deliberately: a published report schema is an interface
    /// (CLAUDE.md § Stability tiers), and a per-field assertion would let a
    /// seventh field's spelling drift unwatched.
    ///
    /// Between the two rows: `status` `updated`, `forge` `github`/`gitlab`,
    /// `transport` `api`/`git`, `credential_kind` `token`,
    /// `push_credential_kind` `null`/`git-helper`, `owner_identity_source`
    /// `resolved`/`ci-environment`, and every `capability_checks` row's
    /// `name`/`status` pair.
    ///
    /// Mutation: drop `#[serde(rename_all = "kebab-case")]` from
    /// `PushCredentialKind` (`git-helper` becomes `GitHelper`); replace a
    /// `serialize_with = "serialize_display"` with serde's default for a
    /// unit-variant enum (`ci-environment` becomes `CiEnvironment`).
    #[test]
    fn claim_report_wire_document_is_unchanged_by_the_typed_vocabularies() {
        let skipped_rows = serde_json::json!([
            { "name": "git-version", "status": "skipped", "detail": null },
            { "name": "push-access", "status": "skipped", "detail": null },
            { "name": "job-token-push", "status": "skipped", "detail": null },
            { "name": "job-token-allowlist", "status": "skipped", "detail": null },
        ]);

        let api = serde_json::to_value(report(request_outcome(), ForgeKind::GitHub, WriteTransport::Api))
            .expect("serializes");
        assert_eq!(
            api,
            serde_json::json!({
                "package": "acme/widget",
                "name": "acme/widget",
                "status": "updated",
                "forge": "github",
                "transport": "api",
                "credential_kind": "token",
                "push_credential_kind": null,
                "author": null,
                "author_identity_source": null,
                "owners": [],
                "owner_identity_source": "resolved",
                "branch": "indexbot-claim-acme-widget",
                "pull_request_url": "https://github.com/ocx-sh/index/pull/42",
                "pull_request_number": 42,
                "fork": null,
                "written_paths": [],
                "capability_checks": skipped_rows,
            })
        );

        let ci_outcome = ClaimOutcome {
            owner_identity_source: OwnerIdentitySource::CiEnvironment,
            author: Some(ResolvedOwner {
                login: "carol".to_string(),
                id: 5,
            }),
            author_identity_source: Some(OwnerIdentitySource::CiEnvironment),
            owners: vec![ResolvedOwner {
                login: "carol".to_string(),
                id: 5,
            }],
            ..out_outcome()
        };
        let git = serde_json::to_value(report(ci_outcome, ForgeKind::GitLab, WriteTransport::Git)).expect("serializes");
        assert_eq!(
            git,
            serde_json::json!({
                "package": "acme/widget",
                "name": "acme/widget",
                "status": "updated",
                "forge": "gitlab",
                "transport": "git",
                "credential_kind": "token",
                "push_credential_kind": "git-helper",
                "author": { "login": "carol", "id": 5 },
                "author_identity_source": "ci-environment",
                "owners": [{ "login": "carol", "id": 5 }],
                "owner_identity_source": "ci-environment",
                "branch": "indexbot-claim-acme-widget",
                "pull_request_url": null,
                "pull_request_number": null,
                "fork": null,
                "written_paths": ["p/acme/widget.json"],
                "capability_checks": skipped_rows,
            })
        );
    }

    /// C-060 / DX-40.3: a key the run did not produce is `null`, never `""` and
    /// never absent.
    ///
    /// `branch` is the deliberate exception and is asserted in the same
    /// function: `ClaimOutcome::branch` is derived from the package rather than
    /// read from the forge, so it is populated on **every** path including
    /// `--out`. A builder porting announce's `""` → `null` rule across would
    /// blank a field that is always present. There is **no reachable red** for
    /// that half — claim never produces an empty branch, so the mutation is a
    /// no-op — and the control is this assertion plus the reviewer noting the
    /// asymmetry against announce.
    ///
    /// Red at the stub: `from_outcome` is `unimplemented!()`.
    /// Mutation once implemented: render `pull_request_url` with
    /// `unwrap_or_default()`.
    #[test]
    fn absent_keys_render_null_and_branch_is_always_present() {
        let value =
            serde_json::to_value(report(out_outcome(), ForgeKind::GitHub, WriteTransport::Api)).expect("serializes");
        for key in [
            "pull_request_url",
            "pull_request_number",
            "fork",
            "author",
            "author_identity_source",
        ] {
            assert!(
                value.get(key).expect("the key is present").is_null(),
                "an absent {key} renders null, never an empty string"
            );
        }
        assert_eq!(
            value.get("branch").and_then(serde_json::Value::as_str),
            Some("indexbot-claim-acme-widget"),
            "branch is derived from the package, so --out reports it too"
        );
    }

    /// C-060 / S-010: a claim `--out` run always reports `updated`.
    ///
    /// **A characterization test at library scope** — `claim::claim` already
    /// returns `ClaimStatus::Updated` on the `--out` path, so a WP-14 test over
    /// a constructed outcome is green the moment `from_outcome` exists. Stated
    /// honestly rather than dressed up: what this pins is the **mapper**, which
    /// is where a builder copying announce goes wrong — announce compares
    /// against the committed root and can report `unchanged`, claim compares
    /// against the open claim branch and a committed root has already exited 65.
    ///
    /// The word is asserted twice on purpose: against the literal the contract
    /// spells, and against `ClaimStatus`'s own `Display`. The literal catches a
    /// `Display` typo; the equality catches a mapper that ignores the outcome
    /// and hardcodes the word.
    ///
    /// Red at the stub: `from_outcome` is `unimplemented!()`.
    /// Mutation once implemented: render `status` from
    /// `outcome.written_paths.is_empty()` rather than from `outcome.status`.
    #[test]
    fn claim_out_status_is_always_updated() {
        let report = report(out_outcome(), ForgeKind::GitHub, WriteTransport::Api);
        assert_eq!(
            report.status.to_string(),
            "updated",
            "C-060's word for a claim that moved the branch"
        );
        assert_eq!(
            report.status,
            ClaimStatus::Updated,
            "the mapper renders the outcome's own status, not a literal of its own"
        );
    }

    /// C-060 / R-32: `forge` and `transport` are the values the **CLI boundary**
    /// resolved, not values re-derived from the coordinate.
    ///
    /// Neither is carried on `ClaimOutcome`, so nothing downstream can catch a
    /// wrong one: a report rendering `forge` from `index_repo.host` prints a
    /// hostname for a self-hosted GitLab the operator declared with `--forge`.
    ///
    /// Red at the stub: `from_outcome` is `unimplemented!()`.
    /// Mutation once implemented: render either field from anything but the
    /// argument.
    #[test]
    fn forge_and_transport_render_the_boundary_resolved_values() {
        let report = report(out_outcome(), ForgeKind::GitLab, WriteTransport::Git);
        assert_eq!(report.forge, ForgeKind::GitLab);
        assert_eq!(report.transport, WriteTransport::Git);
        let value = serde_json::to_value(&report).expect("serializes");
        assert_eq!(value.get("forge").and_then(serde_json::Value::as_str), Some("gitlab"));
        assert_eq!(value.get("transport").and_then(serde_json::Value::as_str), Some("git"));
    }

    /// C-060 / C-069 / S-011 / S-036: `capability_checks` is non-empty on every
    /// run, `--out` included.
    ///
    /// C-069 makes an empty vector unrepresentable *inside* `PushAccess`, but
    /// the CLI can still render an empty array by filtering the `skipped` rows
    /// out — which is exactly what an "omit what does not apply" instinct
    /// produces, and exactly the case S-011 exists for.
    ///
    /// Red at the stub: `from_outcome` is `unimplemented!()`.
    /// Mutation once implemented: filter `status == "skipped"` out of the
    /// rendered array.
    #[test]
    fn capability_checks_are_non_empty_under_out() {
        let report = report(out_outcome(), ForgeKind::GitHub, WriteTransport::Api);
        assert_eq!(
            report.capability_checks.len(),
            4,
            "every capability row is reported, including the ones that did not apply"
        );
    }

    /// C-060: the plain table is exactly five columns, in this order.
    ///
    /// "Five columns" alone is satisfied by any five, so the headers are
    /// asserted as an **ordered sequence**. The arity is what forbids a sixth
    /// column — `owners`, `author`, `author_identity_source` and
    /// `capability_checks` are JSON-only — and
    /// the header/cell pairing is what forbids a table whose columns and values
    /// have drifted apart.
    ///
    /// Red at the stub: `plain_table` is `unimplemented!()`.
    /// Mutation once implemented: swap `Transport` and `Branch`; add an
    /// `Owners` column.
    #[test]
    fn claim_report_plain_is_five_columns() {
        let report = report(request_outcome(), ForgeKind::GitHub, WriteTransport::Api);
        let (headers, cells) = report.plain_table();
        assert_eq!(
            headers,
            vec!["Package", "Status", "Transport", "Branch", "Pull Request"],
            "C-060's five plain columns, in order"
        );
        assert_eq!(
            cells.len(),
            headers.len(),
            "one cell per column — a table whose header and value counts differ prints a lie"
        );
    }

    /// C-060: an absent field renders a dash, **and** a present one renders its
    /// value.
    ///
    /// Both halves are required and are asserted in one function so a builder
    /// cannot ship one: asserting the dash alone passes for a renderer that
    /// emits `-` unconditionally, and asserting the URL alone passes for one
    /// that emits an empty cell when there is none.
    ///
    /// Red at the stub: `plain_table` is `unimplemented!()`.
    /// Mutation once implemented: `unwrap_or_default()` instead of
    /// `unwrap_or_else(|| "-")` reds the absent half; hardcoding `"-"` reds the
    /// present half.
    #[test]
    fn plain_pull_request_cell_renders_a_dash_when_absent_and_the_url_when_present() {
        let absent = report(out_outcome(), ForgeKind::GitHub, WriteTransport::Api);
        let (_, cells) = absent.plain_table();
        assert_eq!(
            cells.last().map(String::as_str),
            Some("-"),
            "a run that opened no request renders a dash, never an empty cell"
        );

        let present = report(request_outcome(), ForgeKind::GitHub, WriteTransport::Api);
        let (_, cells) = present.plain_table();
        assert_eq!(
            cells.last().map(String::as_str),
            Some("https://github.com/ocx-sh/index/pull/42"),
            "a run that opened one renders its URL"
        );
    }
}
