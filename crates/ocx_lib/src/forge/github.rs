// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! GitHub REST forge client.
//!
//! Copy-and-own port of grimoire's GitHub forge flow (`src/catalog/forge.rs`,
//! same owner), transport-adjusted to REST-only and owned by OCX (design
//! register S5). Enforces the X5 invariants: a single no-redirect client per
//! run, bearer via header only, fork parent verified against the upstream,
//! endpoints rebuilt only from a response-body identity, a bounded readiness
//! poll, and a bounded replay of the whole commit sequence for GitHub's "fork
//! metadata ready before git objects" write race and for transient forge
//! faults. Commits are multi-file atomic via the git data API (design register
//! C15).

use std::collections::BTreeMap;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use bytes::Bytes;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

use super::error::status_detail;
use super::http::build_forge_http_client;
use super::identity::{verify_fork_namespace, verify_github_fork};
use super::poll::{PollSchedule, backoff_delays};
use super::{
    BranchComparison, CommitBase, Forge, ForgeError, ForgeToken, ForkIdentity, Mergeability, PullRequest, RefUpdate,
    RepoCoordinate,
};

/// Canonical github.com REST base URL — a dedicated API origin, not a path on
/// the web host. Overridable only under the test seam.
const DEFAULT_BASE_URL: &str = "https://api.github.com";
/// GitHub REST API version header value.
const API_VERSION: &str = "2022-11-28";
/// JSON media type for GitHub REST responses.
const ACCEPT_JSON: &str = "application/vnd.github+json";
/// Raw media type — returns file bytes directly from the contents API.
const ACCEPT_RAW: &str = "application/vnd.github.raw+json";
/// Total per-request timeout for ordinary forge calls.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Backoff delays before each replay of the git-data commit sequence.
///
/// Three replays, ~39s of waiting at worst: long enough to ride out a fresh
/// fork's object provisioning (design register X5) and a short forge fault,
/// short enough not to stretch a push job that has already published to the
/// registry and only owes the index its announce.
const GIT_DATA_RETRY_DELAYS: [Duration; 3] = [Duration::from_secs(3), Duration::from_secs(9), Duration::from_secs(27)];

/// The value GitHub's ref-update endpoint expects in its `force` field.
fn force_flag(update: RefUpdate) -> bool {
    matches!(update, RefUpdate::Reset)
}

/// GitHub REST forge client.
///
/// One no-redirect [`reqwest::Client`] per instance (design register X5); the
/// bearer token is applied per request as a header and never appears in a URL.
pub struct GitHubForge {
    client: reqwest::Client,
    token: ForgeToken,
    base_url: String,
}

impl GitHubForge {
    /// Build a client for `host` — `None` for github.com, `Some` for a GitHub
    /// Enterprise Server instance.
    ///
    /// The two differ in more than the hostname: github.com serves its API from
    /// a dedicated `api.github.com` origin, while Enterprise Server serves it
    /// from the instance itself under `/api/v3`. Composing the wrong one yields
    /// 404s that look like missing repositories.
    ///
    /// Under `cfg(any(test, feature = "__testing"))` the
    /// `__OCX_TESTING_FORGE_BASE_URL` env var redirects the client so the
    /// acceptance fake forge can intercept it; production ignores it.
    ///
    /// # Errors
    ///
    /// Returns [`ForgeError::ClientBuild`] when the hardened HTTP client cannot
    /// be constructed.
    pub fn new(token: ForgeToken, host: Option<&str>) -> Result<Self, ForgeError> {
        let base_url = testing_base_url_override().unwrap_or_else(|| api_base_url(host));
        Self::build(token, base_url)
    }

    /// Build a client against an explicit base URL (acceptance fake-forge seam).
    ///
    /// # Errors
    ///
    /// Returns [`ForgeError::ClientBuild`] when the hardened HTTP client cannot
    /// be constructed.
    #[cfg(any(test, feature = "__testing"))]
    pub fn with_base_url(token: ForgeToken, base_url: String) -> Result<Self, ForgeError> {
        Self::build(token, base_url)
    }

    fn build(token: ForgeToken, base_url: String) -> Result<Self, ForgeError> {
        Ok(Self {
            client: build_forge_http_client(REQUEST_TIMEOUT)?,
            token,
            base_url: base_url.trim_end_matches('/').to_string(),
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }

    /// An authorized request builder — bearer via header only (design register
    /// X5/X6); the token never enters the URL. The `Authorization` header is
    /// omitted entirely when the token is empty (the tokenless `--out` path),
    /// so the request reads as unauthenticated rather than sending a GitHub-
    /// rejected empty bearer.
    fn request(&self, method: Method, url: &str) -> reqwest::RequestBuilder {
        let builder = self
            .client
            .request(method, url)
            .header(ACCEPT, ACCEPT_JSON)
            .header("X-GitHub-Api-Version", API_VERSION);
        if self.token.0.is_empty() {
            builder
        } else {
            builder.header(AUTHORIZATION, format!("Bearer {}", self.token.0))
        }
    }

    fn json_request(&self, method: Method, url: &str, body: &Value) -> Result<reqwest::RequestBuilder, ForgeError> {
        let encoded = serde_json::to_vec(body).map_err(|source| ForgeError::RequestEncode { source })?;
        Ok(self
            .request(method, url)
            .header(CONTENT_TYPE, ACCEPT_JSON)
            .body(encoded))
    }

    async fn send(&self, request: reqwest::RequestBuilder, url: &str) -> Result<(StatusCode, Bytes), ForgeError> {
        let response = request.send().await.map_err(|source| ForgeError::Transport {
            url: url.to_string(),
            source,
        })?;
        let status = response.status();
        let body = response.bytes().await.map_err(|source| ForgeError::Transport {
            url: url.to_string(),
            source,
        })?;
        Ok((status, body))
    }

    /// A non-success status as an error, carrying the forge's own reason.
    ///
    /// GitHub puts the reason in the response body (`{"message": ...}`) and
    /// nowhere else, so every non-success return goes through here rather than
    /// dropping the body and reporting a bare number.
    fn status_error(&self, url: &str, status: StatusCode, body: &[u8]) -> ForgeError {
        ForgeError::Status {
            url: url.to_string(),
            status: status.as_u16(),
            detail: status_detail(body, self.token.0.as_str()),
        }
    }

    fn parse_json(url: &str, body: &[u8]) -> Result<Value, ForgeError> {
        serde_json::from_slice(body).map_err(|source| ForgeError::Decode {
            url: url.to_string(),
            source,
        })
    }

    /// GET a JSON resource. `None` on 404; error on any other non-success.
    async fn get_json_optional(&self, url: &str) -> Result<Option<Value>, ForgeError> {
        let (status, body) = self.send(self.request(Method::GET, url), url).await?;
        if status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(self.status_error(url, status, &body));
        }
        Ok(Some(Self::parse_json(url, &body)?))
    }

    /// POST a JSON body and parse the success response.
    async fn post_json(&self, url: &str, body: &Value) -> Result<Value, ForgeError> {
        let (status, response) = self.send(self.json_request(Method::POST, url, body)?, url).await?;
        if !status.is_success() {
            return Err(self.status_error(url, status, &response));
        }
        Self::parse_json(url, &response)
    }

    async fn authenticated_login(&self) -> Result<String, ForgeError> {
        let url = self.url("/user");
        let body = self
            .get_json_optional(&url)
            .await?
            .ok_or_else(|| ForgeError::MissingField {
                url: url.clone(),
                field: "login".to_string(),
            })?;
        body.get("login")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or(ForgeError::MissingField {
                url,
                field: "login".to_string(),
            })
    }

    /// Bounded readiness poll on the fork's own endpoint (rebuilt from the
    /// verified identity, never an API-returned URL). Each probe carries a short
    /// per-request timeout so one black-holed GET cannot eat the deadline.
    async fn wait_fork_ready(&self, identity: &ForkIdentity) -> Result<(), ForgeError> {
        let url = self.url(&format!("/repos/{}", identity.full_path));
        let schedule = PollSchedule::default();
        if self.probe_ready(&url, schedule.request_timeout).await {
            return Ok(());
        }
        for delay in backoff_delays(&schedule) {
            tokio::time::sleep(delay).await;
            if self.probe_ready(&url, schedule.request_timeout).await {
                return Ok(());
            }
        }
        Err(ForgeError::ForkNotReady {
            deadline_secs: schedule.deadline.as_secs(),
        })
    }

    async fn probe_ready(&self, url: &str, request_timeout: Duration) -> bool {
        matches!(
            self.request(Method::GET, url).timeout(request_timeout).send().await,
            Ok(response) if response.status().is_success()
        )
    }

    // ── git data API (multi-file atomic commit, design register C15) ──

    async fn base_tree_sha(&self, repo: &RepoCoordinate, base_sha: &str) -> Result<String, ForgeError> {
        let url = self.url(&format!("/repos/{}/git/commits/{base_sha}", repo.full_path()));
        // A 404 here is NOT a malformed response: this is the first request of
        // the commit sequence, and a brand-new fork's git object store may not
        // be provisioned yet (design register X5). Surface it as the status it
        // was so [`Self::commit_files`] can retry the whole sequence;
        // `MissingField` stays reserved for a 200 whose body lacks `tree.sha`.
        let body = self.get_json_optional(&url).await?.ok_or_else(|| ForgeError::Status {
            url: url.clone(),
            status: StatusCode::NOT_FOUND.as_u16(),
            // Synthesised from an absent resource, not from a response body:
            // there is no forge message to quote.
            detail: String::new(),
        })?;
        body.get("tree")
            .and_then(|tree| tree.get("sha"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or(ForgeError::MissingField {
                url,
                field: "tree.sha".to_string(),
            })
    }

    async fn create_blob(&self, repo: &RepoCoordinate, contents: &[u8]) -> Result<String, ForgeError> {
        let url = self.url(&format!("/repos/{}/git/blobs", repo.full_path()));
        // base64 so arbitrary CAS bytes round-trip, not just UTF-8 text.
        let body = json!({ "content": BASE64_STANDARD.encode(contents), "encoding": "base64" });
        let value = self.post_json(&url, &body).await?;
        object_sha(&url, &value)
    }

    async fn create_tree(
        &self,
        repo: &RepoCoordinate,
        base_tree_sha: &str,
        entries: Vec<Value>,
    ) -> Result<String, ForgeError> {
        let url = self.url(&format!("/repos/{}/git/trees", repo.full_path()));
        let body = json!({ "base_tree": base_tree_sha, "tree": entries });
        let value = self.post_json(&url, &body).await?;
        object_sha(&url, &value)
    }

    async fn create_commit(
        &self,
        repo: &RepoCoordinate,
        message: &str,
        tree_sha: &str,
        parent_sha: &str,
    ) -> Result<String, ForgeError> {
        let url = self.url(&format!("/repos/{}/git/commits", repo.full_path()));
        let body = json!({ "message": message, "tree": tree_sha, "parents": [parent_sha] });
        let value = self.post_json(&url, &body).await?;
        object_sha(&url, &value)
    }

    /// `POST /merge-upstream` — the request behind [`Forge::sync_fork`], kept
    /// separate from it so the commit retry can re-run the sync and report what
    /// it answered, which the trait method deliberately swallows.
    async fn merge_upstream(&self, fork: &RepoCoordinate, branch: &str) -> Result<(), ForgeError> {
        let url = self.url(&format!("/repos/{}/merge-upstream", fork.full_path()));
        self.post_json(&url, &json!({ "branch": branch })).await.map(|_| ())
    }

    /// Point `refs/heads/<branch>` at `commit_sha`, creating the ref when it
    /// does not yet exist.
    ///
    /// Under [`RefUpdate::FastForward`] — every ordinary announce — the update is
    /// a compare-and-swap (design register C4): a non-fast-forward, meaning a
    /// concurrent announce advanced the branch, surfaces as
    /// [`ForgeError::NonFastForward`] for the caller to re-read and retry, never
    /// a silent force-overwrite. Under [`RefUpdate::Reset`] the rewrite is the
    /// point, so no such rejection can occur.
    ///
    /// A rejected update costs one extra request: GitHub reports "absent ref"
    /// and "not a fast-forward" with the same 422, so the ref is read to tell
    /// them apart. That read is off the happy path entirely — a successful
    /// update returns without it.
    async fn upsert_branch(
        &self,
        repo: &RepoCoordinate,
        branch: &str,
        commit_sha: &str,
        update: RefUpdate,
    ) -> Result<(), ForgeError> {
        let update_url = self.url(&format!("/repos/{}/git/refs/heads/{branch}", repo.full_path()));
        let update_body = json!({ "sha": commit_sha, "force": force_flag(update) });
        let (status, update_response) = self
            .send(
                self.json_request(Method::PATCH, &update_url, &update_body)?,
                &update_url,
            )
            .await?;
        if status.is_success() {
            return Ok(());
        }
        // GitHub answers **422 for both** rejection modes of this endpoint: a
        // fast-forward-only update that is not an ancestor (the concurrent-
        // advance CAS case) AND a ref that does not exist at all (verified live:
        // `{"message":"Reference does not exist"}`, 422 — not the 404 the shape
        // of the endpoint suggests). The two are told apart by asking the ref
        // itself, never by matching GitHub's English prose, which is not a
        // stable API contract. 404 joins the same path: it is not observed for
        // an absent ref, but a fresh fork whose git objects are still
        // provisioning can answer it, and the probe classifies that as absent
        // too — so the create below runs and its own 404 drives the X5 retry,
        // exactly as before.
        if status != StatusCode::UNPROCESSABLE_ENTITY && status != StatusCode::NOT_FOUND {
            return Err(self.status_error(&update_url, status, &update_response));
        }
        if self.get_ref_sha(repo, &format!("heads/{branch}")).await?.is_some() {
            return Err(ForgeError::NonFastForward {
                branch: branch.to_string(),
            });
        }
        let create_url = self.url(&format!("/repos/{}/git/refs", repo.full_path()));
        let create_body = json!({ "ref": format!("refs/heads/{branch}"), "sha": commit_sha });
        let (create_status, create_response) = self
            .send(self.json_request(Method::POST, &create_url, &create_body)?, &create_url)
            .await?;
        if create_status.is_success() {
            return Ok(());
        }
        // A concurrent first announce created the branch between our probe and
        // this create — treat it as a CAS conflict and retry as an update.
        if create_status == StatusCode::UNPROCESSABLE_ENTITY {
            return Err(ForgeError::NonFastForward {
                branch: branch.to_string(),
            });
        }
        Err(self.status_error(&create_url, create_status, &create_response))
    }

    /// One attempt at the [`Forge::commit_files`] sequence: base tree -> blobs ->
    /// tree -> commit -> fast-forward-only ref update.
    async fn commit_files_once(
        &self,
        repo: &RepoCoordinate,
        branch: &str,
        base_sha: &str,
        message: &str,
        files: &BTreeMap<String, Vec<u8>>,
        update: RefUpdate,
    ) -> Result<String, ForgeError> {
        let base_tree_sha = self.base_tree_sha(repo, base_sha).await?;
        let mut tree_entries = Vec::with_capacity(files.len());
        for (path, contents) in files {
            let blob_sha = self.create_blob(repo, contents).await?;
            tree_entries.push(json!({ "path": path, "mode": "100644", "type": "blob", "sha": blob_sha }));
        }
        let tree_sha = self.create_tree(repo, &base_tree_sha, tree_entries).await?;
        let commit_sha = self.create_commit(repo, message, &tree_sha, base_sha).await?;
        self.upsert_branch(repo, branch, &commit_sha, update).await?;
        Ok(commit_sha)
    }
}

/// The GitHub REST realization of the announce operation set.
///
/// Every method below holds the [`Forge`] contract with GitHub's own wire
/// shapes; the notes on each are GitHub specifics, not restatements of the
/// contract, which lives on the trait.
#[async_trait::async_trait]
impl Forge for GitHubForge {
    /// Read a file's bytes at `r#ref`, or `None` when the path does not exist.
    ///
    /// # Errors
    ///
    /// Returns a [`ForgeError`] on transport failure, a non-success status
    /// other than 404, or a response-decode failure.
    async fn get_file_contents(
        &self,
        repo: &RepoCoordinate,
        path: &str,
        r#ref: &str,
    ) -> Result<Option<Vec<u8>>, ForgeError> {
        let url = self.url(&format!("/repos/{}/contents/{path}", repo.full_path()));
        let request = self
            .request(Method::GET, &url)
            .header(ACCEPT, ACCEPT_RAW)
            .query(&[("ref", r#ref)]);
        let (status, body) = self.send(request, &url).await?;
        if status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(self.status_error(&url, status, &body));
        }
        Ok(Some(body.to_vec()))
    }

    /// Resolve a git ref to its commit SHA, or `None` when the ref does not
    /// exist. `r#ref` is a ref path such as `heads/<branch>`.
    ///
    /// # Errors
    ///
    /// Returns a [`ForgeError`] on transport failure, a non-success status
    /// other than 404, or a missing `object.sha` field in the response.
    async fn get_ref_sha(&self, repo: &RepoCoordinate, r#ref: &str) -> Result<Option<String>, ForgeError> {
        let url = self.url(&format!("/repos/{}/git/ref/{}", repo.full_path(), r#ref));
        let Some(body) = self.get_json_optional(&url).await? else {
            return Ok(None);
        };
        let sha = body
            .get("object")
            .and_then(|object| object.get("sha"))
            .and_then(Value::as_str)
            .ok_or(ForgeError::MissingField {
                url,
                field: "object.sha".to_string(),
            })?;
        Ok(Some(sha.to_string()))
    }

    /// How `head_owner:head_branch` stands relative to `base`.
    ///
    /// One compare call answers ancestry exactly, which a bare ref-SHA equality
    /// check cannot. The four states are kept distinct rather than collapsed to
    /// a boolean because [`BranchComparison::Diverged`] and
    /// [`BranchComparison::Ahead`] demand *opposite* handling — see that type's
    /// documentation.
    ///
    /// Fails closed on anything else. A 404 here is an **indeterminate** compare,
    /// not a verdict: the only caller asks this after already observing that the
    /// announce branch exists, so neither ref should be unresolvable, and both a
    /// TOCTOU ref deletion and an inaccessible compare land on the same status.
    /// Reading it as "not ahead" would let the C6 unchanged path return success
    /// while a committed update sits on the branch with no pull request — the
    /// stranded-commit window the C6 amendment exists to close. Erroring is
    /// chosen over a second, equally racy pair of ref reads: two more requests
    /// still cannot distinguish "deleted" from "cannot see", and the caller's
    /// correct response to either is the same — surface it, do not silently
    /// report a clean no-op.
    ///
    /// # Errors
    ///
    /// Returns a [`ForgeError`] on transport failure, any non-success status
    /// (404 included), a missing `status` field, or a `status` value the client
    /// does not model.
    async fn compare_branch(
        &self,
        repo: &RepoCoordinate,
        base: &str,
        head: &RepoCoordinate,
        head_branch: &str,
    ) -> Result<BranchComparison, ForgeError> {
        let head_owner = require_flat_namespace(head)?;
        let url = self.url(&format!(
            "/repos/{}/compare/{base}...{head_owner}:{head_branch}",
            repo.full_path()
        ));
        let Some(body) = self.get_json_optional(&url).await? else {
            return Err(ForgeError::Status {
                url,
                status: StatusCode::NOT_FOUND.as_u16(),
                // Synthesised from an absent comparison, not a response body.
                detail: String::new(),
            });
        };
        let status = body
            .get("status")
            .and_then(Value::as_str)
            .ok_or_else(|| ForgeError::MissingField {
                url: url.clone(),
                field: "status".to_string(),
            })?;
        parse_compare_status(&url, status)
    }

    /// The open pull request whose head is `head`'s `branch`, or `None`.
    ///
    /// Read-only, and deliberately scoped to **open** pull requests: the
    /// announce branch is per package and outlives every pull request opened
    /// from it, so "a pull request exists" is not the same question as "this
    /// branch is still carrying one".
    ///
    /// # Errors
    ///
    /// Returns a [`ForgeError`] on transport failure, a non-success status, or
    /// a malformed pull-request response body.
    async fn find_open_pull_request(
        &self,
        index: &RepoCoordinate,
        head: &RepoCoordinate,
        branch: &str,
    ) -> Result<Option<PullRequest>, ForgeError> {
        let pulls_url = self.url(&format!("/repos/{}/pulls", index.full_path()));
        let head_spec = pr_head(require_flat_namespace(head)?, branch);
        let request = self
            .request(Method::GET, &pulls_url)
            .query(&[("head", head_spec.as_str()), ("state", "open")]);
        let (status, body) = self.send(request, &pulls_url).await?;
        if !status.is_success() {
            return Err(self.status_error(&pulls_url, status, &body));
        }
        let list = Self::parse_json(&pulls_url, &body)?;
        let Some(existing) = list.as_array().and_then(|pulls| pulls.first()) else {
            return Ok(None);
        };
        pull_request_from_body(&pulls_url, existing, true).map(Some)
    }

    /// Whether pull request `number` on `index` merges cleanly.
    ///
    /// The single-pull GET, not the list endpoint: `mergeable` is absent from
    /// every listed pull request, and this GET is also what *asks* GitHub to
    /// compute the merge commit in the first place. One request, no poll — a
    /// verdict GitHub has not finished computing is [`Mergeability::Unknown`],
    /// which the caller re-asks on the next run.
    ///
    /// # Errors
    ///
    /// Returns a [`ForgeError`] on transport failure or a non-success status
    /// other than 404.
    async fn pull_request_mergeability(&self, index: &RepoCoordinate, number: u64) -> Result<Mergeability, ForgeError> {
        let url = self.url(&format!("/repos/{}/pulls/{number}", index.full_path()));
        let Some(body) = self.get_json_optional(&url).await? else {
            return Ok(Mergeability::Unknown);
        };
        Ok(mergeability_from_body(&body))
    }

    /// Look up an existing fork of `upstream` at `fork`, **without creating
    /// one**. `None` when nothing is there, or when what is there is not a
    /// verified fork of `upstream` (a same-named stranger repository).
    ///
    /// Read-only by contract: the caller resolves the fork's real identity
    /// before deciding whether any write is needed at all, so a pure no-op run
    /// never provokes a fork create. It honours the full requested coordinate —
    /// a fork renamed away from the upstream's project name resolves here, where
    /// deriving the path from the upstream's own project name would miss it.
    ///
    /// # Errors
    ///
    /// Returns a [`ForgeError`] on transport failure, a non-success status
    /// other than 404, or a verified fork living under an unexpected owner.
    async fn find_fork(
        &self,
        upstream: &RepoCoordinate,
        fork: &RepoCoordinate,
    ) -> Result<Option<ForkIdentity>, ForgeError> {
        require_flat_namespace(fork)?;
        let url = self.url(&format!("/repos/{}", fork.full_path()));
        let Some(body) = self.get_json_optional(&url).await? else {
            return Ok(None);
        };
        // A same-named stranger repository is "no fork here", not a hard error:
        // the caller's create path is what refuses it (X5).
        let Ok(identity) = verify_github_fork(&body, upstream) else {
            return Ok(None);
        };
        verify_fork_namespace(&identity, &fork.namespace)?;
        Ok(Some(identity))
    }

    async fn ensure_fork(
        &self,
        upstream: &RepoCoordinate,
        target_owner: Option<&str>,
    ) -> Result<ForkIdentity, ForgeError> {
        // The fork must live under an explicit target namespace, else the token
        // identity. This anchor is verified against the fork's own identity.
        let expected_namespace = match target_owner {
            Some(owner) => owner.to_string(),
            None => self.authenticated_login().await?,
        };
        // A namespace cannot fork a repository it already owns. GitHub answers
        // that POST with an opaque 403, and the publisher's real intent — a pull
        // request from a branch on the index itself — is a different code path
        // entirely, so name it rather than letting the fork API refuse it.
        if expected_namespace.eq_ignore_ascii_case(&upstream.namespace) {
            return Err(ForgeError::SelfForkRefused {
                upstream: upstream.to_string(),
                namespace: expected_namespace,
            });
        }
        // Reuse a verified existing fork at the conventional path. A renamed
        // fork (or a same-named stranger) fails verification and falls through
        // to the idempotent create below.
        let conventional = upstream.with_namespace(expected_namespace.clone());
        if let Some(identity) = self.find_fork(upstream, &conventional).await? {
            return Ok(identity);
        }
        // Create (or adopt a renamed) fork; the identity is built ONLY from the
        // response body, never a composed `{namespace}/{project}`.
        let create_url = self.url(&format!("/repos/{}/forks", upstream.full_path()));
        let (status, body) = self
            .send(
                self.json_request(Method::POST, &create_url, &fork_create_body(target_owner))?,
                &create_url,
            )
            .await?;
        if !status.is_success() {
            return Err(self.status_error(&create_url, status, &body));
        }
        let value = Self::parse_json(&create_url, &body)?;
        let identity = verify_github_fork(&value, upstream)?;
        verify_fork_namespace(&identity, &expected_namespace)?;
        self.wait_fork_ready(&identity).await?;
        Ok(identity)
    }

    /// Fast-forward `fork`'s `branch` onto its upstream — GitHub's "Sync fork".
    ///
    /// An announce commit is written to the fork but parents off a SHA read
    /// from the **upstream** repository, so that object reaches the fork only
    /// through the shared fork network. A fork whose own branches never advance
    /// leans on that sharing for every write it ever makes, and GitHub answers
    /// those writes with a 5xx — or a 422 carrying no validation reason — once
    /// the fork has fallen far enough behind: on 2026-08-02 the shared announce
    /// fork stood 33 commits behind and every mirror in the fleet failed its
    /// announce against `POST /git/commits`. Syncing first puts the base commit
    /// in the fork's own history, so the git-data sequence stops depending on
    /// cross-repository object reach.
    ///
    /// Best-effort by construction: this only moves *where* the base object
    /// lives, so it is never a precondition of the commit that follows. A fork
    /// that has diverged from upstream answers 409, one with nothing to pull
    /// answers non-success too, and in both cases the commit sequence is
    /// unaffected — so a failure is logged and the announce proceeds.
    async fn sync_fork(&self, fork: &RepoCoordinate, branch: &str) {
        if let Err(error) = self.merge_upstream(fork, branch).await {
            // WARN, not debug: this is the single most useful fact when the ref
            // write later 404s, and CI runs at INFO — at debug a skipped sync was
            // invisible, so the 404 read as a credential, permission, or ruleset
            // fault and cost ~15 probes to rule all three out.
            tracing::warn!(
                %error,
                fork = %fork.full_path(),
                "fork sync failed; a fork behind upstream cannot reach the announce base commit"
            );
        }
    }

    /// Verify the credential may push a branch to `repo`, before anything is
    /// written there.
    ///
    /// The fork-free announce path commits onto the index repository itself, so
    /// a credential without push permission fails partway through the git-data
    /// sequence — and GitHub reports an unauthorised write as 404 at least as
    /// often as 403, which [`Self::commit_files`] would then mistake for the
    /// fresh-fork provisioning race, sleep 3s, replay the whole sequence, and
    /// finally surface a bare status code naming a URL. One read of the
    /// repository's own `permissions.push` collapses all of that into a named
    /// error before any write is attempted.
    ///
    /// `permissions` is only present on an authenticated read, and a repository
    /// the credential cannot see answers 404 rather than 403 — both mean the same
    /// thing to the caller, so both land on the same error.
    ///
    /// # Errors
    ///
    /// Returns [`ForgeError::PushAccessDenied`] when the repository is invisible
    /// to the credential or reports no push permission, or any other
    /// [`ForgeError`] on transport, status, or decode failure.
    async fn ensure_push_access(&self, repo: &RepoCoordinate) -> Result<(), ForgeError> {
        let url = self.url(&format!("/repos/{}", repo.full_path()));
        let allowed = self
            .get_json_optional(&url)
            .await?
            .and_then(|body| body.get("permissions")?.get("push")?.as_bool())
            .unwrap_or(false);
        if allowed {
            return Ok(());
        }
        Err(ForgeError::PushAccessDenied { repo: repo.full_path() })
    }

    /// Commit `files` atomically onto `branch` at `base_sha`, returning the new
    /// commit SHA. One commit carries the root plus every CAS file via the git
    /// data API — never a loop over the single-file contents API (design
    /// register C15).
    ///
    /// The **whole** sequence is retried once, after a fixed delay, when any of
    /// its requests 404s. That absorbs GitHub's fresh-fork race: a fork's
    /// metadata reads ready before its git objects finish provisioning, so the
    /// readiness poll (which asks `GET /repos/{fork}` — metadata) can pass while
    /// the git object store still answers 404. The race is not confined to one
    /// request: the sequence *opens* with `GET /git/commits/{base_sha}`, so
    /// wrapping only a later write would leave the first two requests exposed.
    /// A settled fork never 404s here, so the retry only ever fires inside the
    /// provisioning window (design register X5). Every step is idempotent — git
    /// objects are content-addressed and nothing is published until the final
    /// ref update — so replaying the sequence cannot double-commit.
    ///
    /// # Errors
    ///
    /// Returns a [`ForgeError`] on transport failure, a non-success status, or
    /// a malformed git-data-API response. A rejected fast-forward-only ref
    /// update surfaces as [`ForgeError::NonFastForward`] and is **not** retried
    /// here — it needs the caller's re-read-and-regenerate (design register C4).
    async fn commit_files(
        &self,
        repo: &RepoCoordinate,
        branch: &str,
        base: CommitBase<'_>,
        message: &str,
        files: &BTreeMap<String, Vec<u8>>,
        update: RefUpdate,
    ) -> Result<String, ForgeError> {
        let mut outcome = self
            .commit_files_once(repo, branch, base.sha, message, files, update)
            .await;
        let cross_repo_base = base.repo != repo;
        let mut sync: Option<String> = None;
        for delay in GIT_DATA_RETRY_DELAYS {
            let Err(error) = &outcome else { break };
            if !is_retryable(error) {
                break;
            }
            tracing::debug!(%error, "replaying the git-data commit sequence");
            tokio::time::sleep(delay).await;
            // A base in another repository reaches this one only through the
            // shared fork network (see `sync_fork`), so a fork that has fallen
            // behind fails every replay identically — replaying blind waits out
            // three delays and surfaces the same status. Re-sync first, and keep
            // what the sync said for the error below.
            if cross_repo_base {
                sync = Some(match self.merge_upstream(repo, base.branch).await {
                    Ok(()) => "ok".to_string(),
                    Err(error) => error.to_string(),
                });
            }
            outcome = self
                .commit_files_once(repo, branch, base.sha, message, files, update)
                .await;
        }
        if let Err(error) = &outcome
            && let Some(named) = fork_base_unreachable(error, repo, base, sync.as_deref())
        {
            return Err(named);
        }
        outcome
    }

    /// Open a pull request from `head`'s `branch` into `index`'s `base`, or
    /// reuse the existing open one (never duplicate — design register C9).
    ///
    /// # Errors
    ///
    /// Returns a [`ForgeError`] on transport failure, a non-success status
    /// other than the "pull request already exists" 422/409, or a malformed
    /// pull-request response body.
    async fn open_or_update_pull_request(
        &self,
        index: &RepoCoordinate,
        head: &RepoCoordinate,
        branch: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<PullRequest, ForgeError> {
        let pulls_url = self.url(&format!("/repos/{}/pulls", index.full_path()));
        let head_spec = pr_head(require_flat_namespace(head)?, branch);
        let create_body = json!({ "title": title, "head": head_spec, "base": base, "body": body });
        let (status, response) = self
            .send(self.json_request(Method::POST, &pulls_url, &create_body)?, &pulls_url)
            .await?;
        if status.is_success() {
            let value = Self::parse_json(&pulls_url, &response)?;
            return pull_request_from_body(&pulls_url, &value, false);
        }
        // 409, or a 422 that says so, = "a pull request already exists" — the
        // branch update already refreshed it; reuse the open one, never
        // duplicate. Any other 422 (notably "No commits between base and head")
        // is a genuine failure and must not fall into list-and-reuse.
        let already_exists =
            status.as_u16() == 409 || (status.as_u16() == 422 && pull_request_already_exists(&response));
        if !already_exists {
            return Err(self.status_error(&pulls_url, status, &response));
        }
        self.find_open_pull_request(index, head, branch)
            .await?
            .ok_or_else(|| ForgeError::MissingField {
                url: pulls_url,
                field: "pull_request".to_string(),
            })
    }
}

/// The REST base URL for a GitHub host.
///
/// github.com answers on `api.github.com`; every Enterprise Server instance
/// answers on itself under `/api/v3`. Always https: the announce credential
/// rides these requests as a bearer header, and a plaintext scheme would put it
/// on the wire.
fn api_base_url(host: Option<&str>) -> String {
    match host {
        None => DEFAULT_BASE_URL.to_string(),
        Some(host) if host.eq_ignore_ascii_case("github.com") || host.eq_ignore_ascii_case("api.github.com") => {
            DEFAULT_BASE_URL.to_string()
        }
        Some(host) => format!("https://{host}/api/v3"),
    }
}

/// The fork-create request body: `organization` when a target owner is named
/// (design register S12), an empty object for the token-identity default.
fn fork_create_body(target_owner: Option<&str>) -> Value {
    match target_owner {
        Some(owner) => json!({ "organization": owner }),
        None => json!({}),
    }
}

/// The GitHub owner of a coordinate, refusing a nested namespace.
///
/// GitHub organizations do not nest: every repository is exactly `owner/repo`.
/// The coordinate type is forge-neutral and permits `a/b/c` so GitLab subgroups
/// are expressible, so the flatness rule lives here, at the one forge that has
/// it — and it is enforced before any request, since GitHub would otherwise
/// answer a nested path with a bare 404 that reads as "no such repository"
/// rather than "that path shape cannot exist here".
pub(super) fn require_flat_namespace(coordinate: &RepoCoordinate) -> Result<&str, ForgeError> {
    if coordinate.namespace.contains('/') {
        return Err(ForgeError::NestedNamespaceUnsupported {
            forge: "GitHub".to_string(),
            namespace: coordinate.namespace.clone(),
        });
    }
    Ok(&coordinate.namespace)
}

/// The cross-repo pull-request head, `owner:branch`.
fn pr_head(head_owner: &str, branch: &str) -> String {
    format!("{head_owner}:{branch}")
}

/// Classify a compare response's `status` into "carries unmerged commits".
///
/// Exhaustive over GitHub's documented four values; an unmodelled value is an
/// error, never a guess — a wrong "not ahead" strands a committed announce with
/// no pull request (design register C6 amendment).
fn parse_compare_status(url: &str, status: &str) -> Result<BranchComparison, ForgeError> {
    match status {
        "identical" => Ok(BranchComparison::Identical),
        "ahead" => Ok(BranchComparison::Ahead),
        "behind" => Ok(BranchComparison::Behind),
        "diverged" => Ok(BranchComparison::Diverged),
        unknown => Err(ForgeError::UnknownCompareStatus {
            url: url.to_string(),
            status: unknown.to_string(),
        }),
    }
}

/// Whether a failed git-data attempt is worth replaying.
///
/// A 404 is GitHub's "fork metadata ready before git objects" provisioning
/// window (design register X5). A 429 or a 5xx is throttling or a server-side
/// fault: on 2026-08-02 the shared announce fork answered `POST /git/commits`
/// with 500 for every mirror in the fleet, and each run had already published
/// to the registry by then, so giving up left the registry ahead of the index.
/// Replaying the whole sequence is safe — blobs and trees are content-addressed
/// so a repeat write returns the same SHA, and the ref update stays a
/// compare-and-swap.
///
/// A 422 is deliberately NOT replayed. GitHub spends it on both "the endpoint
/// has been spammed" and genuine validation failure, and nothing outside the
/// response body tells them apart; replaying the latter only defers the same
/// error. A rejected fast-forward needs the caller's regeneration against the
/// winning head, never a blind replay of the same commit.
/// Rename a spent-retry 404 on a base that lives in another repository into its
/// cause.
///
/// Only that shape: the base object reaches the target only through the shared
/// fork network, so a 404 there is what a fork left behind upstream looks like
/// from the git-data API — not a missing repository, and not the credential,
/// permission, or ruleset fault a bare status naming an endpoint reads as.
/// `sync` is what the last re-sync answered, which is the fact that explains it.
fn fork_base_unreachable(
    error: &ForgeError,
    target: &RepoCoordinate,
    base: CommitBase<'_>,
    sync: Option<&str>,
) -> Option<ForgeError> {
    let ForgeError::Status { status, .. } = error else {
        return None;
    };
    if base.repo == target || *status != StatusCode::NOT_FOUND.as_u16() {
        return None;
    }
    Some(ForgeError::ForkBaseUnreachable {
        fork: target.full_path(),
        branch: base.branch.to_string(),
        sync: sync.unwrap_or("not attempted").to_string(),
    })
}

fn is_retryable(error: &ForgeError) -> bool {
    matches!(
        error,
        ForgeError::Status { status, .. }
            if *status == StatusCode::NOT_FOUND.as_u16()
                || *status == StatusCode::TOO_MANY_REQUESTS.as_u16()
                || (500..600).contains(status)
    )
}

/// Whether a 422 from pull-request create means "one already exists".
///
/// GitHub answers 422 for that **and** for "No commits between base and head".
/// Only the former may fall through to list-and-reuse: the latter finds no open
/// pull request and would surface as a misleading `MissingField`
/// (`pull_request`) instead of the real reason.
fn pull_request_already_exists(body: &[u8]) -> bool {
    String::from_utf8_lossy(body).to_lowercase().contains("already exists")
}

fn object_sha(url: &str, value: &Value) -> Result<String, ForgeError> {
    value
        .get("sha")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| ForgeError::MissingField {
            url: url.to_string(),
            field: "sha".to_string(),
        })
}

fn pull_request_from_body(url: &str, value: &Value, updated: bool) -> Result<PullRequest, ForgeError> {
    let number = value
        .get("number")
        .and_then(Value::as_u64)
        .ok_or_else(|| ForgeError::MissingField {
            url: url.to_string(),
            field: "number".to_string(),
        })?;
    let html_url = value
        .get("html_url")
        .and_then(Value::as_str)
        .ok_or_else(|| ForgeError::MissingField {
            url: url.to_string(),
            field: "html_url".to_string(),
        })?
        .to_string();
    Ok(PullRequest {
        number,
        html_url,
        updated,
    })
}

/// GitHub's `mergeable` tri-state as a [`Mergeability`].
///
/// The field is `null` — present but empty — for as long as GitHub is computing
/// the background merge commit, and the first GET of a pull request is what
/// starts that computation. Reading `null` as either verdict is therefore
/// wrong in opposite directions: as `false` it reports a conflict on a
/// perfectly mergeable request the very first time it is asked, as `true` it
/// clears a conflicting one. Absent is treated the same as `null`, since a
/// shape that carries no field carries no verdict either.
fn mergeability_from_body(value: &Value) -> Mergeability {
    match value.get("mergeable").and_then(Value::as_bool) {
        Some(true) => Mergeability::Mergeable,
        Some(false) => Mergeability::Conflicting,
        None => Mergeability::Unknown,
    }
}

#[cfg(any(test, feature = "__testing"))]
fn testing_base_url_override() -> Option<String> {
    std::env::var("__OCX_TESTING_FORGE_BASE_URL").ok()
}

#[cfg(not(any(test, feature = "__testing")))]
fn testing_base_url_override() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::{TcpListener, TcpStream};

    use super::*;

    /// One recorded request against [`FakeForge`].
    #[derive(Clone)]
    struct Recorded {
        method: String,
        path: String,
        body: String,
    }

    impl Recorded {
        fn route(&self) -> String {
            format!("{} {}", self.method, self.path)
        }
    }

    /// A one-request-per-connection HTTP/1.1 fake for the forge endpoints.
    ///
    /// The real `reqwest` stack is driven end to end rather than a transport
    /// seam: what is under test is a *status-code* classification, and a fake
    /// above the HTTP layer would have to restate the very mapping the tests
    /// exist to pin. Every response carries `connection: close`, so the handler
    /// sees one request per connection, in order, and can answer the same URL
    /// differently on a later call.
    struct FakeForge {
        base_url: String,
        calls: Arc<Mutex<Vec<Recorded>>>,
    }

    impl FakeForge {
        /// Bind an ephemeral loopback port and serve `handler`, which maps
        /// (method, path) to a (status, JSON body) response.
        async fn start(
            handler: impl Fn(&str, &str) -> (u16, String) + Send + Sync + 'static,
        ) -> Result<Self, std::io::Error> {
            let listener = TcpListener::bind("127.0.0.1:0").await?;
            let base_url = format!("http://{}", listener.local_addr()?);
            let calls = Arc::new(Mutex::new(Vec::new()));
            let recorder = Arc::clone(&calls);
            let handler = Arc::new(handler);
            tokio::spawn(async move {
                while let Ok((mut stream, _)) = listener.accept().await {
                    let handler = Arc::clone(&handler);
                    let recorder = Arc::clone(&recorder);
                    tokio::spawn(async move {
                        if let Some(request) = read_request(&mut stream).await {
                            let (status, body) = handler(&request.method, &request.path);
                            if let Ok(mut calls) = recorder.lock() {
                                calls.push(request);
                            }
                            let _ = write_response(&mut stream, status, &body).await;
                        }
                    });
                }
            });
            Ok(Self { base_url, calls })
        }

        fn forge(&self) -> GitHubForge {
            GitHubForge::with_base_url(ForgeToken::new("token".to_string()), self.base_url.clone())
                .expect("client builds")
        }

        fn recorded(&self) -> Vec<Recorded> {
            self.calls.lock().expect("recorder not poisoned").clone()
        }

        fn routes(&self) -> Vec<String> {
            self.recorded().iter().map(Recorded::route).collect()
        }
    }

    /// Read one request: the head byte-at-a-time to the `\r\n\r\n` boundary,
    /// then exactly `content-length` body bytes. Draining the body matters —
    /// closing a socket with bytes still queued makes the kernel answer RST,
    /// which discards the response already written.
    async fn read_request(stream: &mut TcpStream) -> Option<Recorded> {
        let mut head = Vec::new();
        let mut byte = [0_u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).await.ok()?;
            head.push(byte[0]);
        }
        let head = String::from_utf8_lossy(&head).into_owned();
        let mut request_line = head.lines().next()?.split_whitespace();
        let method = request_line.next()?.to_string();
        let path = request_line.next()?.to_string();
        let length = head
            .to_ascii_lowercase()
            .lines()
            .find_map(|line| {
                line.strip_prefix("content-length:")
                    .map(str::trim)
                    .and_then(|v| v.parse().ok())
            })
            .unwrap_or(0_usize);
        let mut body = vec![0_u8; length];
        if length > 0 {
            stream.read_exact(&mut body).await.ok()?;
        }
        Some(Recorded {
            method,
            path,
            body: String::from_utf8_lossy(&body).into_owned(),
        })
    }

    async fn write_response(stream: &mut TcpStream, status: u16, body: &str) -> Result<(), std::io::Error> {
        let response = format!(
            "HTTP/1.1 {status} Status\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await?;
        stream.shutdown().await
    }

    fn test_repo() -> RepoCoordinate {
        RepoCoordinate {
            host: None,
            namespace: "forkuser".to_string(),
            project: "index".to_string(),
        }
    }

    const BRANCH: &str = "indexbot-announce-acme-widget";
    const UPDATE_PATH: &str = "/repos/forkuser/index/git/refs/heads/indexbot-announce-acme-widget";
    const PROBE_PATH: &str = "/repos/forkuser/index/git/ref/heads/indexbot-announce-acme-widget";
    const CREATE_PATH: &str = "/repos/forkuser/index/git/refs";
    const MERGE_UPSTREAM_PATH: &str = "/repos/forkuser/index/merge-upstream";
    const BASE_COMMIT_PATH: &str = "/repos/forkuser/index/git/commits/basesha";
    const BLOBS_PATH: &str = "/repos/forkuser/index/git/blobs";
    const TREES_PATH: &str = "/repos/forkuser/index/git/trees";
    const COMMITS_PATH: &str = "/repos/forkuser/index/git/commits";
    /// GitHub's real answer to a PATCH of a ref that does not exist — a 422,
    /// not the 404 the endpoint shape suggests.
    const REFERENCE_DOES_NOT_EXIST: &str = r#"{"message":"Reference does not exist"}"#;

    #[tokio::test(flavor = "multi_thread")]
    async fn upsert_branch_creates_the_branch_when_a_422_means_the_ref_is_absent() {
        // The first announce for a package: the announce branch does not exist
        // on the fork yet. Reading that 422 as a non-fast-forward sent the
        // caller into the C4 retry against a branch with no head at all.
        let fake = FakeForge::start(|method, path| match (method, path) {
            ("PATCH", UPDATE_PATH) => (422, REFERENCE_DOES_NOT_EXIST.to_string()),
            ("GET", PROBE_PATH) => (404, r#"{"message":"Not Found"}"#.to_string()),
            ("POST", CREATE_PATH) => (201, r#"{"ref":"refs/heads/b","object":{"sha":"newsha"}}"#.to_string()),
            _ => (500, r#"{"message":"unexpected request"}"#.to_string()),
        })
        .await
        .expect("fake forge starts");

        fake.forge()
            .upsert_branch(&test_repo(), BRANCH, "commitsha", RefUpdate::FastForward)
            .await
            .expect("an absent ref is created, not reported as a conflict");

        assert_eq!(
            fake.routes(),
            [
                format!("PATCH {UPDATE_PATH}"),
                format!("GET {PROBE_PATH}"),
                format!("POST {CREATE_PATH}"),
            ]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn upsert_branch_reports_non_fast_forward_when_the_ref_is_present() {
        // The same 422, but the ref resolves: a concurrent announce advanced the
        // branch, so the caller must re-read and regenerate (design register C4).
        let fake = FakeForge::start(|method, path| match (method, path) {
            ("PATCH", UPDATE_PATH) => (422, r#"{"message":"Update is not a fast forward"}"#.to_string()),
            ("GET", PROBE_PATH) => (200, r#"{"object":{"sha":"headsha"}}"#.to_string()),
            _ => (500, r#"{"message":"unexpected request"}"#.to_string()),
        })
        .await
        .expect("fake forge starts");

        let error = fake
            .forge()
            .upsert_branch(&test_repo(), BRANCH, "commitsha", RefUpdate::FastForward)
            .await
            .expect_err("a present ref that rejected the update is a conflict");

        assert!(matches!(error, ForgeError::NonFastForward { ref branch } if branch == BRANCH));
        assert_eq!(
            fake.routes(),
            [format!("PATCH {UPDATE_PATH}"), format!("GET {PROBE_PATH}")],
            "a conflict must never fall through to create"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn upsert_branch_probes_nothing_when_the_update_succeeds() {
        let fake = FakeForge::start(|method, path| match (method, path) {
            ("PATCH", UPDATE_PATH) => (200, r#"{"object":{"sha":"commitsha"}}"#.to_string()),
            _ => (500, r#"{"message":"unexpected request"}"#.to_string()),
        })
        .await
        .expect("fake forge starts");

        fake.forge()
            .upsert_branch(&test_repo(), BRANCH, "commitsha", RefUpdate::FastForward)
            .await
            .expect("a successful update needs nothing else");

        assert_eq!(fake.routes(), [format!("PATCH {UPDATE_PATH}")]);
        // The update is fast-forward-only compare-and-swap (design register C4):
        // `force` must be stated false, never omitted and never true.
        let update: Value =
            serde_json::from_str(&fake.recorded()[0].body).expect("the update body is the JSON we sent");
        assert_eq!(update, json!({ "sha": "commitsha", "force": false }));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn upsert_branch_reports_non_fast_forward_when_the_create_loses_the_race() {
        // A concurrent first announce created the branch between our probe and
        // our create; the caller re-reads that head rather than overwriting it.
        let fake = FakeForge::start(|method, path| match (method, path) {
            ("PATCH", UPDATE_PATH) => (422, REFERENCE_DOES_NOT_EXIST.to_string()),
            ("GET", PROBE_PATH) => (404, r#"{"message":"Not Found"}"#.to_string()),
            ("POST", CREATE_PATH) => (422, r#"{"message":"Reference already exists"}"#.to_string()),
            _ => (500, r#"{"message":"unexpected request"}"#.to_string()),
        })
        .await
        .expect("fake forge starts");

        let error = fake
            .forge()
            .upsert_branch(&test_repo(), BRANCH, "commitsha", RefUpdate::FastForward)
            .await
            .expect_err("a lost create race is a conflict");

        assert!(matches!(error, ForgeError::NonFastForward { ref branch } if branch == BRANCH));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn upsert_branch_surfaces_other_statuses_without_probing_or_creating() {
        // A 401/403/500 says nothing about whether the ref exists. Probing (and
        // worse, creating) on one would turn "cannot see this repository" into
        // a write attempt against it.
        let fake = FakeForge::start(|method, path| match (method, path) {
            ("PATCH", UPDATE_PATH) => (403, r#"{"message":"Resource not accessible"}"#.to_string()),
            _ => (500, r#"{"message":"unexpected request"}"#.to_string()),
        })
        .await
        .expect("fake forge starts");

        let error = fake
            .forge()
            .upsert_branch(&test_repo(), BRANCH, "commitsha", RefUpdate::FastForward)
            .await
            .expect_err("an unmodelled status must surface");

        assert!(matches!(error, ForgeError::Status { status, .. } if status == 403));
        assert_eq!(fake.routes(), [format!("PATCH {UPDATE_PATH}")]);
    }

    #[test]
    fn fork_create_body_includes_organization_when_targeted() {
        assert_eq!(
            fork_create_body(Some("ocx-contrib")),
            json!({ "organization": "ocx-contrib" })
        );
    }

    #[test]
    fn fork_create_body_omits_organization_for_the_token_identity() {
        assert_eq!(fork_create_body(None), json!({}));
    }

    #[test]
    fn pr_head_is_a_cross_repo_head() {
        assert_eq!(
            pr_head("forkuser", "indexbot-announce-ns-pkg"),
            "forkuser:indexbot-announce-ns-pkg"
        );
    }

    #[test]
    fn compare_status_maps_each_github_value_to_its_own_state() {
        // `ahead` and `diverged` must NOT collapse together: appending to an
        // `ahead` branch fast-forwards, appending to a `diverged` one re-proposes
        // squash-merged commits and conflicts (#228).
        for (status, expected) in [
            ("identical", BranchComparison::Identical),
            ("ahead", BranchComparison::Ahead),
            ("behind", BranchComparison::Behind),
            ("diverged", BranchComparison::Diverged),
        ] {
            assert_eq!(
                parse_compare_status("https://api", status).expect("modelled"),
                expected,
                "status {status}"
            );
        }
    }

    #[test]
    fn compare_status_unmodelled_value_errors_instead_of_guessing() {
        // Every wrong verdict here is silent: guessing "spent" discards an
        // unmerged announce, guessing "live" rebuilds the #228 conflict. An
        // unrecognized value must surface, never default.
        let error = parse_compare_status("https://api", "sideways").expect_err("unmodelled status must error");
        assert!(matches!(
            error,
            ForgeError::UnknownCompareStatus { ref status, .. } if status == "sideways"
        ));
    }

    #[test]
    fn only_a_reset_forces_the_ref_update() {
        assert!(!force_flag(RefUpdate::FastForward));
        assert!(force_flag(RefUpdate::Reset));
    }

    #[test]
    fn provisioning_and_transient_statuses_replay_but_validation_failures_do_not() {
        // 404 = the fresh fork's objects are not there yet; 429/5xx = the forge
        // itself is throttling or broken. Both answer differently on a replay.
        for transient in [404, 429, 500, 502, 503] {
            let error = ForgeError::Status {
                url: "https://api/git/commits".to_string(),
                status: transient,
                detail: String::new(),
            };
            assert!(is_retryable(&error), "{transient} must replay");
        }
        // 401/403 will answer the same forever. 422 is GitHub's one code for
        // both "spammed" and "your request is wrong" — replaying the second
        // only defers it, and nothing but the body separates them.
        for terminal in [401, 403, 422] {
            let error = ForgeError::Status {
                url: "https://api/git/trees".to_string(),
                status: terminal,
                detail: String::new(),
            };
            assert!(!is_retryable(&error), "{terminal} must not replay");
        }
        // A rejected fast-forward-only update needs the caller's regeneration,
        // never a blind replay of the same commit.
        assert!(!is_retryable(&ForgeError::NonFastForward {
            branch: "indexbot-announce-acme-widget".to_string(),
        }));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sync_fork_fast_forwards_the_named_branch_onto_upstream() {
        // Without this the announce commit's parent lives only in the upstream
        // repository, and every git-data write leans on fork-network sharing.
        let fake = FakeForge::start(|method, path| match (method, path) {
            ("POST", MERGE_UPSTREAM_PATH) => (200, r#"{"merge_type":"fast-forward"}"#.to_string()),
            _ => (599, r#"{"message":"unexpected request"}"#.to_string()),
        })
        .await
        .expect("fake forge starts");

        fake.forge().sync_fork(&test_repo(), "main").await;

        assert_eq!(fake.routes(), [format!("POST {MERGE_UPSTREAM_PATH}")]);
        let body: Value = serde_json::from_str(&fake.recorded()[0].body).expect("the sync body is the JSON we sent");
        assert_eq!(body, json!({ "branch": "main" }));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sync_fork_tolerates_a_fork_that_cannot_fast_forward() {
        // 409 = diverged. The sync only decides where the base object lives, so
        // a refusal must not abort an announce that would otherwise commit.
        let fake = FakeForge::start(|_method, _path| (409, r#"{"message":"There are merge conflicts"}"#.to_string()))
            .await
            .expect("fake forge starts");

        fake.forge().sync_fork(&test_repo(), "main").await;

        assert_eq!(fake.routes(), [format!("POST {MERGE_UPSTREAM_PATH}")]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn commit_files_resyncs_the_fork_before_replaying_the_sequence() {
        // The 2026-08-22 announce failures: the fork sat behind upstream, so the
        // base commit read from the index was out of reach and the sequence
        // 404'd. Replaying it unchanged asks the same unreachable object again —
        // the sync that makes it reachable has to run between the attempts.
        let attempts = Arc::new(Mutex::new(0_usize));
        let seen = Arc::clone(&attempts);
        let fake = FakeForge::start(move |method, path| match (method, path) {
            ("POST", MERGE_UPSTREAM_PATH) => (200, r#"{"merge_type":"fast-forward"}"#.to_string()),
            ("GET", BASE_COMMIT_PATH) => {
                let mut attempts = seen.lock().expect("counter not poisoned");
                *attempts += 1;
                if *attempts == 1 {
                    // What a base object the fork cannot reach answers.
                    (404, r#"{"message":"Not Found"}"#.to_string())
                } else {
                    (200, r#"{"tree":{"sha":"treesha"}}"#.to_string())
                }
            }
            ("POST", BLOBS_PATH | TREES_PATH | COMMITS_PATH) => (201, r#"{"sha":"newsha"}"#.to_string()),
            ("PATCH", UPDATE_PATH) => (200, r#"{"object":{"sha":"newsha"}}"#.to_string()),
            _ => (599, r#"{"message":"unexpected request"}"#.to_string()),
        })
        .await
        .expect("fake forge starts");
        let upstream = RepoCoordinate {
            host: None,
            namespace: "acme".to_string(),
            project: "index".to_string(),
        };
        let files = BTreeMap::from([("packages/acme/widget.json".to_string(), b"{}".to_vec())]);

        let commit = fake
            .forge()
            .commit_files(
                &test_repo(),
                BRANCH,
                CommitBase {
                    repo: &upstream,
                    sha: "basesha",
                    branch: "main",
                },
                "announce acme/widget",
                &files,
                RefUpdate::FastForward,
            )
            .await
            .expect("the replay commits once the fork can reach the base");

        assert_eq!(commit, "newsha");
        assert_eq!(
            fake.routes(),
            [
                format!("GET {BASE_COMMIT_PATH}"),
                format!("POST {MERGE_UPSTREAM_PATH}"),
                format!("GET {BASE_COMMIT_PATH}"),
                format!("POST {BLOBS_PATH}"),
                format!("POST {TREES_PATH}"),
                format!("POST {COMMITS_PATH}"),
                format!("PATCH {UPDATE_PATH}"),
            ]
        );
    }

    #[test]
    fn a_spent_404_on_a_cross_repository_base_is_named_not_left_as_a_status() {
        // A bare status naming an endpoint pointed every investigation at
        // credentials, permissions, and rulesets — none of them involved.
        let upstream = RepoCoordinate {
            host: None,
            namespace: "acme".to_string(),
            project: "index".to_string(),
        };
        let base = |repo| CommitBase {
            repo,
            sha: "basesha",
            branch: "main",
        };
        let status = |code: u16| ForgeError::Status {
            url: "https://api/repos/forkuser/index/git/refs".to_string(),
            status: code,
            detail: String::new(),
        };

        let fork = test_repo();
        let named = fork_base_unreachable(&status(404), &fork, base(&upstream), Some("HTTP status 409"))
            .expect("a 404 on a base in another repository is the fork-behind shape");
        let rendered = named.to_string();
        assert!(rendered.contains("forkuser/index"), "{rendered}");
        assert!(rendered.contains("main"), "{rendered}");
        assert!(
            rendered.contains("409"),
            "the sync's own answer is the fact that explains it: {rendered}"
        );

        // A base in the repository being committed to cannot be a fork-reach
        // problem, and a non-404 is some other fault — both keep their status.
        assert!(fork_base_unreachable(&status(404), &fork, base(&fork), None).is_none());
        assert!(fork_base_unreachable(&status(500), &fork, base(&upstream), None).is_none());
    }

    #[test]
    fn pull_request_422_is_reused_only_when_the_body_says_it_already_exists() {
        assert!(pull_request_already_exists(
            br#"{"message":"Validation Failed","errors":[{"message":"A pull request already exists for forkuser:branch."}]}"#
        ));
        // The other 422 GitHub answers with — it must NOT fall into
        // list-and-reuse, which would find nothing and report a missing field.
        assert!(!pull_request_already_exists(
            br#"{"message":"Validation Failed","errors":[{"message":"No commits between main and forkuser:branch"}]}"#
        ));
    }

    #[test]
    fn pull_request_from_body_reads_number_and_url() {
        let value = json!({ "number": 42, "html_url": "https://example.test/pull/42" });
        let pull_request = pull_request_from_body("https://api", &value, true).expect("valid body");
        assert_eq!(pull_request.number, 42);
        assert_eq!(pull_request.html_url, "https://example.test/pull/42");
        assert!(pull_request.updated);
    }

    #[test]
    fn mergeability_reads_githubs_tri_state() {
        // `true` and `false` are verdicts; `null` is GitHub still computing the
        // background merge commit, and an absent field carries no verdict
        // either. Each of the three must land on its own variant — a parser
        // that collapsed `null` into a verdict would either invent a conflict
        // on the first look at a clean pull request or clear a real one.
        assert_eq!(
            mergeability_from_body(&json!({ "number": 42, "mergeable": true })),
            Mergeability::Mergeable
        );
        assert_eq!(
            mergeability_from_body(&json!({ "number": 42, "mergeable": false })),
            Mergeability::Conflicting
        );
        assert_eq!(
            mergeability_from_body(&json!({ "number": 42, "mergeable": Value::Null })),
            Mergeability::Unknown
        );
        assert_eq!(mergeability_from_body(&json!({ "number": 42 })), Mergeability::Unknown);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn mergeability_gets_the_single_pull_request_and_reads_404_as_unknown() {
        // The single-pull GET, not the list endpoint: `mergeable` is absent
        // from every listed pull request, so a client reading the list would
        // report `Unknown` forever. One request, and a pull request that is
        // gone is unknown rather than an error — nothing that does not exist
        // can conflict.
        let fake = FakeForge::start(|method, path| match (method, path) {
            ("GET", "/repos/forkuser/index/pulls/42") => (200, r#"{"number":42,"mergeable":false}"#.to_string()),
            ("GET", "/repos/forkuser/index/pulls/7") => (404, r#"{"message":"Not Found"}"#.to_string()),
            _ => (599, r#"{"message":"unexpected request"}"#.to_string()),
        })
        .await
        .expect("fake forge starts");

        let forge = fake.forge();
        assert_eq!(
            forge
                .pull_request_mergeability(&test_repo(), 42)
                .await
                .expect("a modelled body is not an error"),
            Mergeability::Conflicting
        );
        assert_eq!(
            forge
                .pull_request_mergeability(&test_repo(), 7)
                .await
                .expect("an absent pull request is not an error"),
            Mergeability::Unknown
        );

        assert_eq!(
            fake.routes(),
            [
                "GET /repos/forkuser/index/pulls/42".to_string(),
                "GET /repos/forkuser/index/pulls/7".to_string(),
            ],
            "one request per call, and no poll"
        );
    }

    #[test]
    fn client_builds_with_no_redirect_and_embedded_roots() {
        // Construction under the test seam must succeed (embedded roots seeded,
        // redirects disabled inside the builder).
        let forge = GitHubForge::with_base_url(
            ForgeToken::new("token".to_string()),
            "https://api.example.test/".to_string(),
        )
        .expect("client builds");
        // Trailing slash is trimmed so `url()` never doubles it.
        assert_eq!(forge.url("/user"), "https://api.example.test/user");
    }

    #[test]
    fn request_omits_authorization_header_when_token_is_empty() {
        let forge = GitHubForge::with_base_url(ForgeToken::new(String::new()), "https://api.example.test".to_string())
            .expect("client builds");
        let request = forge
            .request(Method::GET, "https://api.example.test/user")
            .build()
            .expect("request builds");
        assert!(
            request.headers().get(AUTHORIZATION).is_none(),
            "an empty token must not produce an Authorization header"
        );
    }

    #[test]
    fn request_includes_bearer_authorization_header_when_token_is_present() {
        let forge = GitHubForge::with_base_url(
            ForgeToken::new("secret-token".to_string()),
            "https://api.example.test".to_string(),
        )
        .expect("client builds");
        let request = forge
            .request(Method::GET, "https://api.example.test/user")
            .build()
            .expect("request builds");
        assert_eq!(
            request
                .headers()
                .get(AUTHORIZATION)
                .expect("authorization header present"),
            "Bearer secret-token"
        );
    }
}
