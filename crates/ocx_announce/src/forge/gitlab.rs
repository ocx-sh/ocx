// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! GitLab REST v4 [`Forge`] client: a commit is one atomic POST of file actions, concurrency is
//! per file (`last_commit_id`), and a project path travels as one percent-encoded segment.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use bytes::Bytes;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use tokio::sync::RwLock;
use url::Url;

use super::error::status_detail;
use super::git_workspace::{GitWorkspace, RefusalContext, confirm_merge_request};
use super::http::build_forge_http_client;
use super::identity::{fork_identity_from_path, verify_fork_namespace, verify_gitlab_fork};
use super::poll::{PollSchedule, backoff_delays};
use super::{
    BranchComparison, CapabilityName, CheckStatus, CommitBase, FileChange, Forge, ForgeCredentials, ForgeError,
    ForgeIdentity, ForkIdentity, GitBinary, Mergeability, PullRequest, PushAccess, RefUpdate, RepoCoordinate,
    WriteTransport,
};

const DEFAULT_HOST: &str = "gitlab.com";
const ACCEPT_JSON: &str = "application/json";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// The lowest access level that may push a branch.
const ACCESS_LEVEL_DEVELOPER: u64 = 30;
const FORKS_PER_PAGE: u32 = 100;
const FORKS_MAX_PAGES: u32 = 10;
/// Walked to a short page: an unpaged read (default 20) misses page two and refuses the run at 86.
const ALLOWLIST_PER_PAGE: u32 = 100;
/// Exhausting this without a hit is `Unreadable`, never a miss, or an incomplete walk refuses the run.
const ALLOWLIST_MAX_PAGES: u32 = 10;
const JOB_TOKEN_PUSH_FIELD: &str = "ci_push_repository_for_job_token_allowed";
const PUBLISHING_PROJECT_VARIABLE: &str = "CI_PROJECT_PATH";
const ALLOWLIST_ENDPOINT: &str = "job_token_scope/allowlist";
const GROUPS_ALLOWLIST_ENDPOINT: &str = "job_token_scope/groups_allowlist";
const GROUP_URL_MARKER: &str = "/groups/";
const PROJECT_ENDPOINT: &str = "projects/:id";
const COMMIT_RETRY_DELAYS: [Duration; 3] = [Duration::from_secs(3), Duration::from_secs(9), Duration::from_secs(27)];

/// Project and file paths each travel as one URL segment, so their `/` and `.` must be encoded.
const SEGMENT: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'_').remove(b'~');

/// `search=^term` is a prefix filter, so a page-one miss would read as "no such branch" (exit 1).
///
/// ponytail: a walk, not `regex=^…$`, which needs a re2-escaped ref name; upgrade if an index
/// ever reaches a thousand prefix collisions.
const BRANCH_SEARCH_PER_PAGE: u32 = 100;
const BRANCH_SEARCH_MAX_PAGES: u32 = 10;
/// Several open requests can share a source branch; a miss on GitLab's default page of 20 opens a duplicate.
const MERGE_REQUEST_PER_PAGE: u32 = 100;

fn encode_segment(value: &str) -> String {
    utf8_percent_encode(value, SEGMENT).to_string()
}

/// GitLab REST v4 forge client.
pub struct GitLabForge {
    client: reqwest::Client,
    credentials: ForgeCredentials,
    base_url: String,
    transport: WriteTransport,
    /// `Some` exactly when `transport` is [`WriteTransport::Git`].
    git: Option<GitBinary>,
    /// The temporary clone, created lazily so an `api` run, or a `git` run with nothing to write, never clones.
    workspace: tokio::sync::OnceCell<GitHalf>,
    git_run: tokio::sync::Mutex<GitRun>,
    /// `namespace/project` -> numeric id; safe to cache because a project's id never changes.
    project_ids: RwLock<HashMap<String, u64>>,
}

impl GitLabForge {
    /// Build a client for `host`: `None` for gitlab.com, `Some` for a self-managed instance.
    ///
    /// `git` must be `Some` exactly when `transport` is [`WriteTransport::Git`].
    ///
    /// # Errors
    ///
    /// Returns [`ForgeError::ClientBuild`] when the HTTP client cannot be constructed.
    pub fn new(
        credentials: ForgeCredentials,
        transport: WriteTransport,
        git: Option<GitBinary>,
        host: Option<&str>,
        extra_roots: &ocx_util::tls::ExtraRoots,
    ) -> Result<Self, ForgeError> {
        // REST only: a `git` push keeps libcurl's own trust, where `GIT_SSL_CAINFO` replaces the roots.
        let base_url = testing_base_url_override().unwrap_or_else(|| api_base_url(host));
        Self::build(credentials, transport, git, base_url, extra_roots)
    }

    /// Build a client against an explicit base URL (acceptance fake-forge seam).
    ///
    /// # Errors
    ///
    /// Returns [`ForgeError::ClientBuild`] when the HTTP client cannot be constructed.
    #[cfg(any(test, feature = "__testing"))]
    pub fn with_base_url(
        credentials: ForgeCredentials,
        transport: WriteTransport,
        git: Option<GitBinary>,
        base_url: String,
    ) -> Result<Self, ForgeError> {
        Self::build(
            credentials,
            transport,
            git,
            base_url,
            &ocx_util::tls::ExtraRoots::default(),
        )
    }

    fn build(
        credentials: ForgeCredentials,
        transport: WriteTransport,
        git: Option<GitBinary>,
        base_url: String,
        extra_roots: &ocx_util::tls::ExtraRoots,
    ) -> Result<Self, ForgeError> {
        Ok(Self {
            client: build_forge_http_client(REQUEST_TIMEOUT, extra_roots)?,
            credentials,
            base_url: base_url.trim_end_matches('/').to_string(),
            transport,
            git,
            workspace: tokio::sync::OnceCell::new(),
            git_run: tokio::sync::Mutex::new(GitRun::default()),
            project_ids: RwLock::new(HashMap::new()),
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }

    /// The git remote for `repo`, from the API base: `RepoCoordinate::host` would dial gitlab.com past the
    /// acceptance seam.
    fn repository_url(&self, repo: &RepoCoordinate) -> String {
        let instance = self.base_url.strip_suffix("/api/v4").unwrap_or(&self.base_url);
        // Keep `.git`: git matches `http.<url>.*` per component, so a shortened remote loses the credential header.
        format!("{instance}/{}.git", repo.full_path())
    }

    fn project_url(&self, repo: &RepoCoordinate, suffix: &str) -> String {
        self.url(&format!("/projects/{}{suffix}", encode_segment(&repo.full_path())))
    }

    fn project_id_url(&self, id: u64, suffix: &str) -> String {
        self.url(&format!("/projects/{id}{suffix}"))
    }

    /// An authorized request: the credential travels only as a header, and an empty one sends none so
    /// the tokenless `--out` path reads unauthenticated.
    fn request(&self, method: Method, url: &str) -> reqwest::RequestBuilder {
        let builder = self.client.request(method, url).header(ACCEPT, ACCEPT_JSON);
        let credential = self.credentials.api().0.as_str();
        if credential.is_empty() {
            builder
        } else if self.credentials.api_is_job_token() {
            // Not `PRIVATE-TOKEN`: a job token has read access only under its own header.
            builder.header("JOB-TOKEN", credential)
        } else {
            builder.header("PRIVATE-TOKEN", credential)
        }
    }

    /// Whether a rejected users-API read means the endpoint is closed to a job token rather than a
    /// refused credential, which would exit 80 and tell the user to replace a working token.
    /// Both 401 and 403 count: GitLab does not document which one a job token earns.
    fn users_api_is_out_of_reach(&self, status: StatusCode) -> bool {
        (status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN) && self.credentials.api_is_job_token()
    }

    /// Whether this run pushes with a CI job token; ungated, the readable-`false` refusal would exit 86
    /// on every `--transport api` announce against a project with the setting off.
    fn job_token_push_applies(&self) -> bool {
        // The push half, not `api_is_job_token`: `OCX_ANNOUNCE_GIT_TOKEN` replaces only the push half.
        self.transport == WriteTransport::Git && self.credentials.push_is_job_token()
    }

    /// Whether the index project's job-token scope admits `publishing`, by name or by an ancestor group;
    /// reading only the projects list refuses, at 86, announces GitLab accepts.
    async fn job_token_allowlist_admits(
        &self,
        repo: &RepoCoordinate,
        publishing: &str,
    ) -> Result<AllowlistAnswer, ForgeError> {
        match self
            .allowlist_walk(repo, ALLOWLIST_ENDPOINT, project_entry_admits, publishing)
            .await?
        {
            AllowlistAnswer::Absent => {
                self.allowlist_walk(repo, GROUPS_ALLOWLIST_ENDPOINT, group_entry_admits, publishing)
                    .await
            }
            // No catch-all: a new variant must not silently skip the groups list.
            other @ (AllowlistAnswer::Admits | AllowlistAnswer::Unreadable(_)) => Ok(other),
        }
    }

    /// One admission list, walked to a short page or the ceiling.
    ///
    /// Anything but a readable list (403 below Maintainer, 404 on an older instance, an unknown entry
    /// shape) is [`AllowlistAnswer::Unreadable`], since a false refusal costs a working run; a 5xx propagates.
    async fn allowlist_walk(
        &self,
        repo: &RepoCoordinate,
        endpoint: &'static str,
        admits: fn(&Value, &str) -> EntryVerdict,
        publishing: &str,
    ) -> Result<AllowlistAnswer, ForgeError> {
        let mut saw_entry = false;
        let mut saw_named_entry = false;
        for page in 1..=ALLOWLIST_MAX_PAGES {
            let url = self.project_url(repo, &format!("/{endpoint}?per_page={ALLOWLIST_PER_PAGE}&page={page}"));
            let (status, body) = self.send(self.request(Method::GET, &url), &url).await?;
            // A job token earns 401 here, a closed door rather than a bad credential (exit 80); the
            // `api_is_job_token` gate keeps a revoked PAT on the `AuthError` path.
            let closed_to_job_token = status == StatusCode::UNAUTHORIZED && self.credentials.api_is_job_token();
            if status == StatusCode::FORBIDDEN || status == StatusCode::NOT_FOUND || closed_to_job_token {
                return Ok(AllowlistAnswer::Unreadable(endpoint));
            }
            if !status.is_success() {
                return Err(self.status_error(&url, status, &body));
            }
            // Not `get_json_optional`: its 404 `None` reads as an empty list, which is a miss and refuses.
            let Some(entries) = Self::parse_json(&url, &body)?.as_array().cloned() else {
                return Ok(AllowlistAnswer::Unreadable(endpoint));
            };
            for entry in &entries {
                saw_entry = true;
                match admits(entry, publishing) {
                    EntryVerdict::Unrecognised => continue,
                    EntryVerdict::Miss => saw_named_entry = true,
                    EntryVerdict::Admits => return Ok(AllowlistAnswer::Admits),
                }
            }
            if entries.len() < ALLOWLIST_PER_PAGE as usize {
                return Ok(if saw_entry && !saw_named_entry {
                    AllowlistAnswer::Unreadable(endpoint)
                } else {
                    AllowlistAnswer::Absent
                });
            }
        }
        Ok(AllowlistAnswer::Unreadable(endpoint))
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

    async fn project(&self, repo: &RepoCoordinate) -> Result<Option<Value>, ForgeError> {
        self.get_json_optional(&self.project_url(repo, "")).await
    }

    async fn project_id(&self, repo: &RepoCoordinate) -> Result<u64, ForgeError> {
        let key = repo.full_path();
        if let Some(id) = self.project_ids.read().await.get(&key) {
            return Ok(*id);
        }
        let url = self.project_url(repo, "");
        let body = self.project(repo).await?.ok_or_else(|| ForgeError::Status {
            url: url.clone(),
            status: StatusCode::NOT_FOUND.as_u16(),
            detail: String::new(),
        })?;
        let id = body.get("id").and_then(Value::as_u64).ok_or(ForgeError::MissingField {
            url,
            field: "id".to_string(),
        })?;
        self.project_ids.write().await.insert(key, id);
        Ok(id)
    }

    async fn authenticated_username(&self) -> Result<String, ForgeError> {
        let url = self.url("/user");
        let body = self
            .get_json_optional(&url)
            .await?
            .ok_or_else(|| ForgeError::MissingField {
                url: url.clone(),
                field: "username".to_string(),
            })?;
        body.get("username")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or(ForgeError::MissingField {
                url,
                field: "username".to_string(),
            })
    }

    /// The head commit of `branch`, or `None` when absent; every forge-supplied sha bound for git argv
    /// passes this shape check.
    async fn branch_sha(&self, repo: &RepoCoordinate, branch: &str) -> Result<Option<String>, ForgeError> {
        // The branch LIST, never `…/branches/<name>`: that endpoint 404s for a CI job token
        // (https://docs.gitlab.com/ci/jobs/ci_job_token/#job-token-access), read as `MissingBaseRef`.
        for page in 1..=BRANCH_SEARCH_MAX_PAGES {
            let url = self.project_url(
                repo,
                &format!(
                    "/repository/branches?search=%5E{}&per_page={BRANCH_SEARCH_PER_PAGE}&page={page}",
                    encode_segment(branch)
                ),
            );
            let Some(body) = self.get_json_optional(&url).await? else {
                return Ok(None);
            };
            // The exact name, never `[0]`: `^main` also matches `maintenance`, in no promised order.
            let entries = body.as_array().ok_or_else(|| ForgeError::MissingField {
                url: url.clone(),
                field: "branches[]".to_string(),
            })?;
            if let Some(entry) = entries
                .iter()
                .find(|entry| entry.get("name").and_then(Value::as_str) == Some(branch))
            {
                return entry
                    .get("commit")
                    .and_then(|commit| commit.get("id"))
                    .and_then(Value::as_str)
                    .filter(|id| is_object_name(id))
                    .map(str::to_string)
                    .ok_or(ForgeError::MissingField {
                        url,
                        field: "commit.id".to_string(),
                    })
                    .map(Some);
            }
            if entries.len() < BRANCH_SEARCH_PER_PAGE as usize {
                return Ok(None);
            }
        }
        Ok(None)
    }

    /// The commit that last touched `path` at `r#ref` (`None` if absent), the value `last_commit_id`
    /// needs: a branch sha would reject every commit whose head did not touch that file.
    async fn file_last_commit(&self, id: u64, path: &str, r#ref: &str) -> Result<Option<(String, String)>, ForgeError> {
        let url = self.project_id_url(
            id,
            &format!(
                "/repository/files/{}?ref={}",
                encode_segment(path),
                encode_segment(r#ref)
            ),
        );
        let Some(body) = self.get_json_optional(&url).await? else {
            return Ok(None);
        };
        let last_commit_id =
            body.get("last_commit_id")
                .and_then(Value::as_str)
                .ok_or_else(|| ForgeError::MissingField {
                    url: url.clone(),
                    field: "last_commit_id".to_string(),
                })?;
        Ok(Some((last_commit_id.to_string(), path.to_string())))
    }

    /// How many commits `to` (in `project`) carries that `from` (in `from_project`) does not, compared
    /// directly, never through a merge base.
    async fn ahead_count(&self, project: u64, to: &str, from_project: u64, from: &str) -> Result<usize, ForgeError> {
        let url = self.project_id_url(
            project,
            &format!(
                "/repository/compare?from={}&to={}&from_project_id={from_project}&straight=true",
                encode_segment(from),
                encode_segment(to)
            ),
        );
        let Some(body) = self.get_json_optional(&url).await? else {
            return Err(ForgeError::Status {
                url,
                status: StatusCode::NOT_FOUND.as_u16(),
                detail: String::new(),
            });
        };
        // A timed-out compare may truncate `commits`, and an undercount force-rebuilds a live branch,
        // discarding unmerged commits.
        if body.get("compare_timeout").and_then(Value::as_bool) == Some(true) {
            return Err(ForgeError::UnknownCompareStatus {
                url,
                status: "compare_timeout".to_string(),
            });
        }
        // Absent `commits` is refused, not counted as 0: `(0, 0)` reads `Identical` and force-rebuilds a live branch.
        body.get("commits")
            .and_then(Value::as_array)
            .map(Vec::len)
            .ok_or_else(|| ForgeError::MissingField {
                url,
                field: "commits".to_string(),
            })
    }

    /// Bounded readiness wait on a freshly created fork.
    async fn wait_fork_ready(&self, id: u64) -> Result<(), ForgeError> {
        let schedule = PollSchedule::default();
        let url = self.project_id_url(id, "");
        if self.probe_import(&url, schedule.request_timeout).await? {
            return Ok(());
        }
        for delay in backoff_delays(&schedule) {
            tokio::time::sleep(delay).await;
            if self.probe_import(&url, schedule.request_timeout).await? {
                return Ok(());
            }
        }
        Err(ForgeError::ForkNotReady {
            deadline_secs: schedule.deadline.as_secs(),
        })
    }

    /// `Ok(false)` while importing or unanswered; `Err` on a terminal `failed` import.
    async fn probe_import(&self, url: &str, request_timeout: Duration) -> Result<bool, ForgeError> {
        let Ok(response) = self.request(Method::GET, url).timeout(request_timeout).send().await else {
            return Ok(false);
        };
        if !response.status().is_success() {
            return Ok(false);
        }
        let Ok(body) = response.json::<Value>().await else {
            return Ok(false);
        };
        match body.get("import_status").and_then(Value::as_str) {
            // `none` is what a never-imported project reports.
            Some("finished" | "none") | None => Ok(true),
            Some("failed") => Err(ForgeError::ForkNotReady { deadline_secs: 0 }),
            Some(_) => Ok(false),
        }
    }

    /// The account's own fork of `upstream_id`, found by listing forks, never by guessing a path that a
    /// rename or concurrent create invalidates.
    async fn find_owned_fork(
        &self,
        upstream_id: u64,
        expected_namespace: &str,
    ) -> Result<Option<ForkIdentity>, ForgeError> {
        for page in 1..=FORKS_MAX_PAGES {
            let url = self.project_id_url(
                upstream_id,
                &format!("/forks?owned=true&per_page={FORKS_PER_PAGE}&page={page}"),
            );
            let Some(body) = self.get_json_optional(&url).await? else {
                return Ok(None);
            };
            let Some(entries) = body.as_array() else {
                return Ok(None);
            };
            if entries.is_empty() {
                return Ok(None);
            }
            for entry in entries {
                let Some(path) = entry.get("path_with_namespace").and_then(Value::as_str) else {
                    continue;
                };
                let Ok(identity) = fork_identity_from_path(path, entry.get("id").and_then(Value::as_u64)) else {
                    continue;
                };
                if identity.namespace.eq_ignore_ascii_case(expected_namespace) {
                    // Re-verify the parent from the project's own document; the listing's context is not trusted.
                    let coordinate = RepoCoordinate {
                        host: None,
                        namespace: identity.namespace.clone(),
                        project: identity.project.clone(),
                    };
                    let url = self.project_url(&coordinate, "");
                    let Some(project) = self.get_json_optional(&url).await? else {
                        continue;
                    };
                    return verify_gitlab_fork(&project, upstream_id).map(Some);
                }
            }
            if entries.len() < FORKS_PER_PAGE as usize {
                return Ok(None);
            }
        }
        Ok(None)
    }

    /// The temporary clone, opened on first use; a later call for another base or branch fetches it,
    /// one for another repository is refused.
    ///
    /// # Errors
    ///
    /// Returns [`ForgeError::GitUnavailable`] without a resolved `git`, when the clone cannot be
    /// created, or for a different repository; propagates read and fetch failures otherwise.
    async fn git_half(&self, repo: &RepoCoordinate, base: &str, branch: &str) -> Result<&GitHalf, ForgeError> {
        // `OnceCell` hands every caller the first caller's clone, so each call is checked against
        // `opened_for`, or `commit_files` parents on a sha the clone never fetched.
        let half = self
            .workspace
            .get_or_try_init(|| async {
                // An error, not `expect`: the acceptance seam can build a `git` transport without a binary.
                let git = self.git.as_ref().ok_or_else(|| ForgeError::GitUnavailable {
                    reason: "the git write transport was selected without a resolved git binary".to_string(),
                })?;
                // Read at open time: a sha re-read at push time degrades `--force-with-lease` to a plain force.
                let branch_head = self.branch_sha(repo, branch).await?;
                let workspace = GitWorkspace::open(
                    git,
                    &self.repository_url(repo),
                    base,
                    branch_head.as_ref().map(|_| branch),
                    self.credentials.push(),
                )
                .await?;
                Ok(GitHalf {
                    workspace,
                    branch_head,
                    opened_for: OpenedFor {
                        repo: repo.full_path(),
                        base: base.to_string(),
                        branch: branch.to_string(),
                    },
                })
            })
            .await?;

        match half.opened_for.reuse_for(&repo.full_path(), base, branch) {
            Reuse::Ready => {}
            Reuse::Fetch => half.workspace.fetch(base, branch).await?,
            Reuse::Refuse => {
                return Err(ForgeError::GitUnavailable {
                    reason: format!(
                        "the git workspace was opened for {} and cannot serve {}; one run writes to one repository",
                        half.opened_for.repo,
                        repo.full_path()
                    ),
                });
            }
        }
        Ok(half)
    }

    /// The recorded preflight, or a fresh one: a push handed [`PushAccess::skipped_all`] instead
    /// would classify every capability refusal as 77, not 86.
    ///
    /// # Errors
    ///
    /// Propagates [`Forge::ensure_push_access`], including its refusals.
    async fn push_preflight(&self, repo: &RepoCoordinate) -> Result<PushAccess, ForgeError> {
        let recorded = self.git_run.lock().await.preflight.clone();
        match recorded {
            Some(preflight) => Ok(preflight),
            None => self.ensure_push_access(repo).await,
        }
    }

    /// The git half of [`Forge::open_or_update_pull_request`]: push a pending commit, return an
    /// already-open request without writing, or push a refresh commit for stranded content.
    ///
    /// # Errors
    ///
    /// Returns the push classifier's error, or [`ForgeError::MergeRequestUnconfirmed`] when the push
    /// landed but no merge request appeared.
    async fn publish_over_git(
        &self,
        index: &RepoCoordinate,
        head: &RepoCoordinate,
        branch: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<PullRequest, ForgeError> {
        // Taken on every attempt: a losing retry skips `commit_files` and lands here, and re-pushing the
        // rejected commit fails identically.
        let pending = std::mem::take(&mut self.git_run.lock().await.pending);
        if matches!(pending, PendingCommit::None)
            && let Some(existing) = self.find_open_pull_request(index, head, branch).await?
        {
            // Before the preflight and before any `git`: an unchanged announce must succeed with push capability off.
            return Ok(existing);
        }

        // Preflight before any `git` process: its refusals promise "before any push".
        let preflight = self.push_preflight(index).await?;

        let repository = index.full_path();
        let half = self.git_half(index, base, branch).await?;
        let lease = match pending {
            PendingCommit::Ready { lease } => lease,
            PendingCommit::None => {
                // Fetch first: a losing retry lands here with a view older than the winning writer. The refresh
                // commit exists because a server processes push options only for a push that moves a ref.
                half.workspace.fetch(base, branch).await?;
                half.workspace.refresh_commit(branch, title).await?;
                None
            }
        };
        // Before the push: a refusal returns through `?`, and the post-rejection re-fetch keys on this flag.
        self.git_run.lock().await.push_attempted = true;
        half.workspace
            .push(
                branch,
                base,
                title,
                body,
                lease.as_deref(),
                RefusalContext {
                    preflight: &preflight,
                    repo: &repository,
                },
            )
            .await?;

        // From the clone's view: after the push, a REST read finds the request the push just opened.
        let updated = half.branch_head.is_some();
        let mut confirmed = confirm_merge_request(|| self.find_open_pull_request(index, head, branch)).await?;
        confirmed.updated = updated;
        Ok(confirmed)
    }

    /// The capability rows behind [`Forge::ensure_push_access`], split out so its recording has one exit.
    async fn push_access_checks(&self, repo: &RepoCoordinate) -> Result<PushAccess, ForgeError> {
        let project = self.project(repo).await?;
        // Only under `git`: `api_is_job_token` can hold under `--transport api`, where tolerating the 404
        // turns exit 80 into a bare exit 1 from `commit_files`.
        let unreadable_under_job_token =
            project.is_none() && self.credentials.api_is_job_token() && self.transport == WriteTransport::Git;
        let access = project.as_ref().map_or(0, project_access_level);
        // A job token gets 404 from the Projects API; refusing on it would exit 80 before a push GitLab accepts.
        if access < ACCESS_LEVEL_DEVELOPER && !unreadable_under_job_token {
            return Err(ForgeError::PushAccessDenied { repo: repo.full_path() });
        }
        // Record each row once: `record` replaces, so a second call walks its status back.
        let mut checks = PushAccess::skipped_all();
        if let Some(git) = self.git.as_ref() {
            checks.record(
                CapabilityName::GitVersion,
                CheckStatus::Passed,
                Some(format!("git {}", git.version)),
            );
        }
        if unreadable_under_job_token {
            // `Unknown`: the level read belongs to the API token, the push token may differ, and the push decides.
            checks.record(
                CapabilityName::PushAccess,
                CheckStatus::Unknown,
                Some(PROJECT_ENDPOINT.to_string()),
            );
        } else {
            checks.record(
                CapabilityName::PushAccess,
                CheckStatus::Passed,
                Some(format!("access level {access}")),
            );
        }
        // Rows left `Skipped` read as "does not apply", not "could not be read".
        if !self.job_token_push_applies() {
            return Ok(checks);
        }

        // `Value::as_bool`, not a defaulted `bool`: absent or null must stay unknown, or every pre-18.4
        // instance is refused.
        match project
            .as_ref()
            .and_then(|body| body.get(JOB_TOKEN_PUSH_FIELD))
            .and_then(Value::as_bool)
        {
            Some(true) => checks.record(CapabilityName::JobTokenPush, CheckStatus::Passed, None),
            Some(false) => {
                return Err(ForgeError::WriteCapabilityUnavailable {
                    capability: CapabilityName::JobTokenPush,
                    repo: repo.full_path(),
                    remedy: format!(
                        "enable Settings → CI/CD → Job token permissions on {}",
                        repo.full_path()
                    ),
                });
            }
            None => checks.record(
                CapabilityName::JobTokenPush,
                CheckStatus::Unknown,
                Some(JOB_TOKEN_PUSH_FIELD.to_string()),
            ),
        }

        // Unknown, never a refusal: a runner that scrubs `CI_PROJECT_PATH` must still push (a safety gate,
        // not a security one: a hostile value can suppress it).
        let Some(publishing) = self.credentials.publishing_project() else {
            checks.record(
                CapabilityName::JobTokenAllowlist,
                CheckStatus::Unknown,
                Some(PUBLISHING_PROJECT_VARIABLE.to_string()),
            );
            return Ok(checks);
        };
        // The index project needs no allowlist entry; case-insensitive because `CI_PROJECT_PATH` is operator-typed.
        if publishing.eq_ignore_ascii_case(&repo.full_path()) {
            return Ok(checks);
        }
        match self.job_token_allowlist_admits(repo, publishing).await? {
            AllowlistAnswer::Admits => checks.record(CapabilityName::JobTokenAllowlist, CheckStatus::Passed, None),
            AllowlistAnswer::Unreadable(endpoint) => {
                checks.record(
                    CapabilityName::JobTokenAllowlist,
                    CheckStatus::Unknown,
                    Some(endpoint.to_string()),
                );
            }
            AllowlistAnswer::Absent => {
                return Err(ForgeError::WriteCapabilityUnavailable {
                    capability: CapabilityName::JobTokenAllowlist,
                    repo: repo.full_path(),
                    remedy: format!(
                        "add {publishing}, or one of its groups, to Settings → CI/CD → Job token permissions on {}",
                        repo.full_path()
                    ),
                });
            }
        }
        Ok(checks)
    }

    /// One attempt at the atomic commit; a delete of a path absent at the base sends no action, since
    /// GitLab 400s the whole commit on it.
    async fn commit_files_once(
        &self,
        repo: &RepoCoordinate,
        branch: &str,
        base: CommitBase<'_>,
        message: &str,
        files: &BTreeMap<String, FileChange>,
        update: RefUpdate,
    ) -> Result<String, ForgeError> {
        let target_id = self.project_id(repo).await?;
        let base_id = self.project_id(base.repo).await?;
        let branch_exists = self.branch_sha(repo, branch).await?.is_some();

        let mut actions = Vec::with_capacity(files.len());
        for (path, change) in files {
            // Asked at the base, the tree the commit is built on: GitLab has no upsert, and a wrong
            // `create`/`update` fails the whole commit.
            let existing = self.file_last_commit(base_id, path, base.sha).await?;
            let mut action = match change {
                FileChange::Put(contents) => json!({
                    "file_path": path,
                    "content": BASE64_STANDARD.encode(contents),
                    "encoding": "base64",
                    "action": if existing.is_some() { "update" } else { "create" },
                }),
                FileChange::Delete => {
                    if existing.is_none() {
                        continue;
                    }
                    json!({ "file_path": path, "action": "delete" })
                }
            };
            // Per-file compare-and-swap, as safe as `RefUpdate::FastForward` only because every announce
            // rewrites the root document; without it a concurrent announce is silently overwritten.
            if let Some((last_commit_id, _)) = existing {
                action["last_commit_id"] = json!(last_commit_id);
            }
            actions.push(action);
        }

        let mut body = json!({
            "branch": branch,
            "commit_message": message,
            "actions": actions,
        });
        // `start_*` only to create or `Reset` the branch: onto a live branch GitLab refuses it as
        // "branch already exists".
        let reset = matches!(update, RefUpdate::Reset);
        if !branch_exists || reset {
            body["start_sha"] = json!(base.sha);
            body["start_project"] = json!(base_id);
        }
        if reset && branch_exists {
            body["force"] = json!(true);
        }

        let url = self.project_id_url(target_id, "/repository/commits");
        let (status, response) = self.send(self.json_request(Method::POST, &url, &body)?, &url).await?;
        if status.is_success() {
            let value = Self::parse_json(&url, &response)?;
            return value
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or(ForgeError::MissingField {
                    url,
                    field: "id".to_string(),
                });
        }
        if is_stale_base(status, &response) {
            return Err(ForgeError::NonFastForward {
                branch: branch.to_string(),
            });
        }
        Err(self.status_error(&url, status, &response))
    }
}

#[async_trait::async_trait]
impl Forge for GitLabForge {
    /// `GET /user`; a job token or empty credential is [`ForgeError::UsersApiUnavailable`], never
    /// `Ok(None)`, so a bare `LOGIN` never becomes an id.
    async fn authenticated_identity(&self) -> Result<Option<ForgeIdentity>, ForgeError> {
        // No request: without a header the 401 reads as a rejected credential (exit 80), breaking tokenless `--out`.
        if self.credentials.api().0.is_empty() {
            return Err(ForgeError::UsersApiUnavailable);
        }
        let url = self.url("/user");
        let (status, body) = self.send(self.request(Method::GET, &url), &url).await?;
        if self.users_api_is_out_of_reach(status) {
            return Err(ForgeError::UsersApiUnavailable);
        }
        if !status.is_success() {
            return Err(self.status_error(&url, status, &body));
        }
        identity_from_body(&url, &Self::parse_json(&url, &body)?).map(Some)
    }

    /// `GET /users?username={login}`, `None` when no entry matches.
    async fn resolve_user(&self, login: &str) -> Result<Option<ForgeIdentity>, ForgeError> {
        // GitLab reads an absent filter as "every user".
        if login.is_empty() {
            return Ok(None);
        }
        // Encoded, or an operator-supplied `--owner` appends a parameter to an authenticated request.
        let url = self.url(&format!("/users?username={}", encode_segment(login)));
        let (status, body) = self.send(self.request(Method::GET, &url), &url).await?;
        if self.users_api_is_out_of_reach(status) {
            return Err(ForgeError::UsersApiUnavailable);
        }
        if !status.is_success() {
            return Err(self.status_error(&url, status, &body));
        }
        // A non-list body is a decode error, or a rewriting gateway reads as an unknown login.
        let entries: Vec<Value> = serde_json::from_slice(&body).map_err(|source| ForgeError::Decode {
            url: url.clone(),
            source,
        })?;
        // The matching `username`, never `[0]`, which could publish the wrong owner into an index root.
        let Some(entry) = entries.iter().find(|entry| {
            entry
                .get("username")
                .and_then(Value::as_str)
                .is_some_and(|username| username.eq_ignore_ascii_case(login))
        }) else {
            return Ok(None);
        };
        identity_from_body(&url, entry).map(Some)
    }

    async fn get_file_contents(
        &self,
        repo: &RepoCoordinate,
        path: &str,
        r#ref: &str,
    ) -> Result<Option<Vec<u8>>, ForgeError> {
        let url = self.project_url(
            repo,
            &format!(
                "/repository/files/{}/raw?ref={}",
                encode_segment(path),
                encode_segment(r#ref)
            ),
        );
        let (status, body) = self.send(self.request(Method::GET, &url), &url).await?;
        if status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(self.status_error(&url, status, &body));
        }
        Ok(Some(body.to_vec()))
    }

    async fn get_ref_sha(&self, repo: &RepoCoordinate, r#ref: &str) -> Result<Option<String>, ForgeError> {
        // A full `refs/heads/x` would encode as one segment and 404, and the claim path would create a
        // duplicate branch.
        let branch = r#ref
            .strip_prefix("refs/heads/")
            .or_else(|| r#ref.strip_prefix("heads/"))
            .unwrap_or(r#ref);
        self.branch_sha(repo, branch).await
    }

    /// The one read that differs by transport: under `git` it comes from the clone, so the commit that
    /// follows is built on the objects the answer came from.
    async fn compare_branch(
        &self,
        repo: &RepoCoordinate,
        base: &str,
        head: &RepoCoordinate,
        head_branch: &str,
    ) -> Result<BranchComparison, ForgeError> {
        if self.transport == WriteTransport::Git {
            let half = self.git_half(repo, base, head_branch).await?;
            return half.workspace.compare(base, head_branch).await;
        }
        let base_id = self.project_id(repo).await?;
        let head_id = self.project_id(head).await?;
        // Both directions: one alone cannot tell `Ahead` from `Diverged`, and a diverged branch read as
        // ahead re-proposes squash-merged work.
        let ahead = self.ahead_count(head_id, head_branch, base_id, base).await?;
        let behind = self.ahead_count(base_id, base, head_id, head_branch).await?;
        Ok(match (ahead, behind) {
            (0, 0) => BranchComparison::Identical,
            (_, 0) => BranchComparison::Ahead,
            (0, _) => BranchComparison::Behind,
            _ => BranchComparison::Diverged,
        })
    }

    async fn find_open_pull_request(
        &self,
        index: &RepoCoordinate,
        head: &RepoCoordinate,
        branch: &str,
    ) -> Result<Option<PullRequest>, ForgeError> {
        // Narrowed to this head, so a fork with the same branch name is never adopted. Onto the index
        // itself by path, never numeric id: `GET /projects/:id` 404s for a job token.
        let same_project = head.full_path() == index.full_path();
        let source_filter = if same_project {
            String::new()
        } else {
            format!("&source_project_id={}", self.project_id(head).await?)
        };
        let url = self.project_url(
            index,
            &format!(
                "/merge_requests?state=opened&per_page={MERGE_REQUEST_PER_PAGE}&source_branch={}{source_filter}",
                encode_segment(branch)
            ),
        );
        let (status, body) = self.send(self.request(Method::GET, &url), &url).await?;
        if !status.is_success() {
            return Err(self.status_error(&url, status, &body));
        }
        let list = Self::parse_json(&url, &body)?;
        let Some(existing) = list.as_array().and_then(|requests| {
            requests
                .iter()
                .find(|request| !same_project || opened_onto_its_own_project(request))
        }) else {
            return Ok(None);
        };
        merge_request_from_body(&url, existing, true).map(Some)
    }

    /// Whether merge request `number` (a project-local `iid`) merges cleanly; GitLab's "still
    /// checking" is [`Mergeability::Unknown`], never polled.
    ///
    /// # Errors
    ///
    /// Returns a [`ForgeError`] on transport failure or a non-success status other than 404.
    async fn pull_request_mergeability(&self, index: &RepoCoordinate, number: u64) -> Result<Mergeability, ForgeError> {
        // By encoded path: only that form is job-token-readable.
        let url = self.project_url(index, &format!("/merge_requests/{number}"));
        let Some(body) = self.get_json_optional(&url).await? else {
            return Ok(Mergeability::Unknown);
        };
        Ok(mergeability_from_merge_request(&body))
    }

    /// Refused under `git` before any request: reading the upstream id first would fail with a
    /// different error on a project the git credential cannot see.
    async fn find_fork(
        &self,
        upstream: &RepoCoordinate,
        fork: &RepoCoordinate,
    ) -> Result<Option<ForkIdentity>, ForgeError> {
        if self.transport == WriteTransport::Git {
            return Err(ForgeError::TransportOperationUnsupported {
                operation: "fork lookup".to_string(),
                transport: self.transport,
            });
        }
        let upstream_id = self.project_id(upstream).await?;
        let Some(project) = self.project(fork).await? else {
            return Ok(None);
        };
        // A same-named stranger is "no fork here"; the create path refuses it.
        let Ok(identity) = verify_gitlab_fork(&project, upstream_id) else {
            return Ok(None);
        };
        verify_fork_namespace(&identity, &fork.namespace)?;
        Ok(Some(identity))
    }

    /// Refused under `git` before any request, like [`Self::find_fork`].
    async fn ensure_fork(
        &self,
        upstream: &RepoCoordinate,
        target_owner: Option<&str>,
    ) -> Result<ForkIdentity, ForgeError> {
        if self.transport == WriteTransport::Git {
            return Err(ForgeError::TransportOperationUnsupported {
                operation: "fork creation".to_string(),
                transport: self.transport,
            });
        }
        let expected_namespace = match target_owner {
            Some(owner) => owner.to_string(),
            None => self.authenticated_username().await?,
        };
        // A self-fork 409s, and the reuse path would spend its whole budget hunting a fork of itself.
        if expected_namespace.eq_ignore_ascii_case(&upstream.namespace) {
            return Err(ForgeError::SelfForkRefused {
                upstream: upstream.to_string(),
                namespace: expected_namespace,
            });
        }
        let upstream_id = self.project_id(upstream).await?;
        let conventional = upstream.with_namespace(expected_namespace.clone());
        if let Some(identity) = self.find_fork(upstream, &conventional).await? {
            return Ok(identity);
        }

        let url = self.project_id_url(upstream_id, "/fork");
        let body = json!({ "namespace_path": expected_namespace });
        let (status, response) = self.send(self.json_request(Method::POST, &url, &body)?, &url).await?;
        let identity = if status.is_success() {
            let value = Self::parse_json(&url, &response)?;
            verify_gitlab_fork(&value, upstream_id)?
        } else if status == StatusCode::CONFLICT {
            self.find_owned_fork(upstream_id, &expected_namespace)
                .await?
                .ok_or_else(|| self.status_error(&url, status, &response))?
        } else {
            return Err(self.status_error(&url, status, &response));
        };
        verify_fork_namespace(&identity, &expected_namespace)?;
        // Never skip the readiness wait on a missing `id`: a commit into a still-importing fork surfaces
        // as an unexplained 404.
        let id = identity.id.ok_or_else(|| ForgeError::MissingField {
            url: url.clone(),
            field: "id".to_string(),
        })?;
        self.wait_fork_ready(id).await?;
        Ok(identity)
    }

    /// A no-op: the commit names its `start_project`, so the base is reachable without syncing.
    async fn sync_fork(&self, fork: &RepoCoordinate, branch: &str) {
        tracing::debug!(
            fork = %fork.full_path(),
            branch,
            "fork sync is a no-op on GitLab: the commit names its start project"
        );
    }

    /// The whole capability array: a `false` job-token setting or an allowlist miss refuses at 86 before
    /// any push, an unreadable one records `unknown` and proceeds.
    async fn ensure_push_access(&self, repo: &RepoCoordinate) -> Result<PushAccess, ForgeError> {
        // No `Failed` status exists: a refusal is the error, never a row in the returned array.
        let checks = self.push_access_checks(repo).await?;
        // Kept for the push inside `open_or_update_pull_request`, whose exit-86 promotion reads this array.
        if self.transport == WriteTransport::Git {
            self.git_run.lock().await.preflight = Some(checks.clone());
        }
        Ok(checks)
    }

    /// One atomic commit under `api`; a local commit in the clone under `git`, which writes nothing.
    ///
    /// Only REST requests are replayed; a lost `git` compare-and-swap is
    /// [`ForgeError::NonFastForward`] at once, for the caller's own retry.
    async fn commit_files(
        &self,
        repo: &RepoCoordinate,
        branch: &str,
        base: CommitBase<'_>,
        message: &str,
        files: &BTreeMap<String, FileChange>,
        update: RefUpdate,
    ) -> Result<String, ForgeError> {
        if self.transport == WriteTransport::Git {
            let half = self.git_half(repo, base.branch, branch).await?;
            // After a rejected push, re-fetch so the rebuild parents on the winning head, or the push is
            // refused identically.
            if self.git_run.lock().await.push_attempted {
                // Both refspecs: a rejection proves the branch exists, whatever the opening read saw.
                half.workspace.fetch(base.branch, branch).await?;
            }
            let committed = half
                .workspace
                .commit_files(branch, base, message, files, update)
                .await?;
            // Lease only a rebuild, on the head read at open time: a sha re-read after the race is a plain force.
            let lease = matches!(update, RefUpdate::Reset)
                .then(|| half.branch_head.clone())
                .flatten();
            self.git_run.lock().await.pending = PendingCommit::Ready { lease };
            return Ok(committed);
        }
        let mut outcome = self.commit_files_once(repo, branch, base, message, files, update).await;
        for delay in COMMIT_RETRY_DELAYS {
            let Err(error) = &outcome else { break };
            if !is_retryable(error) {
                break;
            }
            tracing::debug!(%error, "replaying the GitLab commit");
            tokio::time::sleep(delay).await;
            outcome = self.commit_files_once(repo, branch, base, message, files, update).await;
        }
        outcome
    }

    /// One `POST /merge_requests` under `api`; under `git`, [`Self::publish_over_git`].
    async fn open_or_update_pull_request(
        &self,
        index: &RepoCoordinate,
        head: &RepoCoordinate,
        branch: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<PullRequest, ForgeError> {
        if self.transport == WriteTransport::Git {
            return self.publish_over_git(index, head, branch, base, title, body).await;
        }
        let index_id = self.project_id(index).await?;
        let head_id = self.project_id(head).await?;
        // Created on the SOURCE project, naming its target by id (the reverse of GitHub).
        let url = self.project_id_url(head_id, "/merge_requests");
        let mut request = json!({
            "source_branch": branch,
            "target_branch": base,
            "title": title,
            "description": body,
        });
        if head_id != index_id {
            request["target_project_id"] = json!(index_id);
        }
        let (status, response) = self
            .send(self.json_request(Method::POST, &url, &request)?, &url)
            .await?;
        if status.is_success() {
            let value = Self::parse_json(&url, &response)?;
            return merge_request_from_body(&url, &value, false);
        }
        // 409 or an "already exists" body means reuse; any other failure (400 for nothing to propose) is
        // reported, not masked as a missing request.
        if status != StatusCode::CONFLICT && !merge_request_already_exists(&response) {
            return Err(self.status_error(&url, status, &response));
        }
        self.find_open_pull_request(index, head, branch)
            .await?
            .ok_or_else(|| ForgeError::MissingField {
                url,
                field: "merge_request".to_string(),
            })
    }
}

/// The temporary clone and the branch head it was taken at.
struct GitHalf {
    workspace: GitWorkspace,
    /// `None` when the branch did not exist; the lease and the opened-vs-updated verdict must not
    /// re-read it after the push.
    branch_head: Option<String>,
    opened_for: OpenedFor,
}

/// The `(repository, base, branch)` the clone was taken for; the repository is a path, since
/// comparing coordinates calls two spellings (`host: None` or set) different.
#[derive(Debug)]
struct OpenedFor {
    repo: String,
    base: String,
    branch: String,
}

/// What a later caller must do before using the one clone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reuse {
    Ready,
    Fetch,
    Refuse,
}

impl OpenedFor {
    fn reuse_for(&self, repo: &str, base: &str, branch: &str) -> Reuse {
        // Refused, not fetched: the clone's credential scope belongs to the first repository, and fetching
        // another would carry the credential there.
        if self.repo != repo {
            return Reuse::Refuse;
        }
        // Fetch: an unfetched ref reads as absent, and `commit_files` would parent on the base and drop
        // the branch's tags.
        if self.base != base || self.branch != branch {
            return Reuse::Fetch;
        }
        Reuse::Ready
    }
}

/// Whether the git half holds a commit the server has not been offered; the clone's refs cannot
/// tell, since they differ after a rejection too.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum PendingCommit {
    /// Nothing committed, or already offered and rejected.
    #[default]
    None,
    Ready {
        /// `Some` only for a [`RefUpdate::Reset`] rebuild of an existing branch.
        lease: Option<String>,
    },
}

/// State the git half carries between trait calls; parameters would expose the transport.
#[derive(Default)]
struct GitRun {
    /// Threaded to the push so its promotion to exit 86 stays reachable.
    preflight: Option<PushAccess>,
    /// Set by [`Forge::commit_files`], taken on every push attempt, successful or not.
    pending: PendingCommit,
    /// Whether a push was offered: a rebuild after a rejection must re-fetch, and the clone cannot tell
    /// a first commit from a re-commit.
    push_attempted: bool,
}

/// What the job-token allowlists said: `Absent` refuses the run, `Unreadable` must not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AllowlistAnswer {
    Admits,
    /// Both lists were read and neither admits it.
    Absent,
    /// A list unread or unrecognised, named so `detail` points at the right one.
    Unreadable(&'static str),
}

/// One allowlist entry's verdict; a list with no recognised entry is unknown, never a refusal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EntryVerdict {
    Unrecognised,
    Miss,
    Admits,
}

impl EntryVerdict {
    fn from_match(admits: bool) -> Self {
        if admits { Self::Admits } else { Self::Miss }
    }
}

/// Matched on `path_with_namespace`, the only identity `CI_PROJECT_PATH` gives, case-insensitively
/// because the variable is operator-typed.
fn project_entry_admits(entry: &Value, publishing: &str) -> EntryVerdict {
    let Some(path) = entry.get("path_with_namespace").and_then(Value::as_str) else {
        return EntryVerdict::Unrecognised;
    };
    EntryVerdict::from_match(path.eq_ignore_ascii_case(publishing))
}

/// A groups entry carries only `id`, `web_url` and `name`, so the group path comes from the URL.
fn group_entry_admits(entry: &Value, publishing: &str) -> EntryVerdict {
    let group = entry
        .get("web_url")
        .and_then(Value::as_str)
        .and_then(group_path_from_web_url);
    let Some(group) = group else {
        return EntryVerdict::Unrecognised;
    };
    EntryVerdict::from_match(group_contains(&group, publishing))
}

/// The group's full path, from its `web_url`.
///
/// ponytail: the segment after the first `/groups/`, not a strip of the API base, which a reverse
/// proxy with a different web host breaks. Ceiling: an instance rooted under a path ending in
/// `/groups`; upgrade to `GET /groups/:id` (`full_path`), one request per entry.
fn group_path_from_web_url(web_url: &str) -> Option<String> {
    // The parsed path, never the raw string: a query would fold in and read as a miss, a false refusal at 86.
    let url = Url::parse(web_url).ok()?;
    let (_, path) = url.path().split_once(GROUP_URL_MARKER)?;
    let path = path.trim_end_matches('/');
    (!path.is_empty()).then(|| path.to_string())
}

/// Whether `publishing` lies under `group` at any depth, case-insensitively.
fn group_contains(group: &str, publishing: &str) -> bool {
    // A path-component prefix: `acme/packages` must not admit `acme/packages-legacy/widget`, a
    // `passed` GitLab refuses. The separator check runs first, or the slice can panic off a char boundary.
    publishing.as_bytes().get(group.len()) == Some(&b'/') && publishing[..group.len()].eq_ignore_ascii_case(group)
}

/// The higher of the project and group access levels (either may be null); anything unreadable
/// is 0, never "may push".
fn project_access_level(body: &Value) -> u64 {
    let Some(permissions) = body.get("permissions") else {
        return 0;
    };
    let level = |key: &str| {
        permissions
            .get(key)
            .and_then(|access| access.get("access_level"))
            .and_then(Value::as_u64)
    };
    level("project_access")
        .unwrap_or(0)
        .max(level("group_access").unwrap_or(0))
}

/// Whether `value` is a full git object name (40 or 64 hex digits).
// Forge-controlled input bound for `git read-tree` and `--force-with-lease`: a leading `-` would
// parse as an option (`--upload-pack` runs a program).
fn is_object_name(value: &str) -> bool {
    // Full length only: an abbreviated name is ambiguous, and the Branches API returns the full `commit.id`.
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Build a [`ForgeIdentity`] from a `/user` document or a `/users` entry.
///
/// `bot` defaults to `false` (older instances omit it); a missing or non-integer `id` is an error,
/// since a placeholder id would be published as the owner's forge identity.
fn identity_from_body(url: &str, value: &Value) -> Result<ForgeIdentity, ForgeError> {
    let missing = |field: &str| ForgeError::MissingField {
        url: url.to_string(),
        field: field.to_string(),
    };
    let login = value
        .get("username")
        .and_then(Value::as_str)
        .ok_or_else(|| missing("username"))?
        .to_string();
    let id = value.get("id").and_then(Value::as_u64).ok_or_else(|| missing("id"))?;
    Ok(ForgeIdentity {
        login,
        id,
        bot: value.get("bot").and_then(Value::as_bool).unwrap_or(false),
    })
}

/// Always https: the credential rides as a header, and plaintext would put it on the wire.
fn api_base_url(host: Option<&str>) -> String {
    format!("https://{}/api/v4", host.unwrap_or(DEFAULT_HOST))
}

/// Whether a failed commit means the base moved (a stale `last_commit_id` or a lost branch-create
/// race); the caller re-reads and regenerates, since a blind replay fails again or, with `force`,
/// discards the concurrent announce.
fn is_stale_base(status: StatusCode, body: &[u8]) -> bool {
    if status != StatusCode::BAD_REQUEST {
        return false;
    }
    let body = String::from_utf8_lossy(body).to_lowercase();
    body.contains("changed since you started editing")
        || body.contains("stale")
        || body.contains("already exists")
        || body.contains("invalid reference name")
}

fn merge_request_already_exists(body: &[u8]) -> bool {
    String::from_utf8_lossy(body).to_lowercase().contains("already exists")
}

/// A 429 or 5xx, safe to replay since the commit is atomic; never a 400, which GitLab also spends
/// on genuine validation failures.
fn is_retryable(error: &ForgeError) -> bool {
    matches!(
        error,
        ForgeError::Status { status, .. }
            if *status == StatusCode::TOO_MANY_REQUESTS.as_u16() || (500..600).contains(status)
    )
}

/// Whether a merge request targets its own source project; a missing id is no match, so announce
/// proposes its own request rather than adopt one of unread provenance.
fn opened_onto_its_own_project(request: &Value) -> bool {
    let source = request.get("source_project_id").and_then(Value::as_u64);
    let target = request.get("target_project_id").and_then(Value::as_u64);
    matches!((source, target), (Some(source), Some(target)) if source == target)
}

/// Build a [`PullRequest`] from a merge-request body, numbered by `iid` (the `!123` a human sees),
/// never the global `id`.
fn merge_request_from_body(url: &str, value: &Value, updated: bool) -> Result<PullRequest, ForgeError> {
    let number = value
        .get("iid")
        .and_then(Value::as_u64)
        .ok_or_else(|| ForgeError::MissingField {
            url: url.to_string(),
            field: "iid".to_string(),
        })?;
    let html_url = value
        .get("web_url")
        .and_then(Value::as_str)
        .ok_or_else(|| ForgeError::MissingField {
            url: url.to_string(),
            field: "web_url".to_string(),
        })?
        .to_string();
    Ok(PullRequest {
        number,
        html_url,
        updated,
    })
}

/// A merge-request body's merge status: `has_conflicts` decides first, `detailed_merge_status` is
/// the fallback.
fn mergeability_from_merge_request(value: &Value) -> Mergeability {
    if value.get("has_conflicts").and_then(Value::as_bool) == Some(true) {
        return Mergeability::Conflicting;
    }
    match value.get("detailed_merge_status").and_then(Value::as_str) {
        // `broken_status` is a conflict: an unchanged run must never report success over an unmergeable request.
        Some("conflict" | "broken_status") => Mergeability::Conflicting,
        Some("checking" | "unchecked") => Mergeability::Unknown,
        // Any other value (failed pipeline, missing approval, draft) is not the conflict this detects, so
        // it reads as mergeable.
        _ => Mergeability::Mergeable,
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
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::{TcpListener, TcpStream};

    use super::super::{ForgeToken, GitVersion};
    use super::*;

    /// C-007 / DX-5: the roots handed to `build` are what make the forge
    /// trust the operator's root — a client built with the default set does
    /// not. Same shape as the GitHub sibling; the REST client is the only
    /// one this covers (the `git` transport is libcurl's, see `new`'s doc).
    ///
    /// Mutation: have `build` pass `&ExtraRoots::default()` to
    /// `build_forge_http_client` — the second half reds.
    #[tokio::test(flavor = "multi_thread")]
    async fn extra_ca_build_threads_the_roots_into_the_private_client() {
        use ocx_test_support::pki::{TestPki, assert_untrusted_root, error_chain, serve_https};

        let pki = TestPki::mint();
        let addr = serve_https(&pki).await;
        let credentials = || ForgeCredentials::new(ForgeToken::new("token".to_string()));
        let forge = GitLabForge::with_base_url(credentials(), WriteTransport::Api, None, format!("https://{addr}"))
            .expect("the forge builds");

        let error = forge
            .client
            .get(forge.url("/user"))
            .send()
            .await
            .expect_err("a client built with the default roots does not know the minted root");
        let chain = error_chain(&error);
        assert_untrusted_root(&chain);

        let forge = GitLabForge::build(
            credentials(),
            WriteTransport::Api,
            None,
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

    fn coordinate(value: &str) -> RepoCoordinate {
        value.parse().expect("valid coordinate")
    }

    #[test]
    fn api_base_url_is_the_instance_itself() {
        assert_eq!(api_base_url(None), "https://gitlab.com/api/v4");
        assert_eq!(
            api_base_url(Some("gitlab.example.com")),
            "https://gitlab.example.com/api/v4"
        );
    }

    #[test]
    fn a_nested_project_path_is_one_encoded_segment() {
        // The whole point of the encoding: a subgroup path must not split into
        // multiple URL segments, or GitLab reads it as a different endpoint.
        let encoded = encode_segment(&coordinate("gitlab.com/acme/platform/index").full_path());
        assert_eq!(encoded, "acme%2Fplatform%2Findex");
        assert!(!encoded.contains('/'), "a project path must survive as one segment");
    }

    #[test]
    fn file_paths_encode_their_separators_and_dots() {
        assert_eq!(encode_segment("p/acme/widget.json"), "p%2Facme%2Fwidget%2Ejson");
    }

    #[test]
    fn stale_base_is_recognised_only_on_a_400() {
        let stale =
            br#"{"message":"You are attempting to update a file that has changed since you started editing it."}"#;
        assert!(is_stale_base(StatusCode::BAD_REQUEST, stale));
        // The same body on another status is not the CAS firing — a 500 carrying
        // it is a forge fault, and classifying it as a lost race would send the
        // caller into a regeneration that cannot help.
        assert!(!is_stale_base(StatusCode::INTERNAL_SERVER_ERROR, stale));
        // And an ordinary validation failure must not be read as a lost race.
        assert!(!is_stale_base(
            StatusCode::BAD_REQUEST,
            br#"{"message":"A file with this name doesn't exist"}"#
        ));
    }

    #[test]
    fn merge_requests_report_the_project_local_number() {
        let body = json!({ "id": 90210, "iid": 7, "web_url": "https://gitlab.com/acme/index/-/merge_requests/7" });
        let request = merge_request_from_body("u", &body, false).expect("well-formed merge request");
        assert_eq!(request.number, 7, "iid is the number a human sees, not the global id");
        assert_eq!(request.html_url, "https://gitlab.com/acme/index/-/merge_requests/7");
        assert!(!request.updated);
    }

    #[test]
    fn mergeability_reads_the_conflict_flag_before_the_status_word() {
        // `has_conflicts` is the direct answer and decides on its own, whatever
        // `detailed_merge_status` says beside it.
        assert_eq!(
            mergeability_from_merge_request(&json!({
                "iid": 7,
                "has_conflicts": true,
                "detailed_merge_status": "conflict",
            })),
            Mergeability::Conflicting
        );
        // The documented conflict spelling stands on its own too: a response
        // carrying only one half of the pair must not read as mergeable.
        assert_eq!(
            mergeability_from_merge_request(&json!({ "iid": 7, "detailed_merge_status": "conflict" })),
            Mergeability::Conflicting
        );
    }

    #[test]
    fn mergeability_treats_an_unfinished_check_as_no_verdict() {
        // GitLab computes mergeability asynchronously. These two values are the
        // only ones that mean "not computed yet"; reading either as a verdict
        // would report a conflict on a request nobody has looked at.
        for status in ["checking", "unchecked"] {
            assert_eq!(
                mergeability_from_merge_request(&json!({ "iid": 7, "detailed_merge_status": status })),
                Mergeability::Unknown,
                "{status} is an unfinished check, not a verdict"
            );
        }
    }

    /// GitLab documents `broken_status` as "can not merge the source into the
    /// target branch, potential conflict". The D2 contract is that an unchanged
    /// run never reports success over a request that cannot merge, so this is
    /// a conflict here — `Unknown` would let it through as a benign `unchanged`.
    #[test]
    fn mergeability_treats_a_broken_status_as_a_conflict() {
        assert_eq!(
            mergeability_from_merge_request(&json!({ "iid": 7, "detailed_merge_status": "broken_status" })),
            Mergeability::Conflicting,
            "a request GitLab says cannot merge must trip the detector"
        );
    }

    #[test]
    fn mergeability_reads_every_other_status_as_mergeable() {
        // `has_conflicts: false` with a settled status is the clean case. So is
        // a blocked-for-some-other-reason request: a failed pipeline is not the
        // base-moved-under-the-branch conflict this detector looks for, and an
        // absent field is the older API shape, which carries no conflict either.
        assert_eq!(
            mergeability_from_merge_request(&json!({
                "iid": 7,
                "has_conflicts": false,
                "detailed_merge_status": "mergeable",
            })),
            Mergeability::Mergeable
        );
        assert_eq!(
            mergeability_from_merge_request(&json!({ "iid": 7, "detailed_merge_status": "ci_still_running" })),
            Mergeability::Mergeable
        );
        assert_eq!(
            mergeability_from_merge_request(&json!({ "iid": 7 })),
            Mergeability::Mergeable
        );
    }

    #[test]
    fn only_throttling_and_server_faults_are_replayed() {
        let status = |status: u16| ForgeError::Status {
            url: "u".to_string(),
            status,
            detail: String::new(),
        };
        assert!(is_retryable(&status(429)));
        assert!(is_retryable(&status(503)));
        // A 400 is the stale-base class, already classified before this point;
        // replaying it would either fail identically or discard a concurrent
        // announce.
        assert!(!is_retryable(&status(400)));
        assert!(!is_retryable(&status(404)));
    }

    // ── harness ──────────────────────────────────────────────────────────

    /// Every environment variable [`ForgeCredentials`]'s ladder reads.
    ///
    /// Copied from `credentials.rs`'s `var::ALL`, which is private to that
    /// file. Its job here is DX-22's: [`ocx_util::env::var`]'s test seam falls
    /// through to the real process environment for any key a test did not
    /// override, so a credentials test that overrides half of this list
    /// measures the machine it ran on rather than the code. `CI_PROJECT_PATH`
    /// is the one that bites hardest — it feeds `publishing_project`, which is
    /// the entire allowlist precondition table below.
    const CI_VARIABLES: [&str; 7] = [
        "OCX_ANNOUNCE_TOKEN",
        "OCX_ANNOUNCE_GIT_TOKEN",
        "OCX_ANNOUNCE_GIT_USERNAME",
        "GITLAB_CI",
        "CI_JOB_TOKEN",
        "CI_PROJECT_PATH",
        "CI_PROJECT_ID",
    ];

    /// The environment lock, with every CI variable explicitly removed.
    ///
    /// Acquired **once** per test: the guard wraps a process-wide, non-reentrant
    /// mutex, so a second acquisition while the first is still alive deadlocks
    /// the whole test binary. A test with two phases resets between them with
    /// [`clear_ci`] instead.
    fn isolated_env() -> ocx_util::env::overrides::EnvLock {
        let env = ocx_util::env::overrides::lock();
        clear_ci(&env);
        env
    }

    /// Remove every CI variable again, resetting a held lock between phases.
    fn clear_ci(env: &ocx_util::env::overrides::EnvLock) {
        for key in CI_VARIABLES {
            env.remove(key);
        }
    }

    /// A resolved `git`, for the git-transport constructor. Never executed —
    /// no test here reaches a subprocess.
    fn git_binary() -> GitBinary {
        GitBinary {
            path: std::path::PathBuf::from("git"),
            version: GitVersion::new(2, 47, 0),
        }
    }

    /// One recorded request against [`FakeGitLab`].
    ///
    /// Headers and the **raw request target** are captured beside the body, a
    /// wider set than the GitHub client's sibling harness keeps: C-026 is a
    /// statement about the exact header set, and both the login lookup and the
    /// allowlist walk carry their arguments in the query string. The body is
    /// read here anyway to drain the socket, so keeping it costs nothing and is
    /// what lets a commit's `actions` array be asserted on.
    #[derive(Clone)]
    struct Recorded {
        method: String,
        target: String,
        headers: BTreeMap<String, String>,
        body: String,
    }

    impl Recorded {
        fn route(&self) -> String {
            format!("{} {}", self.method, self.target)
        }

        /// Every credential-bearing header, in a fixed order.
        ///
        /// The whole set rather than a lookup: an implementation that adds
        /// `JOB-TOKEN` and leaves the `PRIVATE-TOKEN` line in place passes any
        /// presence assertion and sends both.
        fn credential_headers(&self) -> Vec<(&'static str, String)> {
            ["authorization", "private-token", "job-token"]
                .into_iter()
                .filter_map(|name| self.headers.get(name).map(|value| (name, value.clone())))
                .collect()
        }
    }

    /// A one-request-per-connection HTTP/1.1 fake for the GitLab endpoints.
    ///
    /// The real `reqwest` stack is driven end to end rather than a transport
    /// seam: what is under test is header selection, status classification and
    /// the exact set of requests issued, none of which survives a fake above
    /// the HTTP layer. Every response carries `connection: close`, so the
    /// handler sees one request per connection, in order, and can answer the
    /// same URL differently on a later call.
    struct FakeGitLab {
        base_url: String,
        calls: Arc<Mutex<Vec<Recorded>>>,
    }

    /// A branch head shaped the way a real one is: forty hex digits.
    ///
    /// Every fixture that answers a Branches API read uses it. The shape is not
    /// decoration — `branch_sha` refuses anything that is not a full object name,
    /// because the value it returns becomes a positional argument of
    /// `git read-tree` and the right-hand side of a `--force-with-lease`, and an
    /// earlier `"abc"` here would have let every one of those fixtures assert
    /// against a value production now rejects.
    const BRANCH_HEAD: &str = "5f2c8b9d1e4a7063f8b2c5d9e1a4706358b2c5d9";

    impl FakeGitLab {
        /// Bind an ephemeral loopback port and serve `handler`, which maps
        /// (method, raw target) to a (status, JSON body) response.
        async fn start(handler: impl Fn(&str, &str) -> (u16, String) + Send + Sync + 'static) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind loopback");
            let base_url = format!("http://{}", listener.local_addr().expect("local address"));
            let calls = Arc::new(Mutex::new(Vec::new()));
            let recorder = Arc::clone(&calls);
            let handler = Arc::new(handler);
            tokio::spawn(async move {
                while let Ok((mut stream, _)) = listener.accept().await {
                    let handler = Arc::clone(&handler);
                    let recorder = Arc::clone(&recorder);
                    tokio::spawn(async move {
                        if let Some(request) = read_request(&mut stream).await {
                            let (status, body) = handler(&request.method, &request.target);
                            if let Ok(mut calls) = recorder.lock() {
                                calls.push(request);
                            }
                            let _ = write_response(&mut stream, status, &body).await;
                        }
                    });
                }
            });
            Self { base_url, calls }
        }

        fn forge(&self, credentials: ForgeCredentials) -> GitLabForge {
            GitLabForge::with_base_url(credentials, WriteTransport::Api, None, self.base_url.clone())
                .expect("client builds")
        }

        fn git_forge(&self, credentials: ForgeCredentials) -> GitLabForge {
            GitLabForge::with_base_url(
                credentials,
                WriteTransport::Git,
                Some(git_binary()),
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
        let target = request_line.next()?.to_string();
        let mut headers = BTreeMap::new();
        for line in head.lines().skip(1) {
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
        let length: usize = headers
            .get("content-length")
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        let mut body = vec![0_u8; length];
        if length > 0 {
            stream.read_exact(&mut body).await.ok()?;
        }
        Some(Recorded {
            method,
            target,
            headers,
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

    /// The index project every preflight test writes to.
    fn index_repo() -> RepoCoordinate {
        coordinate("gitlab.com/acme/index")
    }

    /// A project document carrying `permissions` good enough to push.
    fn pushable_project() -> Value {
        json!({
            "id": 42,
            "path_with_namespace": "acme/index",
            "permissions": { "project_access": { "access_level": ACCESS_LEVEL_DEVELOPER }, "group_access": null },
        })
    }

    /// An empty **groups** allowlist, for a handler whose own canned body
    /// answers the projects list.
    ///
    /// The preflight asks two endpoints (#430), and a projects-shaped body
    /// served for the groups list carries no `web_url` — which the walk reads as
    /// a *shape* it does not understand, not as an empty list. That turns a miss
    /// into `unknown` and makes the 86 unreachable, so a handler that answers
    /// both endpoints with one body silently disarms its own assertion. Call
    /// this first in any handler whose projects body must be a miss.
    fn empty_groups_allowlist(target: &str) -> Option<(u16, String)> {
        target
            .contains(GROUPS_ALLOWLIST_ENDPOINT)
            .then(|| (200, Value::Array(Vec::new()).to_string()))
    }

    /// One `(label, response body, expected identity)` row of the identity
    /// tables: `None` means the read must refuse rather than synthesise.
    type IdentityCase = (&'static str, Value, Option<(&'static str, u64, bool)>);

    /// The job token every CI fixture below runs under.
    const JOB_TOKEN: &str = "glcbt-64-notarealjobtoken";

    /// The three credential shapes the job-token preflight rows discriminate
    /// between. Each is a real C-063 ladder outcome, resolved through the
    /// ladder rather than hand-built, so a cell cannot assert a pair the
    /// constructor could never produce.
    #[derive(Clone, Copy)]
    enum CredentialShape {
        /// A GitLab job with no ocx variable set: the job token carries both
        /// halves (S-014).
        BothHalves,
        /// S-028: `OCX_ANNOUNCE_GIT_TOKEN` replaces the push half while the API
        /// half stays the job token. `api_is_job_token` is true here and
        /// `push_is_job_token` is false, which is the whole reason the second
        /// predicate exists.
        ApiHalfOnly,
        /// An ordinary API token with the job token exported as the push
        /// override, resolved for the **api** transport — a job-token push
        /// credential on a run that will never push.
        PushHalfUnderApi,
        /// The documented split pair: an ordinary API token for the reads,
        /// `OCX_ANNOUNCE_GIT_TOKEN` carrying the job token for the push, over
        /// the **git** transport. `push_is_job_token` is true and
        /// `api_is_job_token` is false, so the allowlist walk happens — and
        /// happens under `PRIVATE-TOKEN`, which is the posture where a 401 is a
        /// rejected credential rather than a closed door.
        PushHalfOnly,
    }

    fn credentials_for(env: &ocx_util::env::overrides::EnvLock, shape: CredentialShape) -> ForgeCredentials {
        env.set("GITLAB_CI", "true");
        env.set("CI_JOB_TOKEN", JOB_TOKEN);
        match shape {
            CredentialShape::BothHalves => ForgeCredentials::resolve(WriteTransport::Git),
            CredentialShape::ApiHalfOnly => {
                env.set("OCX_ANNOUNCE_GIT_TOKEN", "glpat-a-separate-push-token");
                ForgeCredentials::resolve(WriteTransport::Git)
            }
            CredentialShape::PushHalfUnderApi => {
                env.set("OCX_ANNOUNCE_TOKEN", "glpat-notarealpat");
                env.set("OCX_ANNOUNCE_GIT_TOKEN", JOB_TOKEN);
                ForgeCredentials::resolve(WriteTransport::Api)
            }
            CredentialShape::PushHalfOnly => {
                env.set("OCX_ANNOUNCE_TOKEN", "glpat-notarealpat");
                env.set("OCX_ANNOUNCE_GIT_TOKEN", JOB_TOKEN);
                ForgeCredentials::resolve(WriteTransport::Git)
            }
        }
    }

    /// The credentials a GitLab job resolves with no ocx variable set.
    fn job_token_credentials(env: &ocx_util::env::overrides::EnvLock) -> ForgeCredentials {
        credentials_for(env, CredentialShape::BothHalves)
    }

    // ── C-026 header selection ───────────────────────────────────────────

    /// A credential that **is** this environment's own `CI_JOB_TOKEN` travels
    /// as `JOB-TOKEN`, and `PRIVATE-TOKEN` is not sent beside it.
    ///
    /// The exact header set is asserted, not a membership: an implementation
    /// that adds the new header and leaves the existing `PRIVATE-TOKEN` line in
    /// place passes a presence assertion while sending both, which GitLab
    /// answers in a way that reads like a permission problem.
    ///
    /// The second half drives the same credential through the **git**-transport
    /// constructor. C-026 says the choice is transport-independent, and the
    /// only test constructor that existed before this package hardcoded
    /// `Api` — so a build that gated the `JOB-TOKEN` arm on the transport would
    /// have passed every header test ever written against it.
    #[tokio::test]
    async fn job_token_selects_job_token_header() {
        let env = isolated_env();
        let fake = FakeGitLab::start(|_, _| {
            (
                200,
                json!([{ "name": "main", "commit": { "id": BRANCH_HEAD } }]).to_string(),
            )
        })
        .await;
        let credentials = job_token_credentials(&env);
        assert!(credentials.api_is_job_token(), "the fixture must resolve a job token");

        fake.forge(credentials.clone())
            .get_ref_sha(&index_repo(), "heads/main")
            .await
            .expect("the branch read succeeds");
        fake.git_forge(credentials)
            .get_ref_sha(&index_repo(), "heads/main")
            .await
            .expect("the branch read succeeds");

        for (index, request) in fake.recorded().iter().enumerate() {
            assert_eq!(
                request.credential_headers(),
                vec![("job-token", JOB_TOKEN.to_string())],
                "request {index} must carry JOB-TOKEN and nothing else"
            );
        }
        assert_eq!(fake.recorded().len(), 2, "both transports must have issued their read");
    }

    /// Any other credential keeps `PRIVATE-TOKEN`, unchanged from today.
    ///
    /// Four cells, because four different environments all have to land here
    /// and three of them are invisible to a single-cell test:
    ///
    /// - `CI_JOB_TOKEN` **unset**, and `CI_JOB_TOKEN` **set but empty** are
    ///   different states. `ocx_util::env::overrides`'s seam distinguishes `remove`
    ///   from `set(k, "")`, and a build that dropped the non-emptiness filter
    ///   passes the first and fails the second.
    /// - The comparison is **byte-exact**. Trimming is the plausible helpful
    ///   edit, and a trimming build sends `JOB-TOKEN` for a credential that is
    ///   not the job token.
    #[tokio::test]
    async fn non_job_token_keeps_private_token_header() {
        let cases: [(&str, Option<&str>, &str); 4] = [
            ("a different token", Some(JOB_TOKEN), "glpat-notarealpat"),
            ("CI_JOB_TOKEN unset", None, "glpat-notarealpat"),
            ("CI_JOB_TOKEN set but empty", Some(""), "glpat-notarealpat"),
            (
                "a different case is a different token",
                Some("GLPAT-NOTAREALPAT"),
                "glpat-notarealpat",
            ),
        ];
        for (label, job_token, api) in cases {
            let env = isolated_env();
            match job_token {
                Some(value) => env.set("CI_JOB_TOKEN", value),
                None => env.remove("CI_JOB_TOKEN"),
            }
            let credentials = ForgeCredentials::new(ForgeToken::new(api.to_string()));
            assert!(
                !credentials.api_is_job_token(),
                "{label}: the fixture must not be a job token"
            );

            let fake = FakeGitLab::start(|_, _| {
                (
                    200,
                    json!([{ "name": "main", "commit": { "id": BRANCH_HEAD } }]).to_string(),
                )
            })
            .await;
            fake.forge(credentials)
                .get_ref_sha(&index_repo(), "heads/main")
                .await
                .expect("the branch read succeeds");

            assert_eq!(
                fake.recorded().first().expect("one request").credential_headers(),
                vec![("private-token", api.to_string())],
                "{label}: PRIVATE-TOKEN alone must carry the credential"
            );
        }

        // The trailing-whitespace cell cannot travel as a header — `\n` is not a
        // legal header value — so it is asserted on the predicate the header arm
        // reads. Trimming either side is the plausible helpful edit, and a
        // trimming build sends `JOB-TOKEN` for a credential that is not the job
        // token, which GitLab rejects in a way that reads as a permission
        // problem.
        let env = isolated_env();
        env.set("CI_JOB_TOKEN", "glpat-notarealpat");
        assert!(
            !ForgeCredentials::new(ForgeToken::new("glpat-notarealpat\n".to_string())).api_is_job_token(),
            "the comparison is byte-exact, not trimmed"
        );
    }

    /// An empty credential sends **no** authorization header at all, so the
    /// request reads as unauthenticated rather than as a rejected empty
    /// credential. This is the `--out` path (S-011).
    #[tokio::test]
    async fn empty_credential_sends_no_header() {
        let env = isolated_env();
        env.set("CI_JOB_TOKEN", "");
        let fake = FakeGitLab::start(|_, _| {
            (
                200,
                json!([{ "name": "main", "commit": { "id": BRANCH_HEAD } }]).to_string(),
            )
        })
        .await;

        fake.forge(ForgeCredentials::new(ForgeToken::new(String::new())))
            .get_ref_sha(&index_repo(), "heads/main")
            .await
            .expect("the branch read succeeds");

        assert_eq!(
            fake.recorded().first().expect("one request").credential_headers(),
            Vec::new(),
            "an empty credential must send no authorization header"
        );
    }

    // ── C-027 authenticated_identity ─────────────────────────────────────

    /// A CI job token may not call `GET /user`, and both statuses GitLab can
    /// answer with are needles.
    ///
    /// GitLab's own documentation says nothing about which status a job token
    /// earns on the users API, so handling one and not the other would ship a
    /// coin flip. The third cell is the one that keeps the refusal honest: a
    /// pre-request `if api_is_job_token { return Err(...) }` passes both status
    /// cells and never reads an identity — so the 200 cell asserts the request
    /// was **issued** and the identity came back.
    #[tokio::test]
    async fn gitlab_job_token_users_api_is_unavailable() {
        for status in [401_u16, 403] {
            let env = isolated_env();
            let credentials = job_token_credentials(&env);
            let fake = FakeGitLab::start(move |_, _| (status, json!({ "message": "403 Forbidden" }).to_string())).await;

            let error = fake
                .forge(credentials)
                .authenticated_identity()
                .await
                .expect_err("a job token cannot read the users API");
            assert!(
                matches!(error, ForgeError::UsersApiUnavailable),
                "status {status} under a job token must be UsersApiUnavailable, got {error:?}"
            );
        }

        let env = isolated_env();
        let credentials = job_token_credentials(&env);
        let fake =
            FakeGitLab::start(|_, _| (200, json!({ "username": "alice", "id": 7, "bot": false }).to_string())).await;
        let identity = fake
            .forge(credentials)
            .authenticated_identity()
            .await
            .expect("a job token that answers 200 has an identity")
            .expect("GitLab always has a user behind a credential");
        assert_eq!(identity.login, "alice");
        assert_eq!(
            fake.routes(),
            vec!["GET /user".to_string()],
            "the refusal must not pre-empt the request — a job token that CAN read the endpoint is read"
        );
    }

    /// A 401 or 403 on a credential that is **not** the job token is an
    /// ordinary status error, and it must stay one.
    ///
    /// This is the highest-value cell in C-027 and the plan named no test for
    /// it. `UsersApiUnavailable` classifies to `UsageError` (64) and tells the
    /// operator to rewrite their owner as `LOGIN:ID`; a revoked personal access
    /// token needs `AuthError` (80) and a new credential. A status-only gate
    /// gives the first answer to the second problem.
    #[tokio::test]
    async fn a_rejected_non_job_token_is_a_status_error_not_an_unavailable_users_api() {
        for status in [401_u16, 403] {
            let env = isolated_env();
            env.set("CI_JOB_TOKEN", JOB_TOKEN);
            let credentials = ForgeCredentials::new(ForgeToken::new("glpat-revoked".to_string()));
            let fake =
                FakeGitLab::start(move |_, _| (status, json!({ "message": "401 Unauthorized" }).to_string())).await;

            let error = fake
                .forge(credentials)
                .authenticated_identity()
                .await
                .expect_err("a revoked credential fails");
            assert!(
                matches!(error, ForgeError::Status { status: got, .. } if got == status),
                "status {status} on a non-job-token credential must stay a status error, got {error:?}"
            );
        }
    }

    /// The identity is read out of the response body, and every field it needs
    /// is either present or an error — never synthesised.
    ///
    /// `bot` is the exception, and deliberately: GitLab's list representation
    /// may omit it on an older self-managed instance, so an absent `bot` is
    /// `false`. `id` is the opposite case — a synthesised `0` would be written
    /// into a published index root as the owner's forge id.
    #[tokio::test]
    async fn identity_defaults_bot_but_never_synthesises_a_login_or_an_id() {
        let cases: [IdentityCase; 6] = [
            (
                "the full shape",
                json!({ "username": "alice", "id": 7, "bot": true }),
                Some(("alice", 7, true)),
            ),
            (
                "bot absent defaults to false",
                json!({ "username": "alice", "id": 7 }),
                Some(("alice", 7, false)),
            ),
            (
                "bot null defaults to false",
                json!({ "username": "alice", "id": 7, "bot": null }),
                Some(("alice", 7, false)),
            ),
            ("id absent", json!({ "username": "alice" }), None),
            ("id as a string", json!({ "username": "alice", "id": "7" }), None),
            ("username absent", json!({ "id": 7 }), None),
        ];
        for (label, body, expected) in cases {
            let _env = isolated_env();
            let credentials = ForgeCredentials::new(ForgeToken::new("glpat-notarealpat".to_string()));
            let rendered = body.to_string();
            let fake = FakeGitLab::start(move |_, _| (200, rendered.clone())).await;

            let outcome = fake.forge(credentials).authenticated_identity().await;
            match expected {
                Some((login, id, bot)) => {
                    let identity = outcome
                        .unwrap_or_else(|error| panic!("{label}: expected an identity, got {error:?}"))
                        .unwrap_or_else(|| panic!("{label}: expected an identity, got Ok(None)"));
                    assert_eq!(
                        (identity.login.as_str(), identity.id, identity.bot),
                        (login, id, bot),
                        "{label}"
                    );
                }
                None => {
                    let error = outcome
                        .err()
                        .unwrap_or_else(|| panic!("{label}: expected a missing field"));
                    assert!(
                        matches!(error, ForgeError::MissingField { .. }),
                        "{label}: an unreadable field must be MissingField, never a synthesised value; got {error:?}"
                    );
                }
            }
        }
    }

    /// An empty API credential cannot call `GET /user` at all, so no request is
    /// issued and the answer is `UsersApiUnavailable`.
    ///
    /// C-027 gives no rule here and the gap is load-bearing: with no header the
    /// endpoint answers 401, and the ladder above would then classify that as
    /// `Status { 401 }` → exit 80, contradicting S-011's "`--out` with no
    /// credential proceeds unauthenticated". `UsersApiUnavailable` is exactly
    /// "the credential may not call the endpoint at all", and it costs no
    /// network.
    ///
    /// The second half is the harness's own positive control: an empty route
    /// list and a recorder that never fires are indistinguishable, so the same
    /// fake is made to record a request before the absence is trusted.
    #[tokio::test]
    async fn an_empty_credential_reads_no_identity_and_issues_no_request() {
        let _env = isolated_env();
        let fake = FakeGitLab::start(|_, _| (200, json!({ "username": "alice", "id": 7 }).to_string())).await;

        let error = fake
            .forge(ForgeCredentials::new(ForgeToken::new(String::new())))
            .authenticated_identity()
            .await
            .expect_err("an unauthenticated identity read has no answer");
        assert!(
            matches!(error, ForgeError::UsersApiUnavailable),
            "an empty credential must not reach the endpoint; got {error:?}"
        );
        assert_eq!(fake.routes(), Vec::<String>::new(), "no request may be issued");

        // The control: the same fake, an ordinary read, one recorded route.
        fake.forge(ForgeCredentials::new(ForgeToken::new("glpat-notarealpat".to_string())))
            .authenticated_identity()
            .await
            .expect("the control read succeeds");
        assert_eq!(
            fake.routes(),
            vec!["GET /user".to_string()],
            "the recorder must fire when a request IS issued, or the assertion above is vacuous"
        );
    }

    // ── C-028 resolve_user ───────────────────────────────────────────────

    /// The forge's own `bot` field is what sets [`ForgeIdentity::bot`], an
    /// empty list is `Ok(None)`, and an entry that omits `bot` is not a bot.
    ///
    /// The absent-`bot` cell can only be a unit cell: the acceptance fake emits
    /// `bot` unconditionally, while GitLab's documented regular-user "list
    /// users" representation is `id, username, name, state, locked, avatar_url,
    /// web_url` — `bot` sits in the fuller one. An `.ok_or(MissingField)` build
    /// therefore passes the whole acceptance suite and fails on a real
    /// self-managed instance.
    #[tokio::test]
    async fn gitlab_resolve_user_reads_bot_flag() {
        let cases: [IdentityCase; 4] = [
            (
                "an ordinary account",
                json!([{ "username": "alice", "id": 7, "bot": false }]),
                Some(("alice", 7, false)),
            ),
            (
                "a bot the forge names",
                json!([{ "username": "dependabot", "id": 9, "bot": true }]),
                Some(("dependabot", 9, true)),
            ),
            (
                "bot omitted by an older instance",
                json!([{ "username": "alice", "id": 7 }]),
                Some(("alice", 7, false)),
            ),
            ("no such account", json!([]), None),
        ];
        for (label, body, expected) in cases {
            let _env = isolated_env();
            let credentials = ForgeCredentials::new(ForgeToken::new("glpat-notarealpat".to_string()));
            let login = expected.map_or("alice", |(login, _, _)| login);
            let rendered = body.to_string();
            let fake = FakeGitLab::start(move |_, _| (200, rendered.clone())).await;

            let identity = fake
                .forge(credentials)
                .resolve_user(login)
                .await
                .unwrap_or_else(|error| panic!("{label}: {error:?}"));
            match expected {
                Some((login, id, bot)) => {
                    let identity = identity.unwrap_or_else(|| panic!("{label}: expected an identity"));
                    assert_eq!(
                        (identity.login.as_str(), identity.id, identity.bot),
                        (login, id, bot),
                        "{label}"
                    );
                }
                None => assert!(identity.is_none(), "{label}: an empty list is Ok(None)"),
            }
        }
    }

    /// More than one entry is a server contract violation, and the entry that
    /// matches the requested login is the one selected — never `[0]`.
    ///
    /// GitLab documents `?username=` as an exact, case-insensitive match, so a
    /// second entry cannot legitimately arrive. Taking the first is the
    /// arbitrary-selection unfalsifiable green, and its consequence is not
    /// cosmetic: C-048 writes "the server's canonical login spelling" into a
    /// published index root, so `[0]` publishes the wrong owner.
    ///
    /// A list that matches nothing is `Ok(None)`, not a guess.
    #[tokio::test]
    async fn resolve_user_selects_the_entry_whose_login_matches() {
        let _env = isolated_env();
        let credentials = ForgeCredentials::new(ForgeToken::new("glpat-notarealpat".to_string()));
        let fake = FakeGitLab::start(|_, _| {
            (
                200,
                json!([
                    { "username": "alicia", "id": 3, "bot": false },
                    { "username": "Alice", "id": 7, "bot": false },
                ])
                .to_string(),
            )
        })
        .await;

        let identity = fake
            .forge(credentials.clone())
            .resolve_user("alice")
            .await
            .expect("the lookup succeeds")
            .expect("one entry matches");
        assert_eq!(
            (identity.login.as_str(), identity.id),
            ("Alice", 7),
            "the matching entry is selected case-insensitively, and its canonical spelling is kept"
        );

        let fake = FakeGitLab::start(|_, _| (200, json!([{ "username": "alicia", "id": 3 }]).to_string())).await;
        assert!(
            fake.forge(credentials)
                .resolve_user("alice")
                .await
                .expect("the lookup succeeds")
                .is_none(),
            "a list that matches nothing is Ok(None), never the first entry"
        );
    }

    /// A body that is not a list is a decode failure, not "no such account".
    ///
    /// `as_array().and_then(first)` on an object yields `None`, which becomes
    /// `Ok(None)` — "the forge does not know this login", exit 79 with a wrong
    /// diagnosis on an instance behind a rewriting gateway.
    #[tokio::test]
    async fn resolve_user_refuses_a_body_that_is_not_a_list() {
        let _env = isolated_env();
        let credentials = ForgeCredentials::new(ForgeToken::new("glpat-notarealpat".to_string()));
        let fake = FakeGitLab::start(|_, _| (200, json!({ "username": "alice", "id": 7 }).to_string())).await;

        let error = fake
            .forge(credentials)
            .resolve_user("alice")
            .await
            .expect_err("an object body is not the documented shape");
        assert!(
            matches!(error, ForgeError::Decode { .. }),
            "a non-list body must be a decode failure, not an absent account; got {error:?}"
        );
    }

    /// The login is percent-encoded into the query, and an empty login issues
    /// no request at all.
    ///
    /// Both halves guard the same hazard from opposite sides. Raw
    /// interpolation lets `--owner "alice&private_token=x"` append a query
    /// parameter to an authenticated request; an unfiltered empty login makes
    /// GitLab list *every* user, and the selection above would then pick
    /// whichever account happens to spell the empty string — which is none, but
    /// only by luck of the comparison.
    ///
    /// The recorded **target** is asserted, not the outcome: the encoder is
    /// invisible to a result assertion.
    #[tokio::test]
    async fn resolve_user_encodes_the_login_and_never_looks_up_an_empty_one() {
        let _env = isolated_env();
        let credentials = ForgeCredentials::new(ForgeToken::new("glpat-notarealpat".to_string()));
        let fake = FakeGitLab::start(|_, _| (200, "[]".to_string())).await;

        assert!(
            fake.forge(credentials.clone())
                .resolve_user("")
                .await
                .expect("an empty login is simply no account")
                .is_none(),
            "an empty login has no account to find"
        );
        assert_eq!(
            fake.routes(),
            Vec::<String>::new(),
            "an empty login must issue no request"
        );

        // The control, and the encoding assertion in one: the recorder fires
        // for a real lookup, and what it recorded carries no raw `&` or `=`.
        fake.forge(credentials)
            .resolve_user("alice&private_token=x")
            .await
            .expect("the lookup succeeds");
        let target = fake.recorded().first().expect("one request").target.clone();
        assert!(
            target.starts_with("/users?username="),
            "the login travels in the query string: {target}"
        );
        assert!(
            !target.contains("alice&") && !target.contains("private_token="),
            "the login must be percent-encoded, or it injects a query parameter: {target}"
        );
    }

    /// `resolve_user` shares C-027's ladder: a job token cannot read the users
    /// API, and any other rejected credential is an ordinary status error.
    #[tokio::test]
    async fn resolve_user_discriminates_a_job_token_from_a_rejected_credential() {
        for status in [401_u16, 403] {
            let env = isolated_env();
            let credentials = job_token_credentials(&env);
            let fake = FakeGitLab::start(move |_, _| (status, "{}".to_string())).await;
            let error = fake
                .forge(credentials)
                .resolve_user("alice")
                .await
                .expect_err("a job token cannot read the users API");
            assert!(
                matches!(error, ForgeError::UsersApiUnavailable),
                "status {status} under a job token must be UsersApiUnavailable; got {error:?}"
            );

            clear_ci(&env);
            env.set("CI_JOB_TOKEN", JOB_TOKEN);
            let credentials = ForgeCredentials::new(ForgeToken::new("glpat-revoked".to_string()));
            let fake = FakeGitLab::start(move |_, _| (status, "{}".to_string())).await;
            let error = fake
                .forge(credentials)
                .resolve_user("alice")
                .await
                .expect_err("a revoked credential fails");
            assert!(
                matches!(error, ForgeError::Status { status: got, .. } if got == status),
                "status {status} on a non-job-token credential must stay a status error; got {error:?}"
            );
        }
    }

    // ── C-029 the preflight ──────────────────────────────────────────────

    /// The access level is the **higher** of the project grant and the group
    /// grant, and anything unreadable is zero.
    ///
    /// A pure table because the acceptance fake hardcodes `group_access: null`,
    /// so deleting the `group_access` term keeps the whole acceptance suite
    /// green while breaking every group-level member — the commonest GitLab
    /// shape there is. `permissions` absent is the documented unauthenticated
    /// read; a string or float `access_level` is unreadable, and refusing is
    /// the safe direction here because the denial is a verdict rather than an
    /// unreadable capability.
    #[test]
    fn project_access_level_takes_the_higher_grant_and_reads_nothing_else() {
        let cases: [(&str, Value, u64); 7] = [
            ("no permissions at all", json!({ "id": 1 }), 0),
            (
                "the project grant",
                json!({ "permissions": { "project_access": { "access_level": 30 }, "group_access": null } }),
                30,
            ),
            (
                "the group grant, with no project membership",
                json!({ "permissions": { "project_access": null, "group_access": { "access_level": 40 } } }),
                40,
            ),
            (
                "the higher of the two",
                json!({ "permissions": { "project_access": { "access_level": 20 }, "group_access": { "access_level": 30 } } }),
                30,
            ),
            (
                "neither grant",
                json!({ "permissions": { "project_access": null, "group_access": null } }),
                0,
            ),
            (
                "a stringly level is unreadable",
                json!({ "permissions": { "project_access": { "access_level": "30" } } }),
                0,
            ),
            (
                "a fractional level is unreadable",
                json!({ "permissions": { "project_access": { "access_level": 30.5 } } }),
                0,
            ),
        ];
        for (label, body, expected) in cases {
            assert_eq!(project_access_level(&body), expected, "{label}");
        }
    }

    /// Developer (30) is the boundary, and it is **inclusive**.
    ///
    /// Both directions, because "refuses everything" and "accepts everything"
    /// each pass half of a single-cell test. 29 is refused, 30 and 31 are
    /// accepted; C-075 already records this project shipping the mirror image
    /// of this off-by-one on a version gate.
    #[tokio::test]
    async fn developer_is_the_inclusive_boundary_that_may_push() {
        for (level, may_push) in [(29_u64, false), (30, true), (31, true)] {
            let _env = isolated_env();
            let credentials = ForgeCredentials::new(ForgeToken::new("glpat-notarealpat".to_string()));
            let fake = FakeGitLab::start(move |_, _| {
                (
                    200,
                    json!({
                        "id": 42,
                        "permissions": { "project_access": { "access_level": level }, "group_access": null },
                    })
                    .to_string(),
                )
            })
            .await;

            let outcome = fake.forge(credentials).ensure_push_access(&index_repo()).await;
            if may_push {
                let access = outcome.unwrap_or_else(|error| panic!("access level {level} may push: {error:?}"));
                assert_eq!(
                    access.status(CapabilityName::PushAccess),
                    CheckStatus::Passed,
                    "access level {level} passes the push probe"
                );
            } else {
                let error = outcome
                    .err()
                    .unwrap_or_else(|| panic!("access level {level} may not push"));
                assert!(
                    matches!(error, ForgeError::PushAccessDenied { .. }),
                    "access level {level} must be refused; got {error:?}"
                );
            }
        }
    }

    /// The preflight emits the **whole** four-row array, in
    /// [`CapabilityName::ALL`] order, with the status each row earned.
    ///
    /// The whole array, not a membership: `skipped_all` seeds four rows and
    /// `record` replaces in place, so the array's length never changes and only
    /// the statuses discriminate. A "a push-access row exists" assertion is
    /// green for an implementation that calls `skipped_all()` and returns.
    ///
    /// Two runs, because the two job-token rows and the `git-version` row are
    /// exactly what the transport decides.
    #[tokio::test]
    async fn gitlab_ensure_push_access_emits_rows() {
        let env = isolated_env();
        let credentials = ForgeCredentials::new(ForgeToken::new("glpat-notarealpat".to_string()));
        let fake = FakeGitLab::start(|_, _| (200, pushable_project().to_string())).await;

        let access = fake
            .forge(credentials)
            .ensure_push_access(&index_repo())
            .await
            .expect("the credential may push");
        assert_eq!(
            access
                .checks()
                .iter()
                .map(|check| (check.name.to_string(), check.status.to_string(), check.detail.clone()))
                .collect::<Vec<_>>(),
            vec![
                ("git-version".to_string(), "skipped".to_string(), None),
                (
                    "push-access".to_string(),
                    "passed".to_string(),
                    Some("access level 30".to_string())
                ),
                ("job-token-push".to_string(), "skipped".to_string(), None),
                ("job-token-allowlist".to_string(), "skipped".to_string(), None),
            ],
            "an api-transport run reports one probe and three inapplicable rows"
        );

        // Under the git transport the resolved binary answers `git-version`,
        // and a job-token push consults the project's own setting.
        clear_ci(&env);
        env.set("CI_PROJECT_PATH", "acme/index");
        let credentials = job_token_credentials(&env);
        let fake = FakeGitLab::start(|_, _| {
            let mut body = pushable_project();
            body["ci_push_repository_for_job_token_allowed"] = json!(true);
            (200, body.to_string())
        })
        .await;

        let access = fake
            .git_forge(credentials)
            .ensure_push_access(&index_repo())
            .await
            .expect("the credential may push");
        assert_eq!(
            access
                .checks()
                .iter()
                .map(|check| (check.name.to_string(), check.status.to_string()))
                .collect::<Vec<_>>(),
            vec![
                ("git-version".to_string(), "passed".to_string()),
                ("push-access".to_string(), "passed".to_string()),
                ("job-token-push".to_string(), "passed".to_string()),
                // Same project as the index: there is nothing to check against
                // an allowlist, so the row does not apply.
                ("job-token-allowlist".to_string(), "skipped".to_string()),
            ],
            "a git-transport job-token run reports the version, the probe and the job-token setting"
        );
        assert_eq!(
            access
                .checks()
                .iter()
                .find(|check| check.name == CapabilityName::GitVersion)
                .and_then(|check| check.detail.clone()),
            Some("git 2.47.0".to_string()),
            "the git-version row carries the parsed version, which is inside C-011's closed set"
        );
    }

    /// A field that cannot be read is `unknown`, and the run proceeds.
    ///
    /// Six response states collapse onto three answers through
    /// `Value::as_bool`, and the distinction is structural rather than
    /// defensive: `true` passes, `false` refuses, and **absent, `null` and
    /// wrong-typed all mean the same thing** — nothing was read. The defect
    /// this forbids is a typed `#[serde(default)] bool`, which turns all three
    /// into `false` and refuses every pre-18.4 instance at 86 before it pushes.
    #[tokio::test]
    async fn preflight_unknown_field_does_not_fail() {
        let cases: [(&str, Option<Value>, CheckStatus, Option<&str>); 5] = [
            ("enabled", Some(json!(true)), CheckStatus::Passed, None),
            (
                "the key is absent",
                None,
                CheckStatus::Unknown,
                Some("ci_push_repository_for_job_token_allowed"),
            ),
            (
                "the key is null",
                Some(json!(null)),
                CheckStatus::Unknown,
                Some("ci_push_repository_for_job_token_allowed"),
            ),
            (
                "the value is a string",
                Some(json!("false")),
                CheckStatus::Unknown,
                Some("ci_push_repository_for_job_token_allowed"),
            ),
            (
                "the value is a number",
                Some(json!(0)),
                CheckStatus::Unknown,
                Some("ci_push_repository_for_job_token_allowed"),
            ),
        ];
        for (label, field, expected, detail) in cases {
            let env = isolated_env();
            env.set("CI_PROJECT_PATH", "acme/index");
            let credentials = job_token_credentials(&env);
            let fake = FakeGitLab::start(move |_, _| {
                let mut body = pushable_project();
                if let Some(value) = field.clone() {
                    body["ci_push_repository_for_job_token_allowed"] = value;
                }
                (200, body.to_string())
            })
            .await;

            let access = fake
                .git_forge(credentials)
                .ensure_push_access(&index_repo())
                .await
                .unwrap_or_else(|error| panic!("{label}: an unreadable field never fails the call: {error:?}"));
            assert_eq!(access.status(CapabilityName::JobTokenPush), expected, "{label}");
            assert_eq!(
                access
                    .checks()
                    .iter()
                    .find(|check| check.name == CapabilityName::JobTokenPush)
                    .and_then(|check| check.detail.as_deref()),
                detail,
                "{label}: an unreadable row names the field it could not read"
            );
        }
    }

    /// A field that reads **`false`** refuses the run at 86, before any push,
    /// naming the setting to change and the project to change it on.
    ///
    /// The rendered `Display` is asserted, not the struct fields: the variant
    /// renders `"{capability} is unavailable on {repo}: {remedy}"`, so a build
    /// that left `remedy` empty would satisfy a field-shaped assertion and ship
    /// a message that tells the operator nothing.
    #[tokio::test]
    async fn preflight_readable_false_errs_86() {
        let env = isolated_env();
        env.set("CI_PROJECT_PATH", "acme/index");
        let credentials = job_token_credentials(&env);
        let fake = FakeGitLab::start(|_, _| {
            let mut body = pushable_project();
            body["ci_push_repository_for_job_token_allowed"] = json!(false);
            (200, body.to_string())
        })
        .await;

        let error = fake
            .git_forge(credentials)
            .ensure_push_access(&index_repo())
            .await
            .expect_err("a disabled job-token push refuses before the push");
        assert!(
            matches!(
                error,
                ForgeError::WriteCapabilityUnavailable {
                    capability: CapabilityName::JobTokenPush,
                    ..
                }
            ),
            "the refusal names the job-token-push capability; got {error:?}"
        );
        let rendered = error.to_string();
        assert!(
            rendered.contains("Settings → CI/CD → Job token permissions"),
            "the message must name the setting an administrator changes: {rendered}"
        );
        assert!(
            rendered.contains("acme/index"),
            "the message must name the project the setting lives on: {rendered}"
        );
    }

    /// The two job-token rows apply **only** when the run will actually push
    /// with a job token: the git transport, and a push credential that is one.
    ///
    /// C-029 as written is unconditional, and unconditional it is a
    /// release-blocking regression. `ensure_push_access` sits on the ordinary
    /// announce path today, under `--transport api`, for every fork-free run,
    /// and `ci_push_repository_for_job_token_allowed: false` is a plausible
    /// project default with no bearing whatever on an API commit — so every
    /// `ocx package announce` against such a project would exit 86.
    ///
    /// Three cells, and each kills a different half of the conjunction. The
    /// `api` cell reds if the transport conjunct is dropped. The S-028 cell —
    /// `OCX_ANNOUNCE_GIT_TOKEN` replacing the push half while the API half
    /// stays the job token — reds if `api_is_job_token` is substituted for
    /// `push_is_job_token`. The third proves the refusal is still reachable, so
    /// the first two are not green because nothing ever fires.
    #[tokio::test]
    async fn job_token_rows_apply_only_to_a_job_token_push_over_git() {
        let cases: [(&str, CredentialShape, WriteTransport, bool); 3] = [
            (
                "the api transport, with a job-token push credential",
                CredentialShape::PushHalfUnderApi,
                WriteTransport::Api,
                false,
            ),
            (
                "the git transport, with the push half replaced (S-028)",
                CredentialShape::ApiHalfOnly,
                WriteTransport::Git,
                false,
            ),
            (
                "the git transport, pushing with the job token",
                CredentialShape::BothHalves,
                WriteTransport::Git,
                true,
            ),
        ];
        for (label, shape, transport, refuses) in cases {
            let env = isolated_env();
            env.set("CI_PROJECT_PATH", "acme/index");
            let credentials = credentials_for(&env, shape);
            assert_eq!(
                credentials.push_is_job_token() && transport == WriteTransport::Git,
                refuses,
                "{label}: the fixture must produce the run shape the cell is about"
            );

            let fake = FakeGitLab::start(|_, _| {
                let mut body = pushable_project();
                body["ci_push_repository_for_job_token_allowed"] = json!(false);
                (200, body.to_string())
            })
            .await;
            let forge = match transport {
                WriteTransport::Git => fake.git_forge(credentials),
                WriteTransport::Api => fake.forge(credentials),
            };

            let outcome = forge.ensure_push_access(&index_repo()).await;
            if refuses {
                let error = outcome
                    .err()
                    .unwrap_or_else(|| panic!("{label}: a job-token push must be refused"));
                assert!(
                    matches!(error, ForgeError::WriteCapabilityUnavailable { .. }),
                    "{label}: got {error:?}"
                );
            } else {
                let access = outcome.unwrap_or_else(|error| {
                    panic!("{label}: a run that will not push with a job token must not consult the setting: {error:?}")
                });
                assert_eq!(
                    access.status(CapabilityName::JobTokenPush),
                    CheckStatus::Skipped,
                    "{label}: the row does not apply"
                );
                assert_eq!(
                    access.status(CapabilityName::JobTokenAllowlist),
                    CheckStatus::Skipped,
                    "{label}: nor does the allowlist"
                );
            }
        }
    }

    /// The index project's allowlist is read **only** when the push credential
    /// is a job token and the publishing project differs from it.
    ///
    /// Four cells, and the three that must *not* read matter as much as the one
    /// that must. Asserting only "not read when same-project" passes a build
    /// that never reads it at all; asserting only "read when cross-project"
    /// passes an unconditional read, which costs an extra request on every
    /// announce and makes the reported request count a lie (DV-6).
    ///
    /// The comparison is case-insensitive: `CI_PROJECT_PATH` is operator-typed,
    /// and a case mismatch would produce a spurious cross-project read and,
    /// behind it, a spurious 86. `ensure_fork` already compares namespaces this
    /// way.
    #[tokio::test]
    async fn allowlist_read_only_when_cross_project() {
        const ALLOWLIST: &str = "GET /projects/acme%2Findex/job_token_scope/allowlist?per_page=100&page=1";
        let cases: [(&str, &str, CredentialShape, WriteTransport, bool); 4] = [
            (
                "cross-project, job-token push over git",
                "acme/widget",
                CredentialShape::BothHalves,
                WriteTransport::Git,
                true,
            ),
            (
                "the index project itself, spelled differently",
                "Acme/Index",
                CredentialShape::BothHalves,
                WriteTransport::Git,
                false,
            ),
            (
                "cross-project, but the push half is not a job token",
                "acme/widget",
                CredentialShape::ApiHalfOnly,
                WriteTransport::Git,
                false,
            ),
            (
                "cross-project job-token push, but the api transport",
                "acme/widget",
                CredentialShape::PushHalfUnderApi,
                WriteTransport::Api,
                false,
            ),
        ];
        for (label, publishing, shape, transport, reads) in cases {
            let env = isolated_env();
            // Before the ladder runs: `publishing_project` is derived in the
            // constructor, not read at use.
            env.set("CI_PROJECT_PATH", publishing);
            let credentials = credentials_for(&env, shape);
            assert_eq!(
                credentials.publishing_project(),
                Some(publishing),
                "{label}: the publishing project must reach the credentials"
            );

            let fake = FakeGitLab::start(|_, target| {
                if target.contains("job_token_scope") {
                    return (200, json!([{ "path_with_namespace": "acme/widget" }]).to_string());
                }
                let mut body = pushable_project();
                body["ci_push_repository_for_job_token_allowed"] = json!(true);
                (200, body.to_string())
            })
            .await;
            let forge = match transport {
                WriteTransport::Git => fake.git_forge(credentials),
                WriteTransport::Api => fake.forge(credentials),
            };
            forge.ensure_push_access(&index_repo()).await.expect("the run proceeds");

            assert_eq!(
                fake.routes().contains(&ALLOWLIST.to_string()),
                reads,
                "{label}: routes were {:?}",
                fake.routes()
            );
        }
    }

    /// A publishing project absent from the index project's allowlist refuses
    /// at 86, naming **both** project paths.
    ///
    /// **Fixture-proved only.** `GET /projects/:id/job_token_scope/allowlist`
    /// requires Maintainer or Owner on the *index* project, while this
    /// preflight's own bar is Developer — so the publisher this check exists
    /// for, someone opening a claim merge request against a third-party index,
    /// has no role there and reads `unknown` instead. The 86-miss path is
    /// therefore near-unreachable in production, and release gate 4 must
    /// observe the real status against gitlab.com. Annotated the way C-044
    /// annotates its fixture-written phrases rather than dropped: the code path
    /// exists and a Maintainer publishing from their own group does reach it.
    #[tokio::test]
    async fn preflight_allowlist_miss_errs_86() {
        let env = isolated_env();
        env.set("CI_PROJECT_PATH", "acme/widget");
        let credentials = job_token_credentials(&env);
        let fake = FakeGitLab::start(|_, target| {
            if let Some(answer) = empty_groups_allowlist(target) {
                return answer;
            }
            if target.contains("job_token_scope") {
                return (200, json!([{ "path_with_namespace": "acme/other" }]).to_string());
            }
            let mut body = pushable_project();
            body["ci_push_repository_for_job_token_allowed"] = json!(true);
            (200, body.to_string())
        })
        .await;

        let error = fake
            .git_forge(credentials)
            .ensure_push_access(&index_repo())
            .await
            .expect_err("a publishing project outside the allowlist cannot push");
        assert!(
            matches!(
                error,
                ForgeError::WriteCapabilityUnavailable {
                    capability: CapabilityName::JobTokenAllowlist,
                    ..
                }
            ),
            "the refusal names the allowlist capability; got {error:?}"
        );
        let rendered = error.to_string();
        for path in ["acme/index", "acme/widget"] {
            assert!(
                rendered.contains(path),
                "neither path alone tells an operator which project to edit: {rendered}"
            );
        }
    }

    // ── #430 the groups allowlist ────────────────────────────────────────

    /// A group entry admits what is **under** it, and only that.
    ///
    /// A path-component prefix, never a string prefix. `starts_with` alone
    /// over-admits `acme/packages-legacy/widget` — and over-admitting is not a
    /// harmless direction: ocx would report the capability `passed` for a
    /// project GitLab's own scope check refuses, then let the push fail naming
    /// something else entirely.
    #[test]
    fn a_group_admits_by_path_component_never_by_string_prefix() {
        let cases: [(&str, &str, &str, bool); 7] = [
            ("the group's own child", "acme/packages", "acme/packages/widget", true),
            ("a nested subgroup", "acme/packages", "acme/packages/tools/widget", true),
            (
                "a sibling namespace",
                "acme/packages",
                "acme/packages-legacy/widget",
                false,
            ),
            ("operator-typed casing", "ACME/Packages", "acme/packages/widget", true),
            ("a truncated group name", "acme/pack", "acme/packages/widget", false),
            ("a top-level group", "acme", "acme/widget", true),
            ("the group path itself", "acme/packages", "acme/packages", false),
        ];
        for (label, group, publishing, expected) in cases {
            assert_eq!(
                group_contains(group, publishing),
                expected,
                "{label}: {group} vs {publishing}"
            );
        }
    }

    /// The group's path comes out of its `web_url`, because the entry carries
    /// nothing else that names it.
    ///
    /// GitLab's groups-allowlist entries are `id`, `web_url` and `name` — the
    /// display name, not the path. A URL that is not a group URL yields `None`,
    /// which the walk reads as an unreadable shape rather than as a miss.
    ///
    /// Reds on: splitting the raw string instead of the parsed path (the query
    /// and fragment rows return `acme/packages?x=1`, which `group_contains`
    /// reads as a named miss and refuses the run at 86); dropping the
    /// empty-path guard (the bare-marker row returns `Some("")`, which is a
    /// named entry that admits nothing rather than an unreadable shape).
    #[test]
    fn a_group_path_is_the_segment_after_the_first_groups_marker() {
        let cases: [(&str, &str, Option<&str>); 10] = [
            (
                "a top-level group",
                "https://gitlab.example.com/groups/acme",
                Some("acme"),
            ),
            (
                "a subgroup",
                "https://gitlab.example.com/groups/acme/packages",
                Some("acme/packages"),
            ),
            (
                "an instance under a relative root",
                "https://example.com/gitlab/groups/acme/packages",
                Some("acme/packages"),
            ),
            (
                "a group literally named groups",
                "https://gitlab.example.com/groups/acme/groups/tools",
                Some("acme/groups/tools"),
            ),
            (
                "a trailing slash",
                "https://gitlab.example.com/groups/acme/",
                Some("acme"),
            ),
            (
                "a project URL, which is not a group",
                "https://gitlab.example.com/acme/widget",
                None,
            ),
            (
                "a query string, which is not part of the path",
                "https://gitlab.example.com/groups/acme/packages?x=1",
                Some("acme/packages"),
            ),
            (
                "a fragment, likewise",
                "https://gitlab.example.com/groups/acme/packages#members",
                Some("acme/packages"),
            ),
            (
                "the marker with nothing after it",
                "https://gitlab.example.com/groups/",
                None,
            ),
            // A relative reference is a shape this client cannot resolve, which
            // is unreadable rather than a group named `acme`.
            ("not an absolute URL", "/groups/acme", None),
        ];
        for (label, web_url, expected) in cases {
            assert_eq!(
                group_path_from_web_url(web_url).as_deref(),
                expected,
                "{label}: {web_url}"
            );
        }
    }

    /// The defect #430 reports: the projects list is readable and empty, a
    /// group admits, and the run **proceeds**.
    ///
    /// An index that admits its publishers by group — one entry for a whole
    /// `packages/` group rather than one per publisher — carries nobody in the
    /// projects list, so reading only that list refused at 86 an announce
    /// GitLab would have accepted, and told the operator to add an entry their
    /// group entry already covered.
    ///
    /// The body served is the real one: `id`, `name` and `web_url`, with no
    /// path field. A fake that invented `full_path` would let a client reading
    /// the wrong key pass here and fail against an instance.
    #[tokio::test]
    async fn a_group_entry_admits_the_publishing_project() {
        let env = isolated_env();
        env.set("CI_PROJECT_PATH", "acme/packages/widget");
        let credentials = job_token_credentials(&env);
        let fake = FakeGitLab::start(|_, target| {
            if target.contains(GROUPS_ALLOWLIST_ENDPOINT) {
                return (
                    200,
                    json!([{
                        "id": 18000,
                        "name": "Packages",
                        "web_url": "https://gitlab.example.com/groups/acme/packages",
                    }])
                    .to_string(),
                );
            }
            if target.contains(ALLOWLIST_ENDPOINT) {
                return (200, Value::Array(Vec::new()).to_string());
            }
            let mut body = pushable_project();
            body["ci_push_repository_for_job_token_allowed"] = json!(true);
            (200, body.to_string())
        })
        .await;

        let access = fake
            .git_forge(credentials)
            .ensure_push_access(&index_repo())
            .await
            .expect("a group entry is an admission");
        assert_eq!(
            access.status(CapabilityName::JobTokenAllowlist),
            CheckStatus::Passed,
            "an admission read out of the second list is `passed`, not `unknown`"
        );
    }

    /// The groups list is read **only** when the projects list answered without
    /// a hit.
    ///
    /// Both halves matter. A build that always asks spends a request on every
    /// cross-project announce and makes the reported request count a lie (DV-6);
    /// a build that never asks is the defect. The two rows differ only in
    /// whether the projects list carries the publisher.
    #[tokio::test]
    async fn the_groups_list_is_read_only_after_the_projects_list_misses() {
        for (label, listed, expect_groups_read) in [
            ("the projects list admits", "acme/widget", false),
            ("the projects list misses", "acme/other", true),
        ] {
            let env = isolated_env();
            env.set("CI_PROJECT_PATH", "acme/widget");
            let credentials = job_token_credentials(&env);
            let listed = listed.to_string();
            let fake = FakeGitLab::start(move |_, target| {
                if target.contains(GROUPS_ALLOWLIST_ENDPOINT) {
                    return (200, Value::Array(Vec::new()).to_string());
                }
                if target.contains(ALLOWLIST_ENDPOINT) {
                    return (200, json!([{ "path_with_namespace": listed }]).to_string());
                }
                let mut body = pushable_project();
                body["ci_push_repository_for_job_token_allowed"] = json!(true);
                (200, body.to_string())
            })
            .await;

            // The miss row refuses; the admit row proceeds. Either way the
            // routes are what this asserts.
            let _ = fake.git_forge(credentials).ensure_push_access(&index_repo()).await;
            let read = fake
                .routes()
                .iter()
                .any(|route| route.contains(GROUPS_ALLOWLIST_ENDPOINT));
            assert_eq!(read, expect_groups_read, "{label}: {:?}", fake.routes());
        }
    }

    /// An unreadable **groups** list is `unknown`, never a miss — and the row
    /// names the list that could not be read.
    ///
    /// The 403 is not an edge case: both endpoints want Maintainer or Owner on
    /// the index project while this preflight's own bar is Developer. #430's
    /// rule is that *either* list being unreadable leaves the question
    /// unanswered, so a projects-list miss plus an unreadable groups list must
    /// not refuse — refusing there is the false 86 the issue reports, one
    /// endpoint further along.
    #[tokio::test]
    async fn an_unreadable_groups_list_is_unknown_never_a_miss() {
        for (label, status, body) in [
            ("403, the Maintainer bar", 403, json!({ "message": "403 Forbidden" })),
            (
                "404, an instance without the endpoint",
                404,
                json!({ "message": "404 Not Found" }),
            ),
            ("a body that is not a list", 200, json!({ "groups_allowlist": [] })),
            (
                "entries with no group URL",
                200,
                json!([{ "id": 18000, "name": "Packages" }]),
            ),
        ] {
            let env = isolated_env();
            env.set("CI_PROJECT_PATH", "acme/widget");
            let credentials = job_token_credentials(&env);
            let body = body.to_string();
            let fake = FakeGitLab::start(move |_, target| {
                if target.contains(GROUPS_ALLOWLIST_ENDPOINT) {
                    return (status, body.clone());
                }
                if target.contains(ALLOWLIST_ENDPOINT) {
                    return (200, json!([{ "path_with_namespace": "acme/other" }]).to_string());
                }
                let mut project = pushable_project();
                project["ci_push_repository_for_job_token_allowed"] = json!(true);
                (200, project.to_string())
            })
            .await;

            let access = fake
                .git_forge(credentials)
                .ensure_push_access(&index_repo())
                .await
                .unwrap_or_else(|error| panic!("{label}: an unreadable groups list never refuses: {error:?}"));
            assert_eq!(
                access.status(CapabilityName::JobTokenAllowlist),
                CheckStatus::Unknown,
                "{label}: the question is unanswered, not answered no"
            );
            assert_eq!(
                access
                    .checks()
                    .iter()
                    .find(|check| check.name == CapabilityName::JobTokenAllowlist)
                    .and_then(|check| check.detail.as_deref()),
                Some(GROUPS_ALLOWLIST_ENDPOINT),
                "{label}: the row names the list that went unreadable, not the one that answered"
            );
        }
    }

    /// The allowlist is offset-paginated, and a publishing project on page two
    /// is **not** a miss.
    ///
    /// The most dangerous cell in this package. GitLab's default page size is
    /// 20 and C-029 says nothing about paging, so a page-one miss becomes a
    /// false 86 before any push — a perfectly valid run refused. The walk is
    /// bounded, and a walk that exhausts its ceiling without a hit is
    /// `unknown`, never a miss: an incomplete answer must not refuse.
    #[tokio::test]
    async fn allowlist_walks_past_the_first_page_and_never_misses_on_an_incomplete_walk() {
        let env = isolated_env();
        env.set("CI_PROJECT_PATH", "acme/widget");
        let credentials = job_token_credentials(&env);
        let fake = FakeGitLab::start(|_, target| {
            if !target.contains("job_token_scope") {
                let mut body = pushable_project();
                body["ci_push_repository_for_job_token_allowed"] = json!(true);
                return (200, body.to_string());
            }
            if target.contains("&page=1") {
                // A full page that does not carry the publishing project: the
                // client must ask for the next one.
                let filler: Vec<Value> = (0..100)
                    .map(|n| json!({ "path_with_namespace": format!("acme/filler-{n}") }))
                    .collect();
                return (200, Value::Array(filler).to_string());
            }
            (200, json!([{ "path_with_namespace": "acme/widget" }]).to_string())
        })
        .await;

        let access = fake
            .git_forge(credentials)
            .ensure_push_access(&index_repo())
            .await
            .expect("the publishing project is on page two, so the run proceeds");
        assert_eq!(access.status(CapabilityName::JobTokenAllowlist), CheckStatus::Passed);
        assert!(
            fake.routes().iter().any(|route| route.contains("page=2")),
            "the walk must reach page two: {:?}",
            fake.routes()
        );
        assert!(
            fake.routes()
                .iter()
                .all(|route| !route.contains("job_token_scope") || route.contains("per_page=100")),
            "the walk asks for the largest page the API serves: {:?}",
            fake.routes()
        );

        // A server that never runs out of full pages exhausts the ceiling. That
        // is an incomplete answer, so it is `unknown` — refusing here would
        // turn a bounded walk into a false 86.
        clear_ci(&env);
        env.set("CI_PROJECT_PATH", "acme/widget");
        let credentials = job_token_credentials(&env);
        let fake = FakeGitLab::start(|_, target| {
            if !target.contains("job_token_scope") {
                let mut body = pushable_project();
                body["ci_push_repository_for_job_token_allowed"] = json!(true);
                return (200, body.to_string());
            }
            let filler: Vec<Value> = (0..100)
                .map(|n| json!({ "path_with_namespace": format!("acme/filler-{n}") }))
                .collect();
            (200, Value::Array(filler).to_string())
        })
        .await;

        let access = fake
            .git_forge(credentials)
            .ensure_push_access(&index_repo())
            .await
            .expect("an exhausted ceiling never refuses");
        assert_eq!(
            access.status(CapabilityName::JobTokenAllowlist),
            CheckStatus::Unknown,
            "a walk that ran out of pages did not answer the question"
        );
    }

    /// Every allowlist answer that is not a readable list is `unknown`, never a
    /// miss — and an unreachable instance is neither.
    ///
    /// The false-refusal direction is the one that costs a user a working run,
    /// so 403 (the production-common answer: the endpoint wants Maintainer),
    /// 401 under a job token (the same closed door wearing the other status —
    /// what a real GitLab answers a `JOB-TOKEN` header here, #432), 404 (an
    /// instance older than the endpoint), a non-array body and entries that
    /// carry no `path_with_namespace` all proceed. A 5xx is not a capability
    /// answer at all and propagates.
    ///
    /// The 401 row's gate lives in
    /// `a_non_job_token_401_on_the_allowlist_stays_a_status_error`: without it
    /// this table would pass against a build that read every 401 as unreadable
    /// and pushed on a revoked personal access token.
    #[tokio::test]
    async fn allowlist_unreadable_answers_are_unknown_never_a_miss() {
        let cases: [(&str, u16, Value); 5] = [
            (
                "403 — the endpoint wants Maintainer",
                403,
                json!({ "message": "403 Forbidden" }),
            ),
            (
                "401 — what a real GitLab answers a JOB-TOKEN here (#432)",
                401,
                json!({ "message": "401 Unauthorized" }),
            ),
            (
                "404 — an instance older than the endpoint",
                404,
                json!({ "message": "404 Not Found" }),
            ),
            ("a body that is not a list", 200, json!({ "allowlist": [] })),
            ("entries carrying no path", 200, json!([{ "id": 3 }, { "id": 4 }])),
        ];
        for (label, status, body) in cases {
            let env = isolated_env();
            env.set("CI_PROJECT_PATH", "acme/widget");
            let credentials = job_token_credentials(&env);
            let rendered = body.to_string();
            let fake = FakeGitLab::start(move |_, target| {
                if target.contains("job_token_scope") {
                    return (status, rendered.clone());
                }
                let mut project = pushable_project();
                project["ci_push_repository_for_job_token_allowed"] = json!(true);
                (200, project.to_string())
            })
            .await;

            let access = fake
                .git_forge(credentials)
                .ensure_push_access(&index_repo())
                .await
                .unwrap_or_else(|error| panic!("{label}: an unreadable allowlist never refuses: {error:?}"));
            assert_eq!(
                access.status(CapabilityName::JobTokenAllowlist),
                CheckStatus::Unknown,
                "{label}"
            );
        }

        // A 5xx is a fault, not a capability answer.
        let env = isolated_env();
        env.set("CI_PROJECT_PATH", "acme/widget");
        let credentials = job_token_credentials(&env);
        let fake = FakeGitLab::start(|_, target| {
            if target.contains("job_token_scope") {
                return (503, json!({ "message": "503 Service Unavailable" }).to_string());
            }
            let mut project = pushable_project();
            project["ci_push_repository_for_job_token_allowed"] = json!(true);
            (200, project.to_string())
        })
        .await;
        let error = fake
            .git_forge(credentials)
            .ensure_push_access(&index_repo())
            .await
            .expect_err("an unreachable instance is not an answer");
        assert!(
            matches!(error, ForgeError::Status { status: 503, .. }),
            "a 5xx propagates rather than becoming a capability verdict; got {error:?}"
        );
    }

    /// A 401 on the allowlist under a credential that is **not** the job token
    /// stays an ordinary status error.
    ///
    /// The other half of the #432 fix, and the half that makes the first one
    /// mean something. `Unreadable` proceeds to the push; `Status` classifies to
    /// `AuthError` (80) and tells the operator to replace the credential. A
    /// status-only gate — one that mapped every 401 to `Unreadable` — would pass
    /// the row above while silently pushing on a revoked personal access token
    /// and reporting the allowlist as `unknown`.
    ///
    /// The split pair is the posture that reaches this line at all: the push
    /// half is the job token, so the walk runs, while the API half is a PAT, so
    /// the walk travels as `PRIVATE-TOKEN`. That is the arrangement the docs
    /// call the split credential pair.
    #[tokio::test]
    async fn a_non_job_token_401_on_the_allowlist_stays_a_status_error() {
        let env = isolated_env();
        env.set("CI_PROJECT_PATH", "acme/widget");
        let credentials = credentials_for(&env, CredentialShape::PushHalfOnly);
        assert!(
            credentials.push_is_job_token() && !credentials.api_is_job_token(),
            "the fixture must produce a job-token push with an ordinary API half, or this row \
             measures the job-token posture a second time"
        );
        let fake = FakeGitLab::start(|_, target| {
            if target.contains("job_token_scope") {
                return (401, json!({ "message": "401 Unauthorized" }).to_string());
            }
            let mut project = pushable_project();
            project["ci_push_repository_for_job_token_allowed"] = json!(true);
            (200, project.to_string())
        })
        .await;

        let error = fake
            .git_forge(credentials)
            .ensure_push_access(&index_repo())
            .await
            .expect_err("a rejected API credential is not a closed door");
        assert!(
            matches!(error, ForgeError::Status { status: 401, .. }),
            "a 401 that is not a job token's closed door must stay a status error; got {error:?}"
        );
    }

    /// A job-token push with **no** publishing project is `unknown`, not
    /// `skipped`, and it does not refuse.
    ///
    /// Reachable two ways — outside CI, and silently when `CI_PROJECT_PATH`
    /// fails segment validation, which the credential ladder converts into an
    /// absence rather than an error. `skipped` would claim the check does not
    /// apply when it does; refusing would break a runner that scrubs the
    /// variable. The residual is stated rather than hidden: the validator fails
    /// **open**, so a hostile `CI_PROJECT_PATH` suppresses a safety check —
    /// accepted, because the check it suppresses is a safety gate rather than a
    /// security one.
    #[tokio::test]
    async fn a_job_token_push_with_no_publishing_project_is_unknown() {
        for publishing in [None, Some("acme?x=1/index")] {
            let env = isolated_env();
            match publishing {
                Some(value) => env.set("CI_PROJECT_PATH", value),
                None => env.remove("CI_PROJECT_PATH"),
            }
            let credentials = job_token_credentials(&env);
            assert_eq!(
                credentials.publishing_project(),
                None,
                "the fixture must leave it absent"
            );
            let fake = FakeGitLab::start(|_, _| {
                let mut body = pushable_project();
                body["ci_push_repository_for_job_token_allowed"] = json!(true);
                (200, body.to_string())
            })
            .await;

            let access = fake
                .git_forge(credentials)
                .ensure_push_access(&index_repo())
                .await
                .expect("an absent publishing project never refuses");
            assert_eq!(access.status(CapabilityName::JobTokenAllowlist), CheckStatus::Unknown);
            assert_eq!(
                access
                    .checks()
                    .iter()
                    .find(|check| check.name == CapabilityName::JobTokenAllowlist)
                    .and_then(|check| check.detail.as_deref()),
                Some("CI_PROJECT_PATH"),
                "the row names what could not be read"
            );
            assert!(
                fake.routes().iter().all(|route| !route.contains("job_token_scope")),
                "there is nothing to look up, so nothing is read: {:?}",
                fake.routes()
            );
        }
    }

    /// The preflight's 86 and the push classifier's 86 carry **different**
    /// remedies.
    ///
    /// Both are `WriteCapabilityUnavailable` and both exit 86, so a test
    /// asserting only the exit code cannot tell them apart — and a builder who
    /// reuses one string ships a wrong diagnosis for the other. The preflight
    /// read the field and saw `false`, so it names the setting; the classifier
    /// could not read the field at all and the push was refused, so it says
    /// both of those and nothing about a setting it never observed.
    /// `git_stderr.rs` asserts the same disjointness from its own side.
    #[tokio::test]
    async fn the_preflight_remedy_is_not_the_push_classifier_remedy() {
        let env = isolated_env();
        env.set("CI_PROJECT_PATH", "acme/index");
        let credentials = job_token_credentials(&env);
        let fake = FakeGitLab::start(|_, _| {
            let mut body = pushable_project();
            body["ci_push_repository_for_job_token_allowed"] = json!(false);
            (200, body.to_string())
        })
        .await;

        let error = fake
            .git_forge(credentials)
            .ensure_push_access(&index_repo())
            .await
            .expect_err("a disabled setting refuses");
        let ForgeError::WriteCapabilityUnavailable { remedy, .. } = &error else {
            panic!("expected a capability refusal, got {error:?}");
        };
        assert!(
            remedy.contains("Job token permissions"),
            "the preflight read the setting, so it names it: {remedy}"
        );
        assert!(
            !remedy.contains("unreadable"),
            "the preflight READ the field — the two-signal wording belongs to the classifier's 86: {remedy}"
        );
    }

    /// No byte of a forge response reaches a capability `detail` or a refusal
    /// `remedy`.
    ///
    /// Driven behaviourally rather than by scanning this file's source: a
    /// source-text guard for `status_detail` near a `record(` call would match
    /// this module's own prose in every state. Instead every string field of
    /// both response bodies carries a sentinel, and nothing rendered may
    /// contain it.
    ///
    /// The sentinel is deliberately **non-numeric**: C-011's closed set
    /// whitelists the numeric access level, which the `push-access` row
    /// legitimately interpolates.
    #[tokio::test]
    async fn no_response_byte_reaches_a_capability_detail_or_remedy() {
        const SENTINEL: &str = "zzsentinelzz";

        let env = isolated_env();
        env.set("CI_PROJECT_PATH", "acme/widget");
        let credentials = job_token_credentials(&env);
        let fake = FakeGitLab::start(|_, target| {
            if target.contains("job_token_scope") {
                return (
                    200,
                    json!([{ "path_with_namespace": "acme/widget", "name": SENTINEL, "description": SENTINEL }])
                        .to_string(),
                );
            }
            (
                200,
                json!({
                    "id": 42,
                    "path_with_namespace": SENTINEL,
                    "name": SENTINEL,
                    "description": SENTINEL,
                    "default_branch": SENTINEL,
                    "ci_push_repository_for_job_token_allowed": true,
                    "permissions": { "project_access": { "access_level": 30 }, "group_access": null },
                })
                .to_string(),
            )
        })
        .await;

        // The passing shape: every row that carries a detail carries only what
        // ocx already held.
        let access = fake
            .git_forge(credentials)
            .ensure_push_access(&index_repo())
            .await
            .expect("the run proceeds");
        for check in access.checks() {
            assert!(
                check.detail.as_deref().is_none_or(|detail| !detail.contains(SENTINEL)),
                "{} carries a response byte: {:?}",
                check.name,
                check.detail
            );
        }

        // And the refusing shape, whose remedy is the other user-facing string
        // built on this path from this response.
        clear_ci(&env);
        env.set("CI_PROJECT_PATH", "acme/widget");
        let credentials = job_token_credentials(&env);
        let fake = FakeGitLab::start(|_, target| {
            if let Some(answer) = empty_groups_allowlist(target) {
                return answer;
            }
            if target.contains("job_token_scope") {
                return (200, json!([{ "path_with_namespace": SENTINEL }]).to_string());
            }
            (
                200,
                json!({
                    "id": 42,
                    "path_with_namespace": SENTINEL,
                    "description": SENTINEL,
                    "ci_push_repository_for_job_token_allowed": true,
                    "permissions": { "project_access": { "access_level": 30 }, "group_access": null },
                })
                .to_string(),
            )
        })
        .await;
        let error = fake
            .git_forge(credentials)
            .ensure_push_access(&index_repo())
            .await
            .expect_err("the publishing project is not in that allowlist");
        assert!(
            !error.to_string().contains(SENTINEL),
            "the remedy carries a response byte: {error}"
        );
    }

    // ── C-030 the branch-existence read ──────────────────────────────────

    /// A 404 on the branch is `Absent` — the claim's branch-existence read
    /// (DV-6), served by the Branches API call `get_ref_sha` already makes
    /// rather than by a second client method.
    ///
    /// **Three different 404s collapse here and the conflation is accepted**:
    /// the branch is absent, the project is absent or renamed, or the token
    /// cannot see the project (GitLab masks 403 as 404 on a private project).
    /// The only available discriminator is the body string, and branching on it
    /// is both the C-011 anti-pattern and version-fragile. Blast radius, stated
    /// so it is a decision rather than an oversight: on write paths the
    /// invisible project still surfaces correctly as `PushAccessDenied` one step
    /// later; on read-only paths it reports "no branch" where the truth is
    /// "invisible project" — a wrong diagnosis, never a wrong write.
    ///
    /// The second half pins C-031's boundary from the side that can red: the
    /// read stays REST under the **git** transport too. A blanket "if git, use
    /// the clone" arm would divert it, and the clone cannot answer for a
    /// branch it has not fetched.
    #[tokio::test]
    async fn branch_absent_on_404() {
        let env = isolated_env();
        let credentials = job_token_credentials(&env);
        let fake = FakeGitLab::start(|_, _| (404, json!({ "message": "404 Branch Not Found" }).to_string())).await;

        for forge in [fake.forge(credentials.clone()), fake.git_forge(credentials)] {
            assert_eq!(
                forge
                    .get_ref_sha(&index_repo(), "heads/indexbot-claim-acme-widget")
                    .await
                    .expect("a 404 is an answer, not a failure"),
                None,
                "an absent branch is Ok(None)"
            );
        }
        assert_eq!(
            fake.routes(),
            vec![
                "GET /projects/acme%2Findex/repository/branches\
                 ?search=%5Eindexbot-claim-acme-widget&per_page=100&page=1"
                    .to_string();
                2
            ],
            "the branch read is REST under both transports"
        );
    }

    /// A ref given in full (`refs/heads/x`) resolves the same branch as the
    /// short form.
    ///
    /// The Branches API takes a bare branch name. Stripping only `heads/`
    /// leaves `refs/heads/x` to be percent-encoded as one segment, which 404s,
    /// which reads as `Absent` — and under C-051 an `Absent` branch that
    /// exists is what creates a duplicate branch. The claim orchestration is
    /// new surface, so the caller convention is not yet fixed and the client
    /// must not depend on it.
    #[tokio::test]
    async fn get_ref_sha_accepts_a_ref_in_either_form() {
        let env = isolated_env();
        let credentials = job_token_credentials(&env);
        let fake = FakeGitLab::start(|_, target| {
            if target.contains("/repository/branches?search=%5Emain&") {
                return (
                    200,
                    json!([{ "name": "main", "commit": { "id": BRANCH_HEAD } }]).to_string(),
                );
            }
            (200, json!([]).to_string())
        })
        .await;

        for form in ["main", "heads/main", "refs/heads/main"] {
            assert_eq!(
                fake.forge(credentials.clone())
                    .get_ref_sha(&index_repo(), form)
                    .await
                    .expect("the read succeeds"),
                Some(BRANCH_HEAD.to_string()),
                "{form} names the branch `main`"
            );
        }
    }

    /// A branch document with no commit id is a missing field, never `Absent`.
    ///
    /// "No commit means no branch" is the tempting simplification and it
    /// creates a duplicate branch exactly as a mis-stripped ref does.
    #[tokio::test]
    async fn a_branch_without_a_commit_id_is_a_missing_field() {
        let env = isolated_env();
        let credentials = job_token_credentials(&env);
        for body in [json!({ "name": "main" }), json!({ "name": "main", "commit": null })] {
            let rendered = body.to_string();
            let fake = FakeGitLab::start(move |_, _| (200, rendered.clone())).await;
            let error = fake
                .forge(credentials.clone())
                .get_ref_sha(&index_repo(), "heads/main")
                .await
                .expect_err("a branch that exists must carry its head");
            assert!(
                matches!(error, ForgeError::MissingField { .. }),
                "an unreadable head must not read as an absent branch; got {error:?}"
            );
        }
    }

    /// A `commit.id` that is not a full object name is refused, not passed on.
    ///
    /// The value is forge-controlled input at a trust boundary: it goes on to be
    /// a bare positional of `git read-tree` and the right-hand side of a
    /// `--force-with-lease`. Measured against git 2.54.0, a positional beginning
    /// with `-` is parsed as an option — `git read-tree --upload-pack=/bin/false`
    /// exits 129 with "unknown option", not with a bad-revision error — and
    /// `--upload-pack` names a program git runs.
    ///
    /// The rows are the classes a narrower guard misses, not a list of typos: an
    /// abbreviation (which git resolves, ambiguously); the same abbreviation
    /// padded to the right *length* with non-hex bytes, which defeats a
    /// length-only check; and a full-length dashed value, which defeats a
    /// leading-character check applied to short input only.
    ///
    /// The refusal names the field and never the value — the value is
    /// operator-visible text on a path that reaches a CI log.
    ///
    /// Reds on: deleting the shape filter; checking length without the charset;
    /// checking the charset without the length; accepting an empty string.
    #[tokio::test]
    async fn a_commit_id_that_is_not_an_object_name_is_refused() {
        let env = isolated_env();
        let credentials = job_token_credentials(&env);
        let hostile = [
            "",
            "abcdef",
            "--upload-pack=/bin/false",
            // Forty characters, none of them hex past the first six.
            "abcdef--upload-pack=/bin/false-----zzzzz",
            // Forty hex digits with one leading dash, so the length is right and
            // the first byte is not.
            "-f2c8b9d1e4a7063f8b2c5d9e1a4706358b2c5d9",
            "5f2c8b9d1e4a7063f8b2c5d9e1a4706358b2c5d",
        ];
        for id in hostile {
            let body = json!([{ "name": "main", "commit": { "id": id } }]).to_string();
            let fake = FakeGitLab::start(move |_, _| (200, body.clone())).await;
            let error = fake
                .forge(credentials.clone())
                .get_ref_sha(&index_repo(), "heads/main")
                .await
                .expect_err("a head that is not an object name must not reach git");
            let rendered = error.to_string();
            match error {
                ForgeError::MissingField { field, .. } => {
                    assert_eq!(field, "commit.id", "the refusal names the field it refused");
                }
                other => panic!("{id:?} must be refused as an unusable head; got {other:?}"),
            }
            assert!(
                id.is_empty() || !rendered.contains(id),
                "the refusal reproduced the offending value into a CI log: {rendered:?}"
            );
        }
    }

    /// Both hash lengths a forge can answer with are accepted, unaltered.
    ///
    /// The permissive half. Without it a guard of `false` passes every row above
    /// while refusing every real branch read — and the SHA-256 row is the one a
    /// SHA-1-only guard would break on a forge state that is supported today.
    ///
    /// Reds on: hard-coding 40; refusing uppercase hex; trimming or rewriting the
    /// accepted value.
    #[tokio::test]
    async fn a_full_object_name_of_either_hash_length_is_accepted() {
        let env = isolated_env();
        let credentials = job_token_credentials(&env);
        let accepted = [
            BRANCH_HEAD,
            "5F2C8B9D1E4A7063F8B2C5D9E1A4706358B2C5D9",
            "5f2c8b9d1e4a7063f8b2c5d9e1a4706358b2c5d95f2c8b9d1e4a7063f8b2c5d9",
        ];
        for id in accepted {
            let body = json!([{ "name": "main", "commit": { "id": id } }]).to_string();
            let fake = FakeGitLab::start(move |_, _| (200, body.clone())).await;
            assert_eq!(
                fake.forge(credentials.clone())
                    .get_ref_sha(&index_repo(), "heads/main")
                    .await
                    .expect("a full object name is what a branch read returns"),
                Some(id.to_string()),
                "{id} must survive the guard byte for byte"
            );
        }
    }

    /// The branch read picks the entry it asked for, not the first one back.
    ///
    /// `search=^main` is a PREFIX filter, so `maintenance` and `main-old` come
    /// back with `main` and GitLab promises no order. Taking `[0]` would hand a
    /// *different branch's* head to `git read-tree` and to the right-hand side
    /// of a `--force-with-lease` — the two places this value is load-bearing.
    ///
    /// The list is deliberately ordered with the wanted entry last: a fixture
    /// that answered it first would let `[0]` pass.
    ///
    /// Reds on: `.first()` instead of the name match; matching on a prefix
    /// rather than equality.
    #[tokio::test]
    async fn a_prefix_search_selects_the_exact_branch_name() {
        let env = isolated_env();
        let credentials = job_token_credentials(&env);
        let fake = FakeGitLab::start(|_, _| {
            (
                200,
                json!([
                    { "name": "maintenance", "commit": { "id": "1111111111111111111111111111111111111111" } },
                    { "name": "main-old", "commit": { "id": "2222222222222222222222222222222222222222" } },
                    { "name": "main", "commit": { "id": BRANCH_HEAD } },
                ])
                .to_string(),
            )
        })
        .await;

        assert_eq!(
            fake.forge(credentials)
                .get_ref_sha(&index_repo(), "heads/main")
                .await
                .expect("the branch read succeeds"),
            Some(BRANCH_HEAD.to_string()),
            "the entry whose name IS the branch is the one that answers"
        );
    }

    /// A prefix search that matches only neighbours is an absent branch.
    ///
    /// The list endpoint answers 200 with entries, so "not found" here is a
    /// non-empty body containing no exact match — a state the single-branch
    /// endpoint's 404 could never produce, and the one an `[0]` implementation
    /// would read as `Some`.
    #[tokio::test]
    async fn a_prefix_match_that_is_not_the_branch_is_absent() {
        let env = isolated_env();
        let credentials = job_token_credentials(&env);
        let fake = FakeGitLab::start(|_, _| {
            (
                200,
                json!([{ "name": "main-old", "commit": { "id": BRANCH_HEAD } }]).to_string(),
            )
        })
        .await;

        assert_eq!(
            fake.forge(credentials)
                .get_ref_sha(&index_repo(), "heads/main")
                .await
                .expect("a non-matching page is an answer, not a failure"),
            None,
            "only an exact name is this branch"
        );
    }

    /// A prefix with more collisions than one page is walked, not truncated.
    ///
    /// `search=^main` is a prefix filter over a page GitLab caps at 100, so an
    /// index whose base branch shares its prefix with a full page of others read
    /// as *absent* — which the caller reports as `MissingBaseRef` and exits 1,
    /// the same failure [#429] fixes by another cause.
    ///
    /// The fixture serves the wanted entry on page **two**, behind a page one
    /// that is exactly full: a short page one would let a single-request read
    /// pass, and a short page two proves the walk stops rather than spinning to
    /// the ceiling.
    ///
    /// Reds on: dropping the walk (page one is all that is read, and the answer
    /// is `None`); stopping on a full page.
    ///
    /// [#429]: https://github.com/ocx-sh/ocx/issues/429
    #[tokio::test]
    async fn a_prefix_search_walks_past_a_full_page() {
        let env = isolated_env();
        let credentials = job_token_credentials(&env);
        let fake = FakeGitLab::start(|_, target| {
            if target.contains("page=2") {
                return (
                    200,
                    json!([{ "name": "main", "commit": { "id": BRANCH_HEAD } }]).to_string(),
                );
            }
            let page = (0..BRANCH_SEARCH_PER_PAGE)
                .map(|n| json!({ "name": format!("main-{n}"), "commit": { "id": "1111111111111111111111111111111111111111" } }))
                .collect::<Vec<_>>();
            (200, Value::Array(page).to_string())
        })
        .await;

        assert_eq!(
            fake.forge(credentials)
                .get_ref_sha(&index_repo(), "heads/main")
                .await
                .expect("the branch read succeeds"),
            Some(BRANCH_HEAD.to_string()),
            "a full page of prefix collisions is not the end of the search"
        );
        assert_eq!(
            fake.routes().len(),
            2,
            "the walk stops on the short page rather than running to the ceiling"
        );
    }

    /// A job token cannot read `GET /projects/:id`, so under the **git**
    /// transport the push-access row is `unknown` and the run proceeds ([#429])
    /// — and under `api` the same 404 still refuses.
    ///
    /// The endpoint is not on GitLab's job-token list and answers 404 — the same
    /// status a project that is not there answers. Collapsing that to access
    /// level 0 exits 80 before a push GitLab would have accepted, which is the
    /// whole defect. And the level would be the wrong credential's anyway: it
    /// describes the API half, while the identity that pushes is the push half.
    /// `JobTokenPush` folds to `unknown` off the same absent body, which is what
    /// `git_stderr.rs` promotes on — so a push GitLab really does refuse still
    /// lands as 86 naming the setting.
    ///
    /// **Two cells, opposite verdicts, one predicate**, because the whole
    /// tolerance rests on a push following to render the deferred verdict. An
    /// `api` run performs none, and `api_is_job_token` does not imply one:
    /// `ForgeCredentials` sets it by value-equality against `CI_JOB_TOKEN`
    /// (`credentials.rs`), so `OCX_ANNOUNCE_TOKEN=$CI_JOB_TOKEN` reaches it
    /// under `--transport api`. There the suppressed exit-80 becomes a bare 404
    /// out of `commit_files`, which classifies to nothing and exits 1.
    ///
    /// Reds on: refusing on an unreadable project under a git-transport job
    /// token; recording that row as `passed` (86 would then never be promoted);
    /// dropping the `transport == Git` conjunct (the `api` cell then reports
    /// `unknown` instead of refusing).
    ///
    /// [#429]: https://github.com/ocx-sh/ocx/issues/429
    #[tokio::test]
    async fn an_unreadable_project_under_a_job_token_is_unknown_only_where_a_push_decides() {
        for (label, transport, denied) in [
            (
                "the api transport, where no push renders the verdict",
                WriteTransport::Api,
                true,
            ),
            ("the git transport, where the push decides", WriteTransport::Git, false),
        ] {
            let env = isolated_env();
            env.set("CI_PROJECT_PATH", "acme/index");
            let credentials = job_token_credentials(&env);
            assert!(
                credentials.api_is_job_token(),
                "{label}: both cells run the same job-token credential"
            );
            let fake = FakeGitLab::start(|_, _| (404, json!({ "message": "404 Project Not Found" }).to_string())).await;
            let forge = match transport {
                WriteTransport::Git => fake.git_forge(credentials),
                WriteTransport::Api => fake.forge(credentials),
            };

            let outcome = forge.ensure_push_access(&index_repo()).await;
            if denied {
                let error = outcome
                    .err()
                    .unwrap_or_else(|| panic!("{label}: an unreadable project must still refuse"));
                assert!(
                    matches!(error, ForgeError::PushAccessDenied { .. }),
                    "{label}: the refusal is the push-access one; got {error:?}"
                );
                continue;
            }
            let checks = outcome.unwrap_or_else(|error| panic!("{label}: the run must not be refused: {error:?}"));
            assert_eq!(
                checks.status(CapabilityName::PushAccess),
                CheckStatus::Unknown,
                "{label}: the access level is unreadable, not zero"
            );
            assert_eq!(
                checks.status(CapabilityName::JobTokenPush),
                CheckStatus::Unknown,
                "{label}: the same absent body cannot answer the job-token-push question either — \
                 and this is the status the push-time promotion keys on"
            );
        }
    }

    /// The same 404, under a credential that is not a job token, still refuses.
    ///
    /// The regression guard on the row above: a personal access token *can* read
    /// the Projects API, so a 404 there means the project is not visible to it,
    /// which is the state exit 80 exists for. Without this the widening is
    /// indistinguishable from dropping the refusal outright.
    ///
    /// Reds on: keying the arm on anything but `api_is_job_token`.
    #[tokio::test]
    async fn an_unreadable_project_under_a_personal_token_is_still_denied() {
        let env = isolated_env();
        env.remove("CI_JOB_TOKEN");
        let credentials = ForgeCredentials::new(ForgeToken::new("glpat-notarealpat".to_string()));
        assert!(!credentials.api_is_job_token(), "the fixture must not be a job token");
        let fake = FakeGitLab::start(|_, _| (404, json!({ "message": "404 Project Not Found" }).to_string())).await;

        let error = fake
            .git_forge(credentials)
            .ensure_push_access(&index_repo())
            .await
            .expect_err("a project a personal token cannot see is a refusal");
        assert!(
            matches!(error, ForgeError::PushAccessDenied { .. }),
            "the refusal is the push-access one; got {error:?}"
        );
    }

    /// Looking a merge request up spends no `GET /projects/:id`, and still
    /// refuses one opened from somewhere else ([#429]).
    ///
    /// Both halves in one row because they are one decision. The numeric id used
    /// to come from the Projects API, which a job token cannot call; the guard it
    /// bought — "not a stranger's request on the same deterministic branch name"
    /// — is answered by the entry itself, since every merge request carries both
    /// its source and its target project id.
    ///
    /// The fixture answers a foreign request FIRST, so an implementation that
    /// takes `[0]` adopts it.
    ///
    /// Reds on: resolving the project id (a `/projects/` route appears); reading
    /// `[0]`; dropping the source/target comparison; dropping `per_page`, which
    /// leaves GitLab's default of twenty on a query that is no longer narrowed
    /// to one entry.
    ///
    /// [#429]: https://github.com/ocx-sh/ocx/issues/429
    #[tokio::test]
    async fn a_merge_request_lookup_spends_no_project_read_and_refuses_a_stranger() {
        let env = isolated_env();
        let credentials = job_token_credentials(&env);
        let fake = FakeGitLab::start(|_, _| {
            (
                200,
                json!([
                    // A third party's fork, proposing the same branch name onto
                    // the same index.
                    { "iid": 3, "web_url": "https://gitlab.example/mr/3",
                      "source_project_id": 7, "target_project_id": 42 },
                    { "iid": 4, "web_url": "https://gitlab.example/mr/4",
                      "source_project_id": 42, "target_project_id": 42 },
                ])
                .to_string(),
            )
        })
        .await;

        let found = fake
            .git_forge(credentials)
            .find_open_pull_request(&index_repo(), &index_repo(), "indexbot-claim-acme-widget")
            .await
            .expect("the list read succeeds")
            .expect("the index's own request is open");
        assert_eq!(found.number, 4, "a stranger's request must never be adopted");

        let projects: Vec<String> = fake
            .routes()
            .into_iter()
            .filter(|route| route.ends_with("/projects/acme%2Findex"))
            .collect();
        assert_eq!(
            projects,
            Vec::<String>::new(),
            "the Projects API is not readable with a job token, so nothing may need it"
        );
        // The same edit that removed `source_project_id` turned a query matching
        // at most one entry into a list, and GitLab's own default page is 20 —
        // so an index with more open requests onto this branch than that would
        // miss the existing one and open a duplicate.
        assert_eq!(
            fake.routes(),
            vec![
                "GET /projects/acme%2Findex/merge_requests\
                 ?state=opened&per_page=100&source_branch=indexbot-claim-acme-widget"
                    .to_string()
            ],
            "the list is asked for a whole page, not GitLab's default of twenty"
        );
    }

    // ── The one clone, and a later caller that disagrees with it ─────────────

    /// The clone is reused, re-fetched or refused according to which part of
    /// `(repo, base, branch)` a later caller changed.
    ///
    /// A `OnceCell` hands every caller after the first the value the first one
    /// produced, so the three call sites — the comparison, the commit and the
    /// publish — used to agree silently with whichever ran first. Two of those
    /// three arguments decide what the clone can *see*, which is why a
    /// disagreement is not cosmetic: an unfetched base or branch resolves to
    /// nothing, and `rev-parse --verify --quiet` reports it as absent, which is
    /// byte-identical to a branch that does not exist. `commit_files` then
    /// parents on the caller's base instead of the branch head and drops every
    /// tag the branch already carried.
    ///
    /// The two answers differ because the disagreements do. A fetch repairs a
    /// missing ref; nothing repairs a different repository, whose remote and
    /// whose credential scope both belong to the first one.
    ///
    /// Asserted on the pure decision rather than through a live clone: the act
    /// needs a forge that serves git over HTTP, and the acceptance surface is
    /// where that lives. The decision is the part that was wrong.
    ///
    /// Reds on: dropping the repository check (row 4 stops refusing); dropping
    /// the base check (row 2) or the branch check (row 3); checking the base
    /// before the repository (row 5, which changes both, would fetch into the
    /// wrong clone instead of refusing); returning `Fetch` unconditionally
    /// (row 1).
    #[test]
    fn a_later_caller_is_reconciled_against_the_tuple_the_clone_was_opened_for() {
        let opened = OpenedFor {
            repo: "acme/index".to_string(),
            base: "main".to_string(),
            branch: "indexbot-claim-acme-widget".to_string(),
        };
        let cases = [
            (
                ("acme/index", "main", "indexbot-claim-acme-widget"),
                Reuse::Ready,
                "the same tuple costs nothing",
            ),
            (
                ("acme/index", "release", "indexbot-claim-acme-widget"),
                Reuse::Fetch,
                "a base the clone never fetched",
            ),
            (
                ("acme/index", "main", "indexbot-claim-acme-other"),
                Reuse::Fetch,
                "a branch the clone never fetched",
            ),
            (
                ("acme/other", "main", "indexbot-claim-acme-widget"),
                Reuse::Refuse,
                "a second repository one clone cannot serve",
            ),
            (
                ("acme/other", "release", "indexbot-claim-acme-other"),
                Reuse::Refuse,
                "a repository disagreement outranks a fetchable one",
            ),
        ];
        for ((repo, base, branch), expected, case) in cases {
            assert_eq!(
                opened.reuse_for(repo, base, branch),
                expected,
                "{case}: ({repo}, {base}, {branch}) against {opened:?}"
            );
        }
    }

    /// A branch name carrying `/` or `.` survives as **one** encoded segment.
    ///
    /// The recorded target is asserted rather than the outcome: the encoder is
    /// invisible to a result assertion, and a nested namespace makes the claim
    /// branch carry a separator under C-047's naming.
    #[tokio::test]
    async fn a_branch_name_with_separators_is_one_encoded_segment() {
        let env = isolated_env();
        let credentials = job_token_credentials(&env);
        let fake = FakeGitLab::start(|_, _| (404, json!({ "message": "404 Branch Not Found" }).to_string())).await;

        fake.forge(credentials)
            .get_ref_sha(&index_repo(), "heads/indexbot-claim-acme/platform-widget.io")
            .await
            .expect("a 404 is an answer");

        assert_eq!(
            fake.routes(),
            vec![
                "GET /projects/acme%2Findex/repository/branches\
                 ?search=%5Eindexbot-claim-acme%2Fplatform-widget%2Eio&per_page=100&page=1"
                    .to_string()
            ],
            "the branch name must not split into further URL segments"
        );
    }

    // ── C-031 transport dispatch ─────────────────────────────────────────

    /// The fork operations are refused under the git transport **before any
    /// request is issued**, and each names itself.
    ///
    /// The variant alone is not enough. `find_fork` resolves the upstream's
    /// project id first and `ensure_fork` reads the authenticated username
    /// first, so a guard placed after either spends a request and can fail with
    /// a *different* error — a status, or an unavailable users API under a job
    /// token — on a project the git credential cannot see. The route list is
    /// what sees that, and a variant-only assertion does not.
    ///
    /// The two operations carry **distinct** literals: one shared spelling
    /// means the message cannot say which call refused.
    #[tokio::test]
    async fn fork_ops_refused_under_git_before_any_request() {
        let env = isolated_env();
        let credentials = job_token_credentials(&env);
        let fake = FakeGitLab::start(|_, _| (200, pushable_project().to_string())).await;
        let upstream = coordinate("gitlab.com/acme/index");
        let fork = coordinate("gitlab.com/forkuser/index");

        let find = fake
            .git_forge(credentials.clone())
            .find_fork(&upstream, &fork)
            .await
            .expect_err("the git transport cannot reach the fork API");
        let ensure = fake
            .git_forge(credentials.clone())
            .ensure_fork(&upstream, Some("forkuser"))
            .await
            .expect_err("the git transport cannot reach the fork API");

        let operation = |error: &ForgeError| match error {
            ForgeError::TransportOperationUnsupported { operation, transport } => {
                assert_eq!(*transport, WriteTransport::Git, "the refusal names the transport");
                operation.clone()
            }
            other => panic!("expected a transport refusal, got {other:?}"),
        };
        let (find_operation, ensure_operation) = (operation(&find), operation(&ensure));
        assert_ne!(
            find_operation, ensure_operation,
            "one shared spelling leaves the caller unable to tell which call refused"
        );
        assert_eq!(
            fake.routes(),
            Vec::<String>::new(),
            "the refusal precedes every request"
        );

        // The control: the same fake, the same credentials, an operation that
        // is not refused — so an empty route list above means "nothing was
        // issued", not "the recorder never fires".
        fake.git_forge(credentials)
            .get_ref_sha(&index_repo(), "heads/main")
            .await
            .expect_err("the project body is not a branch document");
        assert_eq!(fake.routes().len(), 1, "the recorder fires when a request IS issued");
    }

    /// `sync_fork` is a no-op on GitLab, and the falsifiable half of that is
    /// that it issues no request.
    ///
    /// A function returning `()` is indistinguishable from a working
    /// implementation unless something is asserted, and the one `tracing::debug!`
    /// it emits is not worth a subscriber. The route list is the assertion; the
    /// log line is a review item and is deliberately **not** claimed as
    /// coverage.
    #[tokio::test]
    async fn sync_fork_issues_no_request_under_either_transport() {
        let env = isolated_env();
        let credentials = job_token_credentials(&env);
        let fake = FakeGitLab::start(|_, _| (200, pushable_project().to_string())).await;
        let fork = coordinate("gitlab.com/forkuser/index");

        fake.forge(credentials.clone()).sync_fork(&fork, "main").await;
        fake.git_forge(credentials.clone()).sync_fork(&fork, "main").await;
        assert_eq!(
            fake.routes(),
            Vec::<String>::new(),
            "GitLab names the base project on the commit, so there is no fork to sync"
        );

        // The control, as above: proof the recorder was live for both calls.
        fake.forge(credentials)
            .get_ref_sha(&index_repo(), "heads/main")
            .await
            .expect_err("the project body is not a branch document");
        assert_eq!(fake.routes().len(), 1, "the recorder fires when a request IS issued");
    }

    // ── C-031 transport dispatch ─────────────────────────────────────────

    /// A `git` binary that cannot be spawned, used as a **positive control**.
    ///
    /// Every test below asserts that some REST write did *not* happen under the
    /// git transport. On its own that is satisfiable by a client which does
    /// nothing at all, so each one also has to show the run reached the git
    /// half — and an unspawnable binary makes that observable as a distinct
    /// error rather than as silence. It is a name, not a path: an absent command
    /// fails to spawn identically on Unix and Windows.
    fn unspawnable_git_binary() -> GitBinary {
        GitBinary {
            path: std::path::PathBuf::from("ocx-git-must-not-exist-3f9a"),
            version: GitVersion::new(2, 47, 0),
        }
    }

    /// The commit base every dispatch test commits against.
    fn dispatch_base() -> RepoCoordinate {
        index_repo()
    }

    /// Whether any recorded route is a write.
    ///
    /// The whole method rather than a named endpoint: an implementation that
    /// routed the commit to `git` and left the merge-request `POST` behind — or
    /// the reverse — passes any single-endpoint assertion.
    fn writes(fake: &FakeGitLab) -> Vec<String> {
        fake.routes()
            .into_iter()
            .filter(|route| !route.starts_with("GET "))
            .collect()
    }

    /// C-001 on GitLab: a `Delete` is a `delete` action carrying the file's own
    /// `last_commit_id`, and a `Delete` of a path absent at the base contributes
    /// **no action**.
    ///
    /// The existence read this driver already spends to choose `create` versus
    /// `update` answers the removal question too, so the no-op costs nothing —
    /// and it is not optional: GitLab answers `delete` on a file it does not
    /// have with a 400 that fails the *whole* commit, taking the root and every
    /// CAS object with it.
    ///
    /// `last_commit_id` on the removal is the same C4 compare-and-swap a rewrite
    /// carries. Without it a concurrent announce that rewrote the object between
    /// our read and our commit is deleted anyway, silently.
    ///
    /// Both halves in one commit: the `delete` action alone passes for a build
    /// that emits one unconditionally, and the absent half alone passes for a
    /// build that drops every removal.
    ///
    /// Reds on: ignoring `FileChange::Delete`; emitting a `delete` for the
    /// absent path; dropping `last_commit_id` from the removal.
    #[tokio::test]
    async fn a_delete_is_a_delete_action_and_an_absent_path_contributes_none() {
        const PRESENT: &str = "p/acme/widget/o/sha256/present.json";
        const ABSENT: &str = "p/acme/widget/o/sha256/absent.json";
        const LAST_COMMIT: &str = "1a2b3c4d5e6f70819a2b3c4d5e6f70819a2b3c4d";

        // A file path travels as ONE percent-encoded segment, dots included, so
        // the fixture matches the encoding the client actually sends rather than
        // the plain path — a substring of the plain form matches nothing and
        // would answer 404 for every read, which is the shape that makes the
        // whole assertion below pass for the wrong reason.
        let present_segment = encode_segment(PRESENT);
        let fake = FakeGitLab::start(move |method, target| {
            if method == "POST" {
                return (201, json!({ "id": "newcommit" }).to_string());
            }
            if target.contains("/repository/branches") {
                // The LIST endpoint: an unmatched search is 200 with an empty
                // array, so the branch reads as absent and the commit starts at
                // the base.
                return (200, json!([]).to_string());
            }
            if target.contains("/repository/files/") {
                if target.contains(&present_segment) {
                    return (200, json!({ "last_commit_id": LAST_COMMIT }).to_string());
                }
                return (404, json!({ "message": "404 File Not Found" }).to_string());
            }
            (200, pushable_project().to_string())
        })
        .await;

        let repo = index_repo();
        let files = BTreeMap::from([
            ("p/acme/widget.json".to_string(), FileChange::Put(b"{}".to_vec())),
            (PRESENT.to_string(), FileChange::Delete),
            (ABSENT.to_string(), FileChange::Delete),
        ]);

        fake.forge(ForgeCredentials::new(ForgeToken::new("token".to_string())))
            .commit_files(
                &repo,
                "indexbot-announce-acme-widget",
                CommitBase {
                    repo: &repo,
                    sha: BRANCH_HEAD,
                    branch: "main",
                },
                "announce acme/widget",
                &files,
                RefUpdate::FastForward,
            )
            .await
            .expect("a removal of an absent path must not fail the commit");

        let body: Value = fake
            .recorded()
            .iter()
            .find(|call| call.method == "POST" && call.target.contains("/repository/commits"))
            .map(|call| serde_json::from_str(&call.body).expect("the commit body is JSON"))
            .expect("a commit was posted");
        let actions = body["actions"].as_array().expect("the actions are an array");

        assert_eq!(
            actions,
            &vec![
                json!({
                    "file_path": "p/acme/widget.json",
                    "content": BASE64_STANDARD.encode(b"{}"),
                    "encoding": "base64",
                    "action": "create",
                }),
                json!({ "file_path": PRESENT, "action": "delete", "last_commit_id": LAST_COMMIT }),
            ],
            "the committed orphan is removed under its own last commit, and the absent one is not named at all"
        );
    }

    /// Under `git`, a commit is built in the clone and **no** REST commit is
    /// posted.
    ///
    /// Reds on: deleting the transport arm from `commit_files`, which sends
    /// `POST /projects/42/repository/commits` and reports the fake's own commit
    /// id as success — a run that believes it committed while the git transport
    /// wrote nothing.
    #[tokio::test]
    async fn commit_files_under_git_posts_no_rest_commit() {
        let env = isolated_env();
        let credentials = job_token_credentials(&env);
        let fake = FakeGitLab::start(|method, target| {
            if method == "POST" {
                return (201, json!({ "id": "deadbeef" }).to_string());
            }
            if target.contains("/repository/branches?") {
                // The LIST endpoint: an unmatched search is 200 with an empty
                // array, never a 404. A 404 here would be the project answering,
                // not the branch.
                return (200, json!([]).to_string());
            }
            (200, pushable_project().to_string())
        })
        .await;

        let forge = GitLabForge::with_base_url(
            credentials,
            WriteTransport::Git,
            Some(unspawnable_git_binary()),
            fake.base_url.clone(),
        )
        .expect("client builds");
        let files = BTreeMap::from([("p/acme/widget.json".to_string(), FileChange::Put(b"{}".to_vec()))]);
        let error = forge
            .commit_files(
                &index_repo(),
                "indexbot-claim-acme-widget",
                CommitBase {
                    repo: &dispatch_base(),
                    sha: "0000000000000000000000000000000000000000",
                    branch: "main",
                },
                "claim acme/widget",
                &files,
                RefUpdate::FastForward,
            )
            .await
            .expect_err("the clone cannot be created with an unspawnable git");

        assert!(
            matches!(
                error,
                ForgeError::GitUnavailable { .. } | ForgeError::GitCommandFailed { .. }
            ),
            "the run must reach the git half rather than fall through to REST, got {error:?}"
        );
        assert_eq!(
            writes(&fake),
            Vec::<String>::new(),
            "the git transport commits locally; no REST write may be issued"
        );
        assert!(
            fake.routes()
                .iter()
                .any(|route| route.contains("/repository/branches?")),
            "the branch-existence read is the call that decides the fetch refspecs (C-036, DV-6)"
        );
    }

    /// Under `git`, an unchanged run whose merge request is already open
    /// returns it, writes nothing, and **never starts `git` at all** (C-042).
    ///
    /// The unspawnable binary is what makes the last clause an assertion rather
    /// than a hope: a build that opened the clone before reading fails here with
    /// a git error instead of returning the request.
    ///
    /// Reds on: deleting the transport arm (a `POST /merge_requests` appears);
    /// on moving the REST read after the clone (a git error replaces the
    /// request); and on treating "no pending commit" as "push anyway".
    #[tokio::test]
    async fn unchanged_path_with_an_open_request_pushes_nothing_under_git() {
        let env = isolated_env();
        let credentials = job_token_credentials(&env);
        let fake = FakeGitLab::start(|method, target| {
            if method == "POST" {
                return (
                    201,
                    json!({ "iid": 9, "web_url": "https://gitlab.example/mr/9" }).to_string(),
                );
            }
            if target.contains("/merge_requests") {
                return (
                    200,
                    json!([{
                        "iid": 4,
                        "web_url": "https://gitlab.example/mr/4",
                        // Opened from the index onto the index: the id-free
                        // spelling of `source_project_id=<this project>`, which
                        // is what keeps an unrelated fork's request on the same
                        // branch name from being adopted.
                        "source_project_id": 42,
                        "target_project_id": 42,
                    }])
                    .to_string(),
                );
            }
            (200, pushable_project().to_string())
        })
        .await;

        let forge = GitLabForge::with_base_url(
            credentials,
            WriteTransport::Git,
            Some(unspawnable_git_binary()),
            fake.base_url.clone(),
        )
        .expect("client builds");
        let request = forge
            .open_or_update_pull_request(
                &index_repo(),
                &index_repo(),
                "indexbot-claim-acme-widget",
                "main",
                "claim acme/widget",
                "body",
            )
            .await
            .expect("an already-open request is returned without any write");

        assert_eq!(
            request.number, 4,
            "the open request is reused, not a freshly-minted one"
        );
        assert_eq!(
            writes(&fake),
            Vec::<String>::new(),
            "C-042: no write of any kind on the unchanged path"
        );
        assert!(
            !fake.routes().iter().any(|route| route.contains("/repository/branches")),
            "no clone is opened, so the branch-existence read the clone needs is never issued"
        );
    }

    /// Under `git`, a run that will push is refused by the preflight **before
    /// any git process starts** — and posts no merge request either.
    ///
    /// This is the DX-35 thread made observable without a workable `git`: the
    /// preflight sits between the "already open" return and the clone, so a
    /// project whose job-token push setting reads `false` refuses at 86 with the
    /// clone never created.
    ///
    /// Reds on: deleting the transport arm (a `POST /merge_requests` appears);
    /// on running the preflight after the clone (a git error replaces the 86);
    /// and on substituting `PushAccess::skipped_all()` for a real preflight (no
    /// refusal at all — the run reaches the clone).
    #[tokio::test]
    async fn a_run_that_will_push_is_preflighted_before_any_git_process() {
        let env = isolated_env();
        let credentials = job_token_credentials(&env);
        let fake = FakeGitLab::start(|method, target| {
            if method == "POST" {
                return (
                    201,
                    json!({ "iid": 9, "web_url": "https://gitlab.example/mr/9" }).to_string(),
                );
            }
            if target.contains("/merge_requests") {
                return (200, json!([]).to_string());
            }
            let mut project = pushable_project();
            project[JOB_TOKEN_PUSH_FIELD] = json!(false);
            (200, project.to_string())
        })
        .await;

        let forge = GitLabForge::with_base_url(
            credentials,
            WriteTransport::Git,
            Some(unspawnable_git_binary()),
            fake.base_url.clone(),
        )
        .expect("client builds");
        let error = forge
            .open_or_update_pull_request(
                &index_repo(),
                &index_repo(),
                "indexbot-claim-acme-widget",
                "main",
                "claim acme/widget",
                "body",
            )
            .await
            .expect_err("a project with job-token push disabled is refused");

        assert!(
            matches!(
                error,
                ForgeError::WriteCapabilityUnavailable {
                    capability: CapabilityName::JobTokenPush,
                    ..
                }
            ),
            "the preflight refusal, not a git failure, is what the caller sees, got {error:?}"
        );
        assert_eq!(
            writes(&fake),
            Vec::<String>::new(),
            "a refused run posts no merge request"
        );
        assert!(
            !fake.routes().iter().any(|route| route.contains("/repository/branches")),
            "the refusal lands before the clone, so no branch-existence read is spent"
        );
    }

    /// Under `git`, the preflight already performed is the one the push is
    /// classified against — it is not run a second time (DX-35).
    ///
    /// The fake answers `ci_push_repository_for_job_token_allowed` **true on the
    /// first project read and false afterwards** — a value ocx must never see,
    /// because the preflight it acts on already happened. Reusing the record
    /// therefore ends at the clone; re-running it ends at 86.
    ///
    /// The count is a second, independent witness now that
    /// `find_open_pull_request` addresses the project by path rather than
    /// resolving its numeric id ([#429]): the only `GET /projects/:id` this run
    /// can make is a preflight, so "exactly one" says the record was reused. The
    /// control is a third `ensure_push_access` against the same fake, which must
    /// refuse — that is what proves the knob was armed rather than inert.
    ///
    /// Reds on: dropping the `git_run.preflight` record, and on substituting a
    /// fresh `ensure_push_access` for the recorded one in `push_preflight`.
    ///
    /// [#429]: https://github.com/ocx-sh/ocx/issues/429
    #[tokio::test]
    async fn the_recorded_preflight_is_reused_by_the_publish_path() {
        let env = isolated_env();
        let credentials = job_token_credentials(&env);
        let project_reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&project_reads);
        let fake = FakeGitLab::start(move |method, target| {
            if method == "POST" {
                return (
                    201,
                    json!({ "iid": 9, "web_url": "https://gitlab.example/mr/9" }).to_string(),
                );
            }
            if target.contains("/merge_requests") {
                return (200, json!([]).to_string());
            }
            if target.contains("/repository/branches?") {
                // The LIST endpoint: an unmatched search is 200 with an empty
                // array, never a 404. A 404 here would be the project answering,
                // not the branch.
                return (200, json!([]).to_string());
            }
            let first = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
            let mut project = pushable_project();
            project[JOB_TOKEN_PUSH_FIELD] = json!(first);
            (200, project.to_string())
        })
        .await;

        let forge = GitLabForge::with_base_url(
            credentials,
            WriteTransport::Git,
            Some(unspawnable_git_binary()),
            fake.base_url.clone(),
        )
        .expect("client builds");
        forge
            .ensure_push_access(&index_repo())
            .await
            .expect("a Developer with job-token push enabled passes");

        let error = forge
            .open_or_update_pull_request(
                &index_repo(),
                &index_repo(),
                "indexbot-claim-acme-widget",
                "main",
                "claim acme/widget",
                "body",
            )
            .await
            .expect_err("the clone cannot be created with an unspawnable git");

        assert!(
            matches!(
                error,
                ForgeError::GitUnavailable { .. } | ForgeError::GitCommandFailed { .. }
            ),
            "the recorded preflight must carry the push; a re-read would refuse at 86, got {error:?}"
        );
        assert_eq!(
            project_reads.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the recorded preflight is reused, so the project is read exactly once"
        );
        // The control. Without it the row above is equally green against a fake
        // whose knob never flips — "the run did not refuse" would then be a
        // statement about the fixture, not about the record.
        let refused = forge
            .ensure_push_access(&index_repo())
            .await
            .expect_err("the second project read answers false");
        assert!(
            matches!(refused, ForgeError::WriteCapabilityUnavailable { .. }),
            "the knob must be live, or reusing the record proves nothing: {refused:?}"
        );
        assert_eq!(
            writes(&fake),
            Vec::<String>::new(),
            "no REST write is issued on the git transport"
        );
    }

    /// Under `git`, `compare_branch` is computed from the clone and the compare
    /// endpoint is never read (C-031, C-037).
    ///
    /// Reds on: deleting the transport arm, which issues
    /// `GET /projects/42/repository/compare?...` and answers from the server.
    #[tokio::test]
    async fn compare_branch_under_git_never_reads_the_compare_endpoint() {
        let env = isolated_env();
        let credentials = job_token_credentials(&env);
        let fake = FakeGitLab::start(|_, target| {
            if target.contains("/repository/compare") {
                return (200, json!({ "commits": [] }).to_string());
            }
            if target.contains("/repository/branches?") {
                return (
                    200,
                    json!([{ "name": "indexbot-claim-acme-widget", "commit": { "id": BRANCH_HEAD } }]).to_string(),
                );
            }
            (200, pushable_project().to_string())
        })
        .await;

        let forge = GitLabForge::with_base_url(
            credentials,
            WriteTransport::Git,
            Some(unspawnable_git_binary()),
            fake.base_url.clone(),
        )
        .expect("client builds");
        let error = forge
            .compare_branch(&index_repo(), "main", &index_repo(), "indexbot-claim-acme-widget")
            .await
            .expect_err("the clone cannot be created with an unspawnable git");

        assert!(
            matches!(
                error,
                ForgeError::GitUnavailable { .. } | ForgeError::GitCommandFailed { .. }
            ),
            "the comparison must be attempted in the clone, got {error:?}"
        );
        assert!(
            !fake.routes().iter().any(|route| route.contains("/repository/compare")),
            "the compare endpoint is the REST arm's alone, got {:?}",
            fake.routes()
        );

        // The control: the same comparison under `api` DOES read it, so the
        // absence above is a dispatch decision rather than an unreachable route.
        fake.forge(job_token_credentials(&env))
            .compare_branch(&index_repo(), "main", &index_repo(), "indexbot-claim-acme-widget")
            .await
            .expect("the REST comparison answers from the fake");
        assert!(
            fake.routes().iter().any(|route| route.contains("/repository/compare")),
            "the api transport reads the compare endpoint"
        );
    }

    /// The remote a clone is taken from is the instance the REST half talks to,
    /// carries the full project path, and keeps its `.git` suffix.
    ///
    /// All three are C-034 preconditions rather than cosmetics: the credential
    /// scope is derived from this string, `CredentialScope::new` refuses one
    /// naming fewer than two path segments, and git matches `http.<url>.*`
    /// component-wise so a dropped `.git` stops the header applying at all.
    ///
    /// Reds on: building the remote from `RepoCoordinate::host` (the acceptance
    /// seam then dials gitlab.com), and on dropping the `.git` suffix.
    #[tokio::test]
    async fn the_git_remote_is_the_rest_instance_and_names_the_whole_project() {
        let env = isolated_env();
        let credentials = job_token_credentials(&env);
        let forge = GitLabForge::with_base_url(
            credentials,
            WriteTransport::Git,
            Some(unspawnable_git_binary()),
            "https://gitlab.example/api/v4".to_string(),
        )
        .expect("client builds");

        assert_eq!(
            forge.repository_url(&coordinate("gitlab.com/acme/platform/index")),
            "https://gitlab.example/acme/platform/index.git",
            "the API suffix is stripped, the nested path is kept whole, and `.git` stays"
        );
        super::super::git_workspace::credential_scope(&forge.repository_url(&index_repo()))
            .expect("a remote naming a project is an acceptable credential scope");
    }
}
