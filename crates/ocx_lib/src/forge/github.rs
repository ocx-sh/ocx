// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! GitHub REST forge client.
//!
//! Copy-and-own port of grimoire's GitHub forge flow (`src/catalog/forge.rs`,
//! same owner), transport-adjusted to REST-only and owned by OCX (design
//! register S5). Enforces the X5 invariants: a single no-redirect client per
//! run, bearer via header only, fork parent verified against the upstream,
//! endpoints rebuilt only from a response-body identity, a bounded readiness
//! poll, and one 3s retry of the whole commit sequence for GitHub's "fork
//! metadata ready before git objects" write race. Commits are multi-file atomic
//! via the git data API (design register C15).

use std::collections::BTreeMap;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use bytes::Bytes;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

use super::identity::{verify_fork_identity, verify_fork_owner};
use super::poll::{PollSchedule, backoff_delays};
use super::{ForgeError, ForgeToken, ForkIdentity, PullRequest, RepoCoordinate};

/// Canonical GitHub REST base URL. Overridable only under the test seam.
const DEFAULT_BASE_URL: &str = "https://api.github.com";
/// GitHub REST API version header value.
const API_VERSION: &str = "2022-11-28";
/// JSON media type for GitHub REST responses.
const ACCEPT_JSON: &str = "application/vnd.github+json";
/// Raw media type — returns file bytes directly from the contents API.
const ACCEPT_RAW: &str = "application/vnd.github.raw+json";
/// Client user-agent (GitHub requires one).
const USER_AGENT_VALUE: &str = concat!("ocx/", env!("CARGO_PKG_VERSION"));
/// Total per-request timeout for ordinary forge calls.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Fixed delay before the single fresh-fork write retry (design register X5).
const FRESH_FORK_RETRY_DELAY: Duration = Duration::from_secs(3);

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
    /// Build a client for `api.github.com` (or the test-seam base URL override).
    ///
    /// Under `cfg(any(test, feature = "__testing"))` the
    /// `__OCX_TESTING_FORGE_BASE_URL` env var redirects the client so the
    /// acceptance fake forge can intercept it; production ignores it.
    ///
    /// # Errors
    ///
    /// Returns [`ForgeError::ClientBuild`] when the hardened HTTP client cannot
    /// be constructed.
    pub fn new(token: ForgeToken) -> Result<Self, ForgeError> {
        let base_url = testing_base_url_override().unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
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
            return Err(ForgeError::Status {
                url: url.to_string(),
                status: status.as_u16(),
            });
        }
        Ok(Some(Self::parse_json(url, &body)?))
    }

    /// POST a JSON body and parse the success response.
    async fn post_json(&self, url: &str, body: &Value) -> Result<Value, ForgeError> {
        let (status, response) = self.send(self.json_request(Method::POST, url, body)?, url).await?;
        if !status.is_success() {
            return Err(ForgeError::Status {
                url: url.to_string(),
                status: status.as_u16(),
            });
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
        let url = self.url(&format!("/repos/{}", identity.full_name));
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
        let url = self.url(&format!("/repos/{}/{}/git/commits/{base_sha}", repo.owner, repo.repo));
        // A 404 here is NOT a malformed response: this is the first request of
        // the commit sequence, and a brand-new fork's git object store may not
        // be provisioned yet (design register X5). Surface it as the status it
        // was so [`Self::commit_files`] can retry the whole sequence;
        // `MissingField` stays reserved for a 200 whose body lacks `tree.sha`.
        let body = self.get_json_optional(&url).await?.ok_or_else(|| ForgeError::Status {
            url: url.clone(),
            status: StatusCode::NOT_FOUND.as_u16(),
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
        let url = self.url(&format!("/repos/{}/{}/git/blobs", repo.owner, repo.repo));
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
        let url = self.url(&format!("/repos/{}/{}/git/trees", repo.owner, repo.repo));
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
        let url = self.url(&format!("/repos/{}/{}/git/commits", repo.owner, repo.repo));
        let body = json!({ "message": message, "tree": tree_sha, "parents": [parent_sha] });
        let value = self.post_json(&url, &body).await?;
        object_sha(&url, &value)
    }

    /// Point `refs/heads/<branch>` at `commit_sha`, creating the ref when it
    /// does not yet exist. The update is **fast-forward-only** (compare-and-swap,
    /// design register C4): a non-fast-forward — a concurrent announce advanced
    /// the branch — surfaces as [`ForgeError::NonFastForward`] for the caller to
    /// re-read and retry, never a silent force-overwrite.
    async fn upsert_branch(&self, repo: &RepoCoordinate, branch: &str, commit_sha: &str) -> Result<(), ForgeError> {
        let update_url = self.url(&format!("/repos/{}/{}/git/refs/heads/{branch}", repo.owner, repo.repo));
        let update_body = json!({ "sha": commit_sha, "force": false });
        let (status, _) = self
            .send(
                self.json_request(Method::PATCH, &update_url, &update_body)?,
                &update_url,
            )
            .await?;
        if status.is_success() {
            return Ok(());
        }
        // A fast-forward-only update of an existing ref that is not an ancestor
        // returns 422 — the concurrent-advance (CAS) case.
        if status == StatusCode::UNPROCESSABLE_ENTITY {
            return Err(ForgeError::NonFastForward {
                branch: branch.to_string(),
            });
        }
        // Only a genuinely absent ref (404) falls through to creation.
        if status != StatusCode::NOT_FOUND {
            return Err(ForgeError::Status {
                url: update_url,
                status: status.as_u16(),
            });
        }
        let create_url = self.url(&format!("/repos/{}/{}/git/refs", repo.owner, repo.repo));
        let create_body = json!({ "ref": format!("refs/heads/{branch}"), "sha": commit_sha });
        let (create_status, _) = self
            .send(self.json_request(Method::POST, &create_url, &create_body)?, &create_url)
            .await?;
        if create_status.is_success() {
            return Ok(());
        }
        // A concurrent first announce created the branch between our 404 and
        // this create — treat it as a CAS conflict and retry as an update.
        if create_status == StatusCode::UNPROCESSABLE_ENTITY {
            return Err(ForgeError::NonFastForward {
                branch: branch.to_string(),
            });
        }
        Err(ForgeError::Status {
            url: create_url,
            status: create_status.as_u16(),
        })
    }

    /// Read a file's bytes at `r#ref`, or `None` when the path does not exist.
    ///
    /// # Errors
    ///
    /// Returns a [`ForgeError`] on transport failure, a non-success status
    /// other than 404, or a response-decode failure.
    pub async fn get_file_contents(
        &self,
        repo: &RepoCoordinate,
        path: &str,
        r#ref: &str,
    ) -> Result<Option<Vec<u8>>, ForgeError> {
        let url = self.url(&format!("/repos/{}/{}/contents/{path}", repo.owner, repo.repo));
        let request = self
            .request(Method::GET, &url)
            .header(ACCEPT, ACCEPT_RAW)
            .query(&[("ref", r#ref)]);
        let (status, body) = self.send(request, &url).await?;
        if status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(ForgeError::Status {
                url,
                status: status.as_u16(),
            });
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
    pub async fn get_ref_sha(&self, repo: &RepoCoordinate, r#ref: &str) -> Result<Option<String>, ForgeError> {
        let url = self.url(&format!("/repos/{}/{}/git/ref/{}", repo.owner, repo.repo, r#ref));
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

    /// Whether `head_owner:head_branch` carries commits `base` does not.
    ///
    /// One compare call answers ancestry exactly, which a bare ref-SHA equality
    /// check cannot: GitHub reports `identical` when the branch *is* the base
    /// and `behind` when it is an ancestor of the base (both "not ahead" —
    /// nothing unmerged to recover), against `ahead` / `diverged` when the
    /// branch holds commits of its own.
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
    pub async fn branch_is_ahead(
        &self,
        repo: &RepoCoordinate,
        base: &str,
        head_owner: &str,
        head_branch: &str,
    ) -> Result<bool, ForgeError> {
        let url = self.url(&format!(
            "/repos/{}/{}/compare/{base}...{head_owner}:{head_branch}",
            repo.owner, repo.repo
        ));
        let Some(body) = self.get_json_optional(&url).await? else {
            return Err(ForgeError::Status {
                url,
                status: StatusCode::NOT_FOUND.as_u16(),
            });
        };
        let status = body
            .get("status")
            .and_then(Value::as_str)
            .ok_or_else(|| ForgeError::MissingField {
                url: url.clone(),
                field: "status".to_string(),
            })?;
        compare_status_is_ahead(&url, status)
    }

    /// Look up an existing fork of `upstream` at `fork`, **without creating
    /// one**. `None` when nothing is there, or when what is there is not a
    /// verified fork of `upstream` (a same-named stranger repository).
    ///
    /// Read-only by contract: the caller resolves the fork's real identity
    /// before deciding whether any write is needed at all, so a pure no-op run
    /// never provokes a fork create. It honours the full requested coordinate —
    /// a fork renamed away from the upstream's repository name resolves here,
    /// where deriving the path from `upstream.repo` would miss it.
    ///
    /// # Errors
    ///
    /// Returns a [`ForgeError`] on transport failure, a non-success status
    /// other than 404, or a verified fork living under an unexpected owner.
    pub async fn find_fork(
        &self,
        upstream: &RepoCoordinate,
        fork: &RepoCoordinate,
    ) -> Result<Option<ForkIdentity>, ForgeError> {
        let url = self.url(&format!("/repos/{}/{}", fork.owner, fork.repo));
        let Some(body) = self.get_json_optional(&url).await? else {
            return Ok(None);
        };
        // A same-named stranger repository is "no fork here", not a hard error:
        // the caller's create path is what refuses it (X5).
        let Ok(identity) = verify_fork_identity(&body, upstream) else {
            return Ok(None);
        };
        verify_fork_owner(&identity, &fork.owner)?;
        Ok(Some(identity))
    }

    /// Ensure a fork of `upstream` exists and is ready, returning its verified
    /// identity. `target_owner` = `None` forks under the token identity; `Some`
    /// forks into that owner (organization) and verifies the returned identity
    /// against it (design register S12).
    ///
    /// # Errors
    ///
    /// Returns a [`ForgeError`] on transport failure, a non-success status, a
    /// fork identity that fails verification against `upstream` or the
    /// expected owner, or a readiness poll that exceeds its deadline.
    pub async fn ensure_fork(
        &self,
        upstream: &RepoCoordinate,
        target_owner: Option<&str>,
    ) -> Result<ForkIdentity, ForgeError> {
        // The fork must live under an explicit target owner, else the token
        // identity. This anchor is verified against the fork's own identity.
        let expected_owner = match target_owner {
            Some(owner) => owner.to_string(),
            None => self.authenticated_login().await?,
        };
        // Reuse a verified existing fork at the conventional path. A renamed
        // fork (or a same-named stranger) fails verification and falls through
        // to the idempotent create below.
        let conventional = RepoCoordinate {
            owner: expected_owner.clone(),
            repo: upstream.repo.clone(),
        };
        if let Some(identity) = self.find_fork(upstream, &conventional).await? {
            return Ok(identity);
        }
        // Create (or adopt a renamed) fork; the identity is built ONLY from the
        // response body, never `{expected_owner}/{upstream.repo}`.
        let create_url = self.url(&format!("/repos/{}/{}/forks", upstream.owner, upstream.repo));
        let (status, body) = self
            .send(
                self.json_request(Method::POST, &create_url, &fork_create_body(target_owner))?,
                &create_url,
            )
            .await?;
        if !status.is_success() {
            return Err(ForgeError::Status {
                url: create_url,
                status: status.as_u16(),
            });
        }
        let value = Self::parse_json(&create_url, &body)?;
        let identity = verify_fork_identity(&value, upstream)?;
        verify_fork_owner(&identity, &expected_owner)?;
        self.wait_fork_ready(&identity).await?;
        Ok(identity)
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
    pub async fn commit_files(
        &self,
        repo: &RepoCoordinate,
        branch: &str,
        base_sha: &str,
        message: &str,
        files: &BTreeMap<String, Vec<u8>>,
    ) -> Result<String, ForgeError> {
        match self.commit_files_once(repo, branch, base_sha, message, files).await {
            Err(error) if is_fresh_fork_race(&error) => {
                tokio::time::sleep(FRESH_FORK_RETRY_DELAY).await;
                self.commit_files_once(repo, branch, base_sha, message, files).await
            }
            result => result,
        }
    }

    /// One attempt at the [`Self::commit_files`] sequence: base tree -> blobs ->
    /// tree -> commit -> fast-forward-only ref update.
    async fn commit_files_once(
        &self,
        repo: &RepoCoordinate,
        branch: &str,
        base_sha: &str,
        message: &str,
        files: &BTreeMap<String, Vec<u8>>,
    ) -> Result<String, ForgeError> {
        let base_tree_sha = self.base_tree_sha(repo, base_sha).await?;
        let mut tree_entries = Vec::with_capacity(files.len());
        for (path, contents) in files {
            let blob_sha = self.create_blob(repo, contents).await?;
            tree_entries.push(json!({ "path": path, "mode": "100644", "type": "blob", "sha": blob_sha }));
        }
        let tree_sha = self.create_tree(repo, &base_tree_sha, tree_entries).await?;
        let commit_sha = self.create_commit(repo, message, &tree_sha, base_sha).await?;
        self.upsert_branch(repo, branch, &commit_sha).await?;
        Ok(commit_sha)
    }

    /// Open a pull request from `head_owner:branch` into `index`'s `base`, or
    /// reuse the existing open one (never duplicate — design register C9).
    ///
    /// # Errors
    ///
    /// Returns a [`ForgeError`] on transport failure, a non-success status
    /// other than the "pull request already exists" 422/409, or a malformed
    /// pull-request response body.
    pub async fn open_or_update_pull_request(
        &self,
        index: &RepoCoordinate,
        head_owner: &str,
        branch: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<PullRequest, ForgeError> {
        let pulls_url = self.url(&format!("/repos/{}/{}/pulls", index.owner, index.repo));
        let head = pr_head(head_owner, branch);
        let create_body = json!({ "title": title, "head": head, "base": base, "body": body });
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
            return Err(ForgeError::Status {
                url: pulls_url,
                status: status.as_u16(),
            });
        }
        let request = self
            .request(Method::GET, &pulls_url)
            .query(&[("head", head.as_str()), ("state", "open")]);
        let (list_status, list_body) = self.send(request, &pulls_url).await?;
        if !list_status.is_success() {
            return Err(ForgeError::Status {
                url: pulls_url,
                status: list_status.as_u16(),
            });
        }
        let list = Self::parse_json(&pulls_url, &list_body)?;
        let existing = list
            .as_array()
            .and_then(|pulls| pulls.first())
            .ok_or_else(|| ForgeError::MissingField {
                url: pulls_url.clone(),
                field: "pull_request".to_string(),
            })?;
        pull_request_from_body(&pulls_url, existing, true)
    }
}

/// Build the no-redirect forge HTTP client (design register X5).
///
/// Redirects are disabled because reqwest otherwise replays the bearer
/// `Authorization` header on a cross-host 3xx `Location`, exfiltrating the
/// token. These REST endpoints never legitimately redirect; a non-2xx surfaces
/// as an error, never chased. Embedded Mozilla roots are seeded so TLS works
/// with no system trust store (minimal CI runner), mirroring the index HTTP
/// client's hardening (`oci/index/ocx_index.rs`).
fn build_forge_http_client(timeout: Duration) -> Result<reqwest::Client, ForgeError> {
    let builder = reqwest::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(USER_AGENT_VALUE);
    crate::utility::tls::seed_embedded_roots(builder)
        .build()
        .map_err(|source| ForgeError::ClientBuild { source })
}

/// The fork-create request body: `organization` when a target owner is named
/// (design register S12), an empty object for the token-identity default.
fn fork_create_body(target_owner: Option<&str>) -> Value {
    match target_owner {
        Some(owner) => json!({ "organization": owner }),
        None => json!({}),
    }
}

/// The cross-repo pull-request head, `head_owner:branch`.
fn pr_head(head_owner: &str, branch: &str) -> String {
    format!("{head_owner}:{branch}")
}

/// Classify a compare response's `status` into "carries unmerged commits".
///
/// Exhaustive over GitHub's documented four values; an unmodelled value is an
/// error, never a guess — a wrong "not ahead" strands a committed announce with
/// no pull request (design register C6 amendment).
fn compare_status_is_ahead(url: &str, status: &str) -> Result<bool, ForgeError> {
    match status {
        "ahead" | "diverged" => Ok(true),
        "identical" | "behind" => Ok(false),
        unknown => Err(ForgeError::UnknownCompareStatus {
            url: url.to_string(),
            status: unknown.to_string(),
        }),
    }
}

/// Whether an error is a 404 from the git data API — GitHub's "fork metadata
/// ready before git objects" provisioning window (design register X5), the one
/// failure [`GitHubForge::commit_files`] retries.
fn is_fresh_fork_race(error: &ForgeError) -> bool {
    matches!(error, ForgeError::Status { status, .. } if *status == StatusCode::NOT_FOUND.as_u16())
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
    use super::*;

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
    fn compare_status_ahead_and_diverged_carry_unmerged_commits() {
        assert!(compare_status_is_ahead("https://api", "ahead").expect("modelled"));
        assert!(compare_status_is_ahead("https://api", "diverged").expect("modelled"));
    }

    #[test]
    fn compare_status_identical_and_behind_carry_nothing_unmerged() {
        assert!(!compare_status_is_ahead("https://api", "identical").expect("modelled"));
        assert!(!compare_status_is_ahead("https://api", "behind").expect("modelled"));
    }

    #[test]
    fn compare_status_unmodelled_value_errors_instead_of_guessing() {
        // A wrong "not ahead" strands a committed announce with no pull
        // request, so an unrecognized value must surface, never default.
        let error = compare_status_is_ahead("https://api", "sideways").expect_err("unmodelled status must error");
        assert!(matches!(
            error,
            ForgeError::UnknownCompareStatus { ref status, .. } if status == "sideways"
        ));
    }

    #[test]
    fn only_a_404_counts_as_the_fresh_fork_race() {
        let not_found = ForgeError::Status {
            url: "https://api/git/commits/abc".to_string(),
            status: 404,
        };
        assert!(is_fresh_fork_race(&not_found));
        for other in [401, 422, 500] {
            let error = ForgeError::Status {
                url: "https://api/git/trees".to_string(),
                status: other,
            };
            assert!(!is_fresh_fork_race(&error), "{other} must not trigger the retry");
        }
        // A rejected fast-forward-only update needs the caller's regeneration,
        // never a blind replay of the same commit.
        assert!(!is_fresh_fork_race(&ForgeError::NonFastForward {
            branch: "indexbot-announce-acme-widget".to_string(),
        }));
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
