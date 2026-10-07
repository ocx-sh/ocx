// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! GitHub REST forge client.

use std::collections::BTreeMap;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use bytes::Bytes;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

use super::error::status_detail;
use super::http::build_forge_http_client;
use super::identity::{verify_fork_namespace, verify_github_fork};
use super::poll::{PollSchedule, backoff_delays};
use super::{
    BranchComparison, CapabilityName, CheckStatus, CommitBase, FileChange, Forge, ForgeCredentials, ForgeError,
    ForgeIdentity, ForkIdentity, Mergeability, PullRequest, PushAccess, RefUpdate, RepoCoordinate,
};

const DEFAULT_BASE_URL: &str = "https://api.github.com";
const API_VERSION: &str = "2022-11-28";
const ACCEPT_JSON: &str = "application/vnd.github+json";
/// Makes the contents API answer the raw file bytes rather than base64 JSON.
const ACCEPT_RAW: &str = "application/vnd.github.raw+json";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// ~39 s at worst: outlasts a fresh fork's object provisioning without stalling a push that
/// already published to the registry.
const GIT_DATA_RETRY_DELAYS: [Duration; 3] = [Duration::from_secs(3), Duration::from_secs(9), Duration::from_secs(27)];

fn force_flag(update: RefUpdate) -> bool {
    matches!(update, RefUpdate::Reset)
}

/// GitHub REST forge client.
pub struct GitHubForge {
    client: reqwest::Client,
    credentials: ForgeCredentials,
    base_url: String,
    /// Always [`GIT_DATA_RETRY_DELAYS`] outside tests; see [`Self::with_retry_delays`].
    retry_delays: &'static [Duration],
}

impl GitHubForge {
    /// Build a client for `host`: `None` for github.com, `Some` for a GitHub
    /// Enterprise Server instance.
    ///
    /// # Errors
    ///
    /// Returns [`ForgeError::ClientBuild`] when the HTTP client cannot be built.
    pub fn new(
        credentials: ForgeCredentials,
        host: Option<&str>,
        extra_roots: &ocx_util::tls::ExtraRoots,
    ) -> Result<Self, ForgeError> {
        let base_url = testing_base_url_override().unwrap_or_else(|| api_base_url(host));
        Self::build(credentials, base_url, extra_roots)
    }

    /// Build a client against an explicit base URL (acceptance fake-forge seam).
    ///
    /// # Errors
    ///
    /// Returns [`ForgeError::ClientBuild`] when the HTTP client cannot be built.
    #[cfg(any(test, feature = "__testing"))]
    pub fn with_base_url(credentials: ForgeCredentials, base_url: String) -> Result<Self, ForgeError> {
        Self::build(credentials, base_url, &ocx_util::tls::ExtraRoots::default())
    }

    /// Replace the shipped [`GIT_DATA_RETRY_DELAYS`] so a test can drive a replay without
    /// seconds of wall clock.
    #[cfg(any(test, feature = "__testing"))]
    #[must_use]
    pub fn with_retry_delays(mut self, delays: &'static [Duration]) -> Self {
        self.retry_delays = delays;
        self
    }

    fn build(
        credentials: ForgeCredentials,
        base_url: String,
        extra_roots: &ocx_util::tls::ExtraRoots,
    ) -> Result<Self, ForgeError> {
        Ok(Self {
            client: build_forge_http_client(REQUEST_TIMEOUT, extra_roots)?,
            credentials,
            base_url: base_url.trim_end_matches('/').to_string(),
            retry_delays: &GIT_DATA_RETRY_DELAYS,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }

    /// The token rides only the `Authorization` header, never the URL every [`ForgeError`] carries.
    fn request(&self, method: Method, url: &str) -> reqwest::RequestBuilder {
        let builder = self
            .client
            .request(method, url)
            .header(ACCEPT, ACCEPT_JSON)
            .header("X-GitHub-Api-Version", API_VERSION);
        // No header for an empty token (tokenless `--output`): GitHub rejects an empty bearer.
        if self.credentials.api().0.is_empty() {
            builder
        } else {
            builder.header(AUTHORIZATION, format!("Bearer {}", self.credentials.api().0))
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

    /// GitHub's reason lives only in the response body, so every non-success return routes
    /// through here to keep it.
    fn status_error(&self, url: &str, status: StatusCode, body: &[u8]) -> ForgeError {
        ForgeError::Status {
            url: url.to_string(),
            status: status.as_u16(),
            detail: status_detail(body, self.credentials.api().0.as_str()),
        }
    }

    fn parse_json(url: &str, body: &[u8]) -> Result<Value, ForgeError> {
        serde_json::from_slice(body).map_err(|source| ForgeError::Decode {
            url: url.to_string(),
            source,
        })
    }

    /// `None` on 404; any other non-success is an error.
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

    /// Polls a URL rebuilt from the verified identity, never one the API returned, which could
    /// aim the bearer at another host.
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

    /// Its own short timeout, so one black-holed GET cannot eat the poll deadline.
    async fn probe_ready(&self, url: &str, request_timeout: Duration) -> bool {
        matches!(
            self.request(Method::GET, url).timeout(request_timeout).send().await,
            Ok(response) if response.status().is_success()
        )
    }

    // ── git data API (multi-file atomic commit) ──

    async fn base_tree_sha(&self, repo: &RepoCoordinate, base_sha: &str) -> Result<String, ForgeError> {
        let url = self.url(&format!("/repos/{}/git/commits/{base_sha}", repo.full_path()));
        // A 404 stays a `Status`, never `MissingField`: a fresh fork's unprovisioned object store
        // answers it, and `commit_files` replays only on a status.
        let body = self.get_json_optional(&url).await?.ok_or_else(|| ForgeError::Status {
            url: url.clone(),
            status: StatusCode::NOT_FOUND.as_u16(),
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

    /// `POST /merge-upstream`; unlike [`Forge::sync_fork`] it returns the answer, which the
    /// commit retry reports.
    async fn merge_upstream(&self, fork: &RepoCoordinate, branch: &str) -> Result<(), ForgeError> {
        let url = self.url(&format!("/repos/{}/merge-upstream", fork.full_path()));
        self.post_json(&url, &json!({ "branch": branch })).await.map(|_| ())
    }

    /// Point `refs/heads/<branch>` at `commit_sha`, creating the ref when absent.
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
        // GitHub answers 422 both for a non-fast-forward and for a missing ref, so read the ref to
        // tell them apart; its English message is no stable contract.
        if status != StatusCode::UNPROCESSABLE_ENTITY && status != StatusCode::NOT_FOUND {
            return Err(self.status_error(&update_url, status, &update_response));
        }
        // 404 falls through too: a still-provisioning fork answers it, and the create's own 404
        // then drives the commit retry.
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
        // A concurrent first announce created the branch after the probe: a CAS conflict.
        if create_status == StatusCode::UNPROCESSABLE_ENTITY {
            return Err(ForgeError::NonFastForward {
                branch: branch.to_string(),
            });
        }
        Err(self.status_error(&create_url, create_status, &create_response))
    }

    /// One attempt at the [`Forge::commit_files`] sequence.
    async fn commit_files_once(
        &self,
        repo: &RepoCoordinate,
        branch: &str,
        base_sha: &str,
        message: &str,
        files: &BTreeMap<String, FileChange>,
        update: RefUpdate,
    ) -> Result<String, ForgeError> {
        let base_tree_sha = self.base_tree_sha(repo, base_sha).await?;
        let mut tree_entries = Vec::with_capacity(files.len());
        for (path, change) in files {
            match change {
                FileChange::Put(contents) => {
                    let blob_sha = self.create_blob(repo, contents).await?;
                    tree_entries.push(json!({ "path": path, "mode": "100644", "type": "blob", "sha": blob_sha }));
                }
                // ponytail: one read per deleted path; one `GET /git/trees/<base>?recursive=1` if
                // runs ever delete many, minding GitHub's truncation cap on that response.
                FileChange::Delete => {
                    // An absent path adds no null-`sha` entry: GitHub leaves that case undefined,
                    // and the trait owes a no-op.
                    if self.get_file_contents(repo, path, base_sha).await?.is_none() {
                        continue;
                    }
                    tree_entries.push(json!({ "path": path, "mode": "100644", "type": "blob", "sha": Value::Null }));
                }
            }
        }
        let tree_sha = self.create_tree(repo, &base_tree_sha, tree_entries).await?;
        let commit_sha = self.create_commit(repo, message, &tree_sha, base_sha).await?;
        self.upsert_branch(repo, branch, &commit_sha, update).await?;
        Ok(commit_sha)
    }
}

#[async_trait::async_trait]
impl Forge for GitHubForge {
    /// An App installation token has no user and answers 403 `NO_INTEGRATION_USER`: `Ok(None)`.
    async fn authenticated_identity(&self) -> Result<Option<ForgeIdentity>, ForgeError> {
        let url = self.url("/user");
        let body = match self.get_json_optional(&url).await {
            Ok(Some(body)) => body,
            Ok(None) => return Ok(None),
            Err(error) if no_user_behind_the_credential(&error) => return Ok(None),
            Err(error) => return Err(error),
        };
        Ok(Some(identity_from_body(&url, &body)?))
    }

    async fn resolve_user(&self, login: &str) -> Result<Option<ForgeIdentity>, ForgeError> {
        // Escaped `.`/`..` still collapse as dot segments and empty hits the `/users/` list, each
        // retargeting the request; no account bears these names.
        if matches!(login, "" | "." | "..") {
            return Ok(None);
        }
        let url = self.url(&format!("/users/{}", encode_segment(login)));
        let Some(body) = self.get_json_optional(&url).await? else {
            return Ok(None);
        };
        Ok(Some(identity_from_body(&url, &body)?))
    }

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

    /// A 404 is an error, never "not ahead", which would report success over a committed update
    /// that has no pull request.
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

    /// Open only: the per-package announce branch outlives every pull request opened from it.
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

    /// The single-pull GET, never the list: listed pull requests omit `mergeable`, and this GET
    /// is what starts GitHub computing it.
    async fn pull_request_mergeability(&self, index: &RepoCoordinate, number: u64) -> Result<Mergeability, ForgeError> {
        let url = self.url(&format!("/repos/{}/pulls/{number}", index.full_path()));
        let Some(body) = self.get_json_optional(&url).await? else {
            return Ok(Mergeability::Unknown);
        };
        Ok(mergeability_from_body(&body))
    }

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
        // A same-named stranger is "no fork here", not an error: the create path refuses it.
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
        let expected_namespace = match target_owner {
            Some(owner) => owner.to_string(),
            None => self.authenticated_login().await?,
        };
        // GitHub refuses a self-fork with an opaque 403, so name the refusal here.
        if expected_namespace.eq_ignore_ascii_case(&upstream.namespace) {
            return Err(ForgeError::SelfForkRefused {
                upstream: upstream.to_string(),
                namespace: expected_namespace,
            });
        }
        // A renamed fork misses here, and the idempotent create below adopts it.
        let conventional = upstream.with_namespace(expected_namespace.clone());
        if let Some(identity) = self.find_fork(upstream, &conventional).await? {
            return Ok(identity);
        }
        // The identity comes only from the response body: a composed `{namespace}/{project}`
        // misses a renamed fork.
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

    /// GitHub's "Sync fork": a fork far behind upstream answers writes parented on an upstream
    /// SHA with a 5xx or a reasonless 422.
    ///
    /// Best-effort: a diverged or up-to-date fork answers non-success, so a failure is only logged.
    async fn sync_fork(&self, fork: &RepoCoordinate, branch: &str) {
        if let Err(error) = self.merge_upstream(fork, branch).await {
            // WARN, not debug: CI logs at INFO, and without this line a later ref-write 404 reads
            // as a credential, permission or ruleset fault.
            tracing::warn!(
                %error,
                fork = %fork.full_path(),
                "fork sync failed; a fork behind upstream cannot reach the announce base commit"
            );
        }
    }

    /// GitHub answers an unauthorised write with 404 as often as 403, which
    /// [`Self::commit_files`] would replay as the fresh-fork race, so refuse before any write.
    ///
    /// # Errors
    ///
    /// Returns [`ForgeError::PushAccessDenied`] when `permissions.push` is not `true`.
    async fn ensure_push_access(&self, repo: &RepoCoordinate) -> Result<PushAccess, ForgeError> {
        let url = self.url(&format!("/repos/{}", repo.full_path()));
        let allowed = self
            .get_json_optional(&url)
            .await?
            .and_then(|body| body.get("permissions")?.get("push")?.as_bool())
            .unwrap_or(false);
        // An unreadable `permissions.push` (invisible repo, unauthenticated read) refuses too,
        // never `Unknown`: the point is to refuse before any write.
        if !allowed {
            return Err(ForgeError::PushAccessDenied { repo: repo.full_path() });
        }
        let mut access = PushAccess::skipped_all();
        access.record(CapabilityName::PushAccess, CheckStatus::Passed, None);
        Ok(access)
    }

    /// One git-data-API commit, never a loop over the single-file contents API, so the root and
    /// its CAS files land atomically.
    ///
    /// Replay cannot double-commit: objects are content-addressed and nothing publishes before
    /// the ref update.
    ///
    /// # Errors
    ///
    /// [`ForgeError::NonFastForward`] is never retried here: it needs the caller to regenerate.
    async fn commit_files(
        &self,
        repo: &RepoCoordinate,
        branch: &str,
        base: CommitBase<'_>,
        message: &str,
        files: &BTreeMap<String, FileChange>,
        update: RefUpdate,
    ) -> Result<String, ForgeError> {
        // Replay from the top: the opening `GET /git/commits` also 404s in a fresh fork's
        // provisioning window, which the metadata readiness poll does not cover.
        let mut outcome = self
            .commit_files_once(repo, branch, base.sha, message, files, update)
            .await;
        let cross_repo_base = base.repo != repo;
        let mut sync: Option<String> = None;
        for &delay in self.retry_delays {
            let Err(error) = &outcome else { break };
            if !is_retryable(error) {
                break;
            }
            tracing::debug!(%error, "replaying the git-data commit sequence");
            tokio::time::sleep(delay).await;
            // A fork behind a cross-repo base fails every blind replay identically, so re-sync
            // first and keep its answer for the error.
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
        // 409, or a 422 saying so, means one is already open, and the branch update refreshed it.
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

/// RFC 3986 unreserved minus `.`: a login is unvalidated operator input (`--owner`), and an
/// unescaped `?` or `/` would end or retarget the path.
const SEGMENT: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'_').remove(b'~');

fn encode_segment(value: &str) -> String {
    utf8_percent_encode(value, SEGMENT).to_string()
}

/// GitHub's 403 message for an App installation token, which has no user account.
const NO_INTEGRATION_USER: &str = "Resource not accessible by integration";

/// Matches the message, not the bare 403, or a SAML or missing-scope failure reads as an absent
/// account instead of an error.
///
/// `status_detail` caps `detail` at 300 characters; GitHub's `message` leads the body, so the
/// marker survives the cap.
fn no_user_behind_the_credential(error: &ForgeError) -> bool {
    matches!(error, ForgeError::Status { status: 403, detail, .. } if detail.contains(NO_INTEGRATION_USER))
}

/// `id` is required: a defaulted zero mismatches every `--owner LOGIN:ID` and refuses for the
/// wrong reason.
///
/// `type` is optional, since some shapes omit it and demanding it fails a legitimate account.
/// `bot` reads only `type == "Bot"`, never a `[bot]` login suffix, because it gates claim authorship.
fn identity_from_body(url: &str, body: &Value) -> Result<ForgeIdentity, ForgeError> {
    let missing = |field: &str| ForgeError::MissingField {
        url: url.to_string(),
        field: field.to_string(),
    };
    let login = body
        .get("login")
        .and_then(Value::as_str)
        .ok_or_else(|| missing("login"))?;
    let id = body.get("id").and_then(Value::as_u64).ok_or_else(|| missing("id"))?;
    Ok(ForgeIdentity {
        login: login.to_string(),
        id,
        bot: body.get("type").and_then(Value::as_str) == Some("Bot"),
    })
}

/// Always https: the bearer header would otherwise cross the wire in plaintext.
fn api_base_url(host: Option<&str>) -> String {
    match host {
        None => DEFAULT_BASE_URL.to_string(),
        Some(host) if host.eq_ignore_ascii_case("github.com") || host.eq_ignore_ascii_case("api.github.com") => {
            DEFAULT_BASE_URL.to_string()
        }
        Some(host) => format!("https://{host}/api/v3"),
    }
}

fn fork_create_body(target_owner: Option<&str>) -> Value {
    match target_owner {
        Some(owner) => json!({ "organization": owner }),
        None => json!({}),
    }
}

/// The GitHub owner of a coordinate, refusing a nested namespace before any request, which
/// GitHub would answer with a bare 404 that reads as a missing repository.
pub(super) fn require_flat_namespace(coordinate: &RepoCoordinate) -> Result<&str, ForgeError> {
    if coordinate.namespace.contains('/') {
        return Err(ForgeError::NestedNamespaceUnsupported {
            forge: "GitHub".to_string(),
            namespace: coordinate.namespace.clone(),
        });
    }
    Ok(&coordinate.namespace)
}

fn pr_head(head_owner: &str, branch: &str) -> String {
    format!("{head_owner}:{branch}")
}

/// An unmodelled `status` is an error, never a guess: a wrong "not ahead" strands a committed
/// announce with no pull request.
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

/// Name a spent-retry 404 on a cross-repo base as the fork lagging upstream, which a bare
/// status misreads as a credential, permission or ruleset fault.
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

/// 5xx is replayed because giving up after the registry push leaves the registry ahead of the
/// index.
///
/// 422 is never replayed: GitHub also spends it on real validation failures, and a rejected
/// fast-forward needs regeneration, not a blind replay.
fn is_retryable(error: &ForgeError) -> bool {
    matches!(
        error,
        ForgeError::Status { status, .. }
            if *status == StatusCode::NOT_FOUND.as_u16()
                || *status == StatusCode::TOO_MANY_REQUESTS.as_u16()
                || (500..600).contains(status)
    )
}

/// GitHub also answers 422 for "No commits between base and head", which list-and-reuse would
/// misreport as a missing `pull_request`.
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

/// A `null` or absent `mergeable` means GitHub is still computing: read as `false` it flags a
/// clean request, as `true` it clears a conflicting one.
fn mergeability_from_body(value: &Value) -> Mergeability {
    match value.get("mergeable").and_then(Value::as_bool) {
        Some(true) => Mergeability::Mergeable,
        Some(false) => Mergeability::Conflicting,
        None => Mergeability::Unknown,
    }
}

#[cfg(any(test, feature = "__testing"))]
fn testing_base_url_override() -> Option<String> {
    ocx_env::__OCX_TESTING_FORGE_BASE_URL
        .get_raw()
        .and_then(|url| url.into_string().ok())
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

    use super::super::{CapabilityCheck, ForgeToken};
    use super::*;

    /// C-007 / DX-5: the roots handed to `build` are what make the forge
    /// trust the operator's root — a client built with the default set does
    /// not. One in-process HTTPS server signed by a minted root: the forge
    /// built with no roots ends in `UnknownIssuer`, the one built with the
    /// root gets a 200. Both halves in one test so the seam cannot pass by
    /// never threading the roots.
    ///
    /// Mutation: have `build` pass `&ExtraRoots::default()` to
    /// `build_forge_http_client` — the second half reds.
    #[tokio::test(flavor = "multi_thread")]
    async fn extra_ca_build_threads_the_roots_into_the_private_client() {
        use ocx_test_support::pki::{TestPki, assert_untrusted_root, error_chain, serve_https};

        let pki = TestPki::mint();
        let addr = serve_https(&pki).await;
        let credentials = || ForgeCredentials::new(ForgeToken::new("token".to_string()));
        let forge = GitHubForge::with_base_url(credentials(), format!("https://{addr}")).expect("the forge builds");

        let error = forge
            .client
            .get(forge.url("/user"))
            .send()
            .await
            .expect_err("a client built with the default roots does not know the minted root");
        let chain = error_chain(&error);
        assert_untrusted_root(&chain);

        let forge = GitHubForge::build(
            credentials(),
            format!("https://{addr}"),
            &ocx_util::tls::ExtraRoots::from_pem(pki.root_pem().as_bytes()).expect("a minted root parses"),
        )
        .expect("the forge builds");
        let response = forge
            .client
            .get(forge.url("/user"))
            .send()
            .await
            .expect("a client built with the root trusts it");
        assert_eq!(response.status(), reqwest::StatusCode::OK);
    }

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
            GitHubForge::with_base_url(
                ForgeCredentials::new(ForgeToken::new("token".to_string())),
                self.base_url.clone(),
            )
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

    /// The shipped replay schedule, asserted without sleeping: the replay test
    /// below runs on [`FAST_RETRY_DELAYS`], so without this pin a change to
    /// the shipped delays would red nothing.
    #[test]
    fn the_shipped_git_data_retry_schedule_is_three_delays() {
        assert_eq!(
            GIT_DATA_RETRY_DELAYS,
            [Duration::from_secs(3), Duration::from_secs(9), Duration::from_secs(27)],
            "three replays, ~39 s at worst (design register X5)"
        );
        assert_eq!(
            GitHubForge::with_base_url(
                ForgeCredentials::new(ForgeToken::new("token".to_string())),
                "https://example.invalid".to_string(),
            )
            .expect("client builds")
            .retry_delays,
            GIT_DATA_RETRY_DELAYS,
            "an ordinary client replays on the shipped schedule; only `with_retry_delays` changes it"
        );
        assert_eq!(
            FAST_RETRY_DELAYS.len(),
            GIT_DATA_RETRY_DELAYS.len(),
            "same shape, shorter waits"
        );
    }

    /// The shipped [`GIT_DATA_RETRY_DELAYS`]' shape at a duration a unit test
    /// can afford. Same length, so a test replaying through it exhausts the
    /// same number of attempts.
    const FAST_RETRY_DELAYS: [Duration; 3] = [Duration::from_millis(1); 3];

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
        let files = BTreeMap::from([("packages/acme/widget.json".to_string(), FileChange::Put(b"{}".to_vec()))]);

        let commit = fake
            .forge()
            .with_retry_delays(&FAST_RETRY_DELAYS)
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

    /// C-001 on GitHub: a `Delete` is a tree entry whose `sha` is `null`, and a
    /// `Delete` of a path the base tree does not carry contributes **no entry**.
    ///
    /// Both halves in one commit on purpose. The null entry alone would pass for
    /// a build that emits one unconditionally — and that build fails a real
    /// announce the first time an orphan set names an object some earlier run
    /// never committed, which is the ordinary case once a root is rewritten more
    /// than once. The absent half alone would pass for a build that drops every
    /// removal on the floor.
    ///
    /// The probe count is asserted too, because it is the *reason* the absent
    /// half holds: a `Put` must not pay for a read, and a `Delete` must.
    ///
    /// Reds on: ignoring `FileChange::Delete` (no null entry, one tree entry);
    /// emitting the entry without the probe (the absent path joins the tree);
    /// probing on the `Put` arm (the contents-read count).
    #[tokio::test(flavor = "multi_thread")]
    async fn a_delete_is_a_null_tree_entry_and_an_absent_path_contributes_none() {
        const CONTENTS_PREFIX: &str = "/repos/forkuser/index/contents/";
        const PRESENT: &str = "p/acme/widget/o/sha256/present.json";
        const ABSENT: &str = "p/acme/widget/o/sha256/absent.json";

        let fake = FakeForge::start(|method, path| match (method, path) {
            ("GET", BASE_COMMIT_PATH) => (200, r#"{"tree":{"sha":"treesha"}}"#.to_string()),
            ("GET", target) if target.starts_with(CONTENTS_PREFIX) => {
                if target.contains("present.json") {
                    (200, "{}".to_string())
                } else {
                    (404, r#"{"message":"Not Found"}"#.to_string())
                }
            }
            ("POST", BLOBS_PATH | TREES_PATH | COMMITS_PATH) => (201, r#"{"sha":"newsha"}"#.to_string()),
            ("PATCH", UPDATE_PATH) => (200, r#"{"object":{"sha":"newsha"}}"#.to_string()),
            _ => (599, r#"{"message":"unexpected request"}"#.to_string()),
        })
        .await
        .expect("fake forge starts");

        let repo = test_repo();
        let files = BTreeMap::from([
            ("p/acme/widget.json".to_string(), FileChange::Put(b"{}".to_vec())),
            (PRESENT.to_string(), FileChange::Delete),
            (ABSENT.to_string(), FileChange::Delete),
        ]);

        fake.forge()
            .commit_files(
                &repo,
                BRANCH,
                CommitBase {
                    repo: &repo,
                    sha: "basesha",
                    branch: "main",
                },
                "announce acme/widget",
                &files,
                RefUpdate::FastForward,
            )
            .await
            .expect("a removal of an absent path must not fail the commit");

        let recorded = fake.recorded();
        let tree_body: Value = recorded
            .iter()
            .find(|call| call.route() == format!("POST {TREES_PATH}"))
            .map(|call| serde_json::from_str(&call.body).expect("the tree body is JSON"))
            .expect("a tree was posted");
        let entries = tree_body["tree"].as_array().expect("the tree is an array");

        assert_eq!(
            entries,
            &vec![
                json!({ "path": "p/acme/widget.json", "mode": "100644", "type": "blob", "sha": "newsha" }),
                json!({ "path": PRESENT, "mode": "100644", "type": "blob", "sha": Value::Null }),
            ],
            "the written path carries its blob, the committed orphan a null sha, and the absent one nothing"
        );

        let probes: Vec<&str> = recorded
            .iter()
            .filter(|call| call.method == "GET" && call.path.starts_with(CONTENTS_PREFIX))
            .map(|call| call.path.as_str())
            .collect();
        assert_eq!(
            probes.len(),
            2,
            "one read per removal and none for the write, got {probes:?}"
        );
        assert_eq!(
            recorded
                .iter()
                .filter(|call| call.route() == format!("POST {BLOBS_PATH}"))
                .count(),
            1,
            "a removal mints no blob"
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
            ForgeCredentials::new(ForgeToken::new("token".to_string())),
            "https://api.example.test/".to_string(),
        )
        .expect("client builds");
        // Trailing slash is trimmed so `url()` never doubles it.
        assert_eq!(forge.url("/user"), "https://api.example.test/user");
    }

    #[test]
    fn request_omits_authorization_header_when_token_is_empty() {
        let forge = GitHubForge::with_base_url(
            ForgeCredentials::new(ForgeToken::new(String::new())),
            "https://api.example.test".to_string(),
        )
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
            ForgeCredentials::new(ForgeToken::new("secret-token".to_string())),
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

    // ---- WP-7: GitHub REST identity and the write preflight ----

    /// The fake's answer to any route a test did not arm. A 500 rather than a
    /// 404, so an unexpected request is never mistaken for "absent".
    const UNEXPECTED: &str = r#"{"message":"unexpected request"}"#;

    #[tokio::test(flavor = "multi_thread")]
    async fn github_authenticated_identity_reads_user() {
        // C-023: the credential's own account comes from `GET /user` and from
        // nowhere else.
        let fake = FakeForge::start(|method, path| match (method, path) {
            ("GET", "/user") => (200, r#"{"login":"octocat","id":583231,"type":"User"}"#.to_string()),
            _ => (500, UNEXPECTED.to_string()),
        })
        .await
        .expect("fake forge starts");

        let identity = fake
            .forge()
            .authenticated_identity()
            .await
            .expect("the credential's account is readable")
            .expect("a user token has an account behind it");

        assert_eq!(
            identity,
            ForgeIdentity {
                login: "octocat".to_string(),
                id: 583_231,
                bot: false,
            }
        );
        assert_eq!(fake.routes(), ["GET /user".to_string()]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn github_bot_type_sets_bot_flag() {
        // C-008: `bot` carries GitHub's own `type` assertion and nothing else.
        //
        // The `[bot]`-suffixed login under `type: "User"` is the row that
        // refuses a login heuristic — an implementation reading
        // `login.ends_with("[bot]")` passes the `Bot` row and reds here.
        // C-049's login shapes are WP-9's, at the point a login enters the
        // claim body, and must not leak into this client.
        //
        // `Organization` and an absent `type` are legitimate identities rather
        // than decode failures: making `type` required would turn a real
        // account into an exit-1 decode error.
        for (case, expected_bot) in [
            (r#"{"login":"indexbot","id":1,"type":"Bot"}"#, true),
            (r#"{"login":"octocat","id":2,"type":"User"}"#, false),
            (r#"{"login":"dependabot[bot]","id":49699333,"type":"User"}"#, false),
            (r#"{"login":"acme-corp","id":3,"type":"Organization"}"#, false),
            (r#"{"login":"octocat","id":4}"#, false),
        ] {
            let body = case.to_string();
            let fake = FakeForge::start(move |method, path| match (method, path) {
                ("GET", "/user") => (200, body.clone()),
                _ => (500, UNEXPECTED.to_string()),
            })
            .await
            .expect("fake forge starts");

            let identity = fake
                .forge()
                .authenticated_identity()
                .await
                .expect("every shape here is a readable account")
                .expect("a user token has an account behind it");

            assert_eq!(identity.bot, expected_bot, "for the wire shape {case}");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn github_authenticated_identity_is_none_when_the_user_endpoint_is_absent() {
        // The second half of C-009's `Ok(None)`: an absent `/user` is a
        // credential with nothing behind it, which the owner ladder falls
        // through rather than failing on. The installation token — the
        // credential C-009 names — is the 403 arm below, not this one.
        //
        // Kept as its own arm because 404 cannot mean anything else here: this
        // endpoint takes no path parameter, so there is no "wrong login" a 404
        // could be reporting.
        let fake = FakeForge::start(|method, path| match (method, path) {
            ("GET", "/user") => (404, r#"{"message":"Not Found"}"#.to_string()),
            _ => (500, UNEXPECTED.to_string()),
        })
        .await
        .expect("fake forge starts");

        assert!(
            fake.forge()
                .authenticated_identity()
                .await
                .expect("a credential with no account is not a failure")
                .is_none(),
            "an absent account is Ok(None), never an error the ladder cannot fall through"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn github_authenticated_identity_is_none_for_an_app_installation_token() {
        // C-009's named `Ok(None)` case, on the wire shape it actually has.
        // `GET /user` under a GitHub App installation token answers **403**
        // with `Resource not accessible by integration` — the endpoint is
        // simply not part of an installation's surface. Read as an error, the
        // one credential the contract names by example exits 80 instead of
        // falling through the owner ladder.
        //
        // **The real-server shape is unverified until release gate 4**: the
        // status and message here are GitHub's documented text, not a recorded
        // response, and `test/tests/fake_forge.py` models the users API as one
        // `users_api_status` knob whose non-200 arms raise. If the live answer
        // differs, the fix is here, not in the ladder above.
        let fake = FakeForge::start(|method, path| match (method, path) {
            ("GET", "/user") => (
                403,
                r#"{"message":"Resource not accessible by integration","status":"403"}"#.to_string(),
            ),
            _ => (500, UNEXPECTED.to_string()),
        })
        .await
        .expect("fake forge starts");

        assert!(
            fake.forge()
                .authenticated_identity()
                .await
                .expect("an installation token has no user, which is not a failure")
                .is_none(),
            "the installation-token 403 is Ok(None), never an error the ladder cannot fall through"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn github_authenticated_identity_surfaces_an_ordinary_403_as_a_status() {
        // The falsifying half of the test above: only the installation-token
        // *message* is `Ok(None)`. Every other 403 on this endpoint is a real
        // permission failure — an org enforcing SAML, a token missing the
        // scope — and must reach the operator as exit 80, not vanish into an
        // absent account that sends the ladder looking for another owner.
        //
        // It also holds the `UsersApiUnavailable` boundary: that variant is
        // scoped to a GitLab job token, and a builder porting the job-token arm
        // across would turn exit 80 into exit 64, telling the operator to write
        // `LOGIN:ID` for what is really a permission failure.
        let fake = FakeForge::start(|method, path| match (method, path) {
            ("GET", "/user") => (
                403,
                r#"{"message":"Resource protected by organization SAML enforcement"}"#.to_string(),
            ),
            _ => (500, UNEXPECTED.to_string()),
        })
        .await
        .expect("fake forge starts");

        let error = fake
            .forge()
            .authenticated_identity()
            .await
            .expect_err("a forbidden identity read is an error");

        assert!(
            matches!(error, ForgeError::Status { status: 403, .. }),
            "an ordinary 403 stays a status, never Ok(None) and never UsersApiUnavailable: {error:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn github_identity_id_is_required_and_never_defaulted() {
        // `ForgeIdentity.id` is the value C-048 compares `--owner LOGIN:ID`
        // against, so a defaulted `0` would disagree with every real account and
        // exit 64 for the wrong reason. `Value::as_u64` answers `None` for a
        // JSON string and for a negative number, and both must be named
        // failures rather than silent zeroes.
        for (case, field) in [
            (r#"{"login":"octocat","type":"User"}"#, "id"),
            (r#"{"login":"octocat","id":"7","type":"User"}"#, "id"),
            (r#"{"login":"octocat","id":-1,"type":"User"}"#, "id"),
            (r#"{"id":7,"type":"User"}"#, "login"),
        ] {
            let body = case.to_string();
            let fake = FakeForge::start(move |method, path| match (method, path) {
                ("GET", "/user") => (200, body.clone()),
                _ => (500, UNEXPECTED.to_string()),
            })
            .await
            .expect("fake forge starts");

            let error = fake
                .forge()
                .authenticated_identity()
                .await
                .expect_err("an unreadable identity field is a named failure");

            assert!(
                matches!(&error, ForgeError::MissingField { field: got, .. } if got == field),
                "the wire shape {case} must name the missing {field}: {error:?}"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn github_resolve_user_404_is_none() {
        // C-024: the forge has no such account. `OwnerUnknown` (79) is decided
        // above this client, so a 404 must reach it as `Ok(None)`.
        let fake = FakeForge::start(|method, path| match (method, path) {
            ("GET", "/users/nobody") => (404, r#"{"message":"Not Found"}"#.to_string()),
            _ => (500, UNEXPECTED.to_string()),
        })
        .await
        .expect("fake forge starts");

        assert!(
            fake.forge()
                .resolve_user("nobody")
                .await
                .expect("an unknown login is not an error here")
                .is_none()
        );
        assert_eq!(fake.routes(), ["GET /users/nobody".to_string()]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn github_resolve_user_surfaces_non_404_statuses() {
        // Only 404 means "no such account". A `.ok()` or `unwrap_or(None)`
        // collapse would report `OwnerUnknown` (79) — "the forge has no such
        // account" — for a credential that may not look, or for an outage,
        // instead of 80 / 80 / 69.
        //
        // The last row is the installation-token 403 that
        // `authenticated_identity` reads as `Ok(None)`. It is an error *here*:
        // that arm answers "this credential has no user of its own", which says
        // nothing about whether `--owner LOGIN` names a real account. Hoisting
        // the check into a shared helper reds this row.
        for (status, body) in [
            (401_u16, r#"{"message":"nope"}"#),
            (403, r#"{"message":"nope"}"#),
            (500, r#"{"message":"nope"}"#),
            (403, r#"{"message":"Resource not accessible by integration"}"#),
        ] {
            let body = body.to_string();
            let fake = FakeForge::start(move |_, _| (status, body.clone()))
                .await
                .expect("fake forge starts");

            let error = fake
                .forge()
                .resolve_user("octocat")
                .await
                .expect_err("a non-404 failure is not an absent account");

            assert!(
                matches!(&error, ForgeError::Status { status: got, .. } if *got == status),
                "a {status} must surface as itself: {error:?}"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn github_resolve_user_percent_encodes_the_login_into_one_path_segment() {
        // `--owner` is operator input and reaches here unvalidated — a login is
        // not a `RepoCoordinate` and has no parse guard. Interpolated raw, `?`
        // would end the path and turn the rest into a query, and `/` would
        // retarget the request at a different endpoint entirely.
        //
        // The assertion is on the **recorded path**, never on the response: a
        // fake that 404s whatever it is asked answers the same in both states,
        // so a response assertion here would be green with the encoding gone.
        let fake = FakeForge::start(|_, _| (404, r#"{"message":"Not Found"}"#.to_string()))
            .await
            .expect("fake forge starts");

        let absent = fake
            .forge()
            .resolve_user("x?a=1")
            .await
            .expect("an absent account is not an error");

        assert!(absent.is_none());
        assert_eq!(
            fake.routes(),
            ["GET /users/x%3Fa%3D1".to_string()],
            "the login travels as one encoded path segment, never as a query or a second segment"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn github_resolve_user_refuses_a_login_that_cannot_be_a_path_segment() {
        // Percent-encoding alone does not stop traversal: the URL parser pops
        // `%2E%2E` exactly as it pops `..` (url-2.5.8 `parser.rs`), so
        // `--owner ..` would request the API root, which answers 200 with no
        // `login` field — a decode error where C-048 is owed "no such account".
        // The empty login is the same failure by a different route: `/users/`
        // is GitHub's list-users endpoint, whose 200 array has no `login`
        // either. No account is named ``, `.` or `..`, so `Ok(None)` is the
        // honest answer and nothing reaches the wire.
        for login in ["", ".", ".."] {
            let fake = FakeForge::start(|_, _| (200, r#"{"login":"root","id":1}"#.to_string()))
                .await
                .expect("fake forge starts");

            assert!(
                fake.forge()
                    .resolve_user(login)
                    .await
                    .expect("a dot segment is an absent account, not an error")
                    .is_none(),
                "{login:?} must not resolve to an identity"
            );
            assert!(
                fake.routes().is_empty(),
                "{login:?} must never reach the wire: encoded it is still collapsed by the URL parser"
            );
        }

        // The literal spelling is inert and must still travel — it is escaped
        // to `%252E%252E`, which no parser reads as a dot segment, so refusing
        // it would refuse a legal (if absent) login for no reason.
        let fake = FakeForge::start(|_, _| (404, r#"{"message":"Not Found"}"#.to_string()))
            .await
            .expect("fake forge starts");

        assert!(
            fake.forge()
                .resolve_user("%2E%2E")
                .await
                .expect("an absent account is not an error")
                .is_none()
        );
        assert_eq!(fake.routes(), ["GET /users/%252E%252E".to_string()]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn github_resolve_user_returns_the_canonical_login_spelling() {
        // C-048's confirm step: the lookup is case-insensitive and the account's
        // own spelling is what comes back. A client echoing its `login`
        // argument would make WP-9's canonical-spelling test green against a
        // lie, since that test can only see what this returns.
        let fake = FakeForge::start(|method, path| match (method, path) {
            ("GET", "/users/AliCe") => (200, r#"{"login":"alice","id":7,"type":"User"}"#.to_string()),
            _ => (500, UNEXPECTED.to_string()),
        })
        .await
        .expect("fake forge starts");

        let identity = fake
            .forge()
            .resolve_user("AliCe")
            .await
            .expect("the account resolves")
            .expect("the forge knows this account");

        assert_eq!(
            identity,
            ForgeIdentity {
                login: "alice".to_string(),
                id: 7,
                bot: false,
            },
            "the body's spelling wins over the caller's"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn github_ensure_push_access_emits_rows() {
        // **Characterization, not contract-first.** `ensure_push_access` was
        // shipped implemented in WP-5 (DX-22), so this test was green on
        // arrival and never had a red of its own. Its red was produced by
        // mutation instead, in review round 1: recording the upgrade against
        // `CapabilityName::JobTokenPush` reds the slice assertion below with
        // `assertion `left == right` failed` on the `push-access` and
        // `job-token-push` rows. The mutation was reverted, not committed.
        //
        // It locks the row set C-025 promises, with one clause read as
        // unreachable rather than implemented: C-025 reads `git-version` from
        // "the held `GitBinary`", and this client holds none — GitHub plus the
        // git write transport is refused before a client is built — so that row
        // is `skipped` here, and GitHub has no job-token capability to report.
        //
        // The whole slice is asserted, order included: a single-row assertion
        // on `push-access` stays green when the upgrade is recorded against the
        // wrong capability.
        let fake = FakeForge::start(|method, path| match (method, path) {
            ("GET", "/repos/forkuser/index") => (200, r#"{"permissions":{"push":true}}"#.to_string()),
            _ => (500, UNEXPECTED.to_string()),
        })
        .await
        .expect("fake forge starts");

        let access = fake
            .forge()
            .ensure_push_access(&test_repo())
            .await
            .expect("a pushable repository passes the preflight");

        let expected: &[CapabilityCheck] = &[
            CapabilityCheck {
                name: CapabilityName::GitVersion,
                status: CheckStatus::Skipped,
                detail: None,
            },
            CapabilityCheck {
                name: CapabilityName::PushAccess,
                status: CheckStatus::Passed,
                detail: None,
            },
            CapabilityCheck {
                name: CapabilityName::JobTokenPush,
                status: CheckStatus::Skipped,
                detail: None,
            },
            CapabilityCheck {
                name: CapabilityName::JobTokenAllowlist,
                status: CheckStatus::Skipped,
                detail: None,
            },
        ];
        assert_eq!(access.checks(), expected);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn github_capability_detail_is_never_response_derived() {
        // C-011: `detail` is drawn only from a closed set of values ocx already
        // holds, never from a forge response body — widening that closure means
        // routing the new source through the redactor first. GitHub's
        // `push-access` row carries `None` because its permissions payload is a
        // bare boolean with nothing further to report, and DX-22 rules that
        // asymmetry with GitLab's `Some("access level {n}")` deliberate signal
        // a later package must not reconcile.
        const MARKER: &str = "detail-must-never-carry-this-9f3c";
        let body = format!(r#"{{"description":"{MARKER}","permissions":{{"push":true}}}}"#);
        let fake = FakeForge::start(move |method, path| match (method, path) {
            ("GET", "/repos/forkuser/index") => (200, body.clone()),
            _ => (500, UNEXPECTED.to_string()),
        })
        .await
        .expect("fake forge starts");

        let access = fake
            .forge()
            .ensure_push_access(&test_repo())
            .await
            .expect("a pushable repository passes the preflight");

        for check in access.checks() {
            assert_eq!(check.detail, None, "GitHub reports no qualifier on {}", check.name);
        }
        assert!(
            !format!("{access:?}").contains(MARKER),
            "no part of the preflight may carry a response body: {access:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn github_unreadable_push_permission_is_denied_never_unknown() {
        // **A stated exception to C-011 on this forge, not a defect** — and it
        // covers only the rows where the field cannot be read. C-011 says a
        // check whose field is unreadable reports `Unknown` and does not fail
        // the call; GitHub's probe predates the capability vocabulary and
        // refuses instead, for the three shapes below that carry no
        // `permissions.push`: a repository invisible to the credential answers
        // 404 rather than 403, an unauthenticated read omits the object, and a
        // present object may still not carry the key. The last row needs no
        // exception — `permissions.push == false` is a readable field saying
        // no. All four land on `PushAccessDenied` (80) before any write.
        //
        // The consequence is a cross-package constraint: GitHub's `push-access`
        // row has two outcomes only, `passed` or a raised error, so S-011's
        // `push-access: skipped` under `--output` is satisfied by **not calling
        // this at all** on that path and seeding `PushAccess::skipped_all()`
        // above it, never by this returning `skipped`.
        for (status, case) in [
            (404_u16, r#"{"message":"Not Found"}"#),
            (200, r#"{}"#),
            (200, r#"{"permissions":{}}"#),
            (200, r#"{"permissions":{"push":false}}"#),
        ] {
            let body = case.to_string();
            let fake = FakeForge::start(move |method, path| match (method, path) {
                ("GET", "/repos/forkuser/index") => (status, body.clone()),
                _ => (500, UNEXPECTED.to_string()),
            })
            .await
            .expect("fake forge starts");

            let error = fake
                .forge()
                .ensure_push_access(&test_repo())
                .await
                .expect_err("an unreadable push permission refuses the write");

            assert!(
                matches!(&error, ForgeError::PushAccessDenied { repo } if repo == "forkuser/index"),
                "{status} {case} must be a named refusal, never an Unknown row: {error:?}"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn github_ensure_push_access_without_a_credential_is_denied() {
        // S-011's `--output` run: an empty token sends no `Authorization` header,
        // and GitHub answers an unauthenticated repository read 200 **without**
        // `permissions`. Pinned so that a later package which starts routing
        // `--output` through the preflight reds here rather than shipping exit 80
        // on a run that never intended to write.
        let fake = FakeForge::start(|method, path| match (method, path) {
            ("GET", "/repos/forkuser/index") => (200, r#"{"full_name":"forkuser/index"}"#.to_string()),
            _ => (500, UNEXPECTED.to_string()),
        })
        .await
        .expect("fake forge starts");

        let forge = GitHubForge::with_base_url(
            ForgeCredentials::new(ForgeToken::new(String::new())),
            fake.base_url.clone(),
        )
        .expect("client builds");

        let error = forge
            .ensure_push_access(&test_repo())
            .await
            .expect_err("an unauthenticated read carries no push permission");

        assert!(
            matches!(&error, ForgeError::PushAccessDenied { repo } if repo == "forkuser/index"),
            "{error:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn github_ensure_fork_names_the_missing_login_when_the_user_endpoint_is_absent() {
        // `authenticated_login` and `authenticated_identity` read the same
        // endpoint and answer an absent `/user` **differently on purpose**:
        // C-009 owes `Ok(None)` so the owner ladder can fall through, while
        // `ensure_fork` has no fallback — without a login there is no namespace
        // to fork into, so it must stop with a named field. Folding the two
        // would turn this into a fork attempt against an empty namespace.
        let fake = FakeForge::start(|method, path| match (method, path) {
            ("GET", "/user") => (404, r#"{"message":"Not Found"}"#.to_string()),
            _ => (500, UNEXPECTED.to_string()),
        })
        .await
        .expect("fake forge starts");

        let upstream = RepoCoordinate {
            host: None,
            namespace: "ocx-sh".to_string(),
            project: "index".to_string(),
        };
        let error = fake
            .forge()
            .ensure_fork(&upstream, None)
            .await
            .expect_err("a fork needs a namespace, and there is none");

        assert!(
            matches!(&error, ForgeError::MissingField { field, .. } if field == "login"),
            "{error:?}"
        );
    }

    #[test]
    fn api_base_url_is_the_dedicated_origin_for_github_com_and_api_v3_elsewhere() {
        // The identity endpoints are the first ones a GitHub Enterprise Server
        // publisher hits, and a wrong base reads as "user not found" rather than
        // "wrong host". `api.github.com` is accepted as a spelling of github.com
        // so a configured API host is not composed into
        // `https://api.github.com/api/v3`.
        assert_eq!(api_base_url(None), "https://api.github.com");
        assert_eq!(api_base_url(Some("GitHub.com")), "https://api.github.com");
        assert_eq!(api_base_url(Some("api.github.com")), "https://api.github.com");
        assert_eq!(api_base_url(Some("ghe.acme.test")), "https://ghe.acme.test/api/v3");
    }
}
