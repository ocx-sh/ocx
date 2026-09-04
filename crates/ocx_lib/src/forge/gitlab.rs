// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! GitLab REST v4 forge client.
//!
//! The second [`Forge`] implementation, holding the identical announce contract
//! against a very different API. Three differences shape the whole file:
//!
//! 1. **A commit is one request.** GitLab's commits API takes a batch of file
//!    actions, so the five-step blob/tree/commit/ref dance the GitHub client
//!    performs collapses into a single atomic POST (design register C15, more
//!    directly than on GitHub).
//! 2. **Concurrency is per file, not per ref.** GitLab has no compare-and-swap
//!    on a branch ref; it has `last_commit_id` on a file action, which refuses
//!    the commit when that file moved since the version being edited. Since every
//!    announce commit rewrites the package's root document, that guard is exactly
//!    the C4 protection [`RefUpdate::FastForward`] promises, expressed on the one
//!    file whose staleness matters. It is **not** a force-push: `grim`, the donor,
//!    force-pushes here and silently clobbers a concurrent announce.
//! 3. **Everything is addressed by project.** Paths are one percent-encoded `:id`
//!    segment (so nested groups need no special casing), numeric project ids are
//!    what cross-project operations name, and a fork's parent is verified by
//!    immutable id rather than by a path that a rename can change.
//!
//! The X5 invariants are re-proved here rather than inherited: no-redirect
//! client, credential in a header only, fork parent verified against the
//! upstream, identity read from response bodies, bounded readiness wait.

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

use super::error::status_detail;
use super::http::build_forge_http_client;
use super::identity::{fork_identity_from_path, verify_fork_namespace, verify_gitlab_fork};
use super::poll::{PollSchedule, backoff_delays};
use super::{
    BranchComparison, CommitBase, Forge, ForgeError, ForgeToken, ForkIdentity, Mergeability, PullRequest, RefUpdate,
    RepoCoordinate,
};

/// Canonical GitLab host — every other host is a self-managed instance.
const DEFAULT_HOST: &str = "gitlab.com";
/// JSON media type.
const ACCEPT_JSON: &str = "application/json";
/// Total per-request timeout for ordinary forge calls.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// The Developer access level — the lowest that may push a branch.
const ACCESS_LEVEL_DEVELOPER: u64 = 30;
/// Page size for fork enumeration, and the page ceiling that bounds it.
const FORKS_PER_PAGE: u32 = 100;
/// Pages of forks the 409-reuse path will walk before giving up. A user forks a
/// project once into their own namespace, and the listing is filtered to what
/// the credential owns, so the answer is on page one in every realistic case;
/// the ceiling exists so a pathological account cannot spin forever.
const FORKS_MAX_PAGES: u32 = 10;
/// Backoff delays before each replay of a failed commit.
const COMMIT_RETRY_DELAYS: [Duration; 3] = [Duration::from_secs(3), Duration::from_secs(9), Duration::from_secs(27)];

/// Everything that is not unreserved must be escaped in a path segment.
///
/// A project path (`group/subgroup/project`) and a file path (`p/acme/pkg.json`)
/// each travel as **one** URL segment, so their own slashes and dots must be
/// encoded — `NON_ALPHANUMERIC` minus the RFC 3986 unreserved marks, which is
/// stricter than necessary and therefore never wrong.
const SEGMENT: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'_').remove(b'~');

/// Percent-encode one path segment.
fn encode_segment(value: &str) -> String {
    utf8_percent_encode(value, SEGMENT).to_string()
}

/// GitLab REST v4 forge client.
pub struct GitLabForge {
    client: reqwest::Client,
    token: ForgeToken,
    base_url: String,
    /// `namespace/project` -> numeric project id.
    ///
    /// Cross-project operations (a fork's merge request, a commit based on the
    /// upstream) name projects by id, and a path lookup is a whole round trip.
    /// The mapping is immutable for the life of a run — a project's id never
    /// changes, only its path can — so caching it is safe and saves a request per
    /// repeat reference.
    project_ids: RwLock<HashMap<String, u64>>,
}

impl GitLabForge {
    /// Build a client for `host` — `None` for gitlab.com, `Some` for a
    /// self-managed instance.
    ///
    /// Unlike GitHub, both cases share one shape: the API always lives at
    /// `/api/v4` on the instance itself, so gitlab.com is not a special case
    /// beyond supplying the default hostname.
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
            project_ids: RwLock::new(HashMap::new()),
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }

    /// The project endpoint for a coordinate, addressed by its encoded path.
    fn project_url(&self, repo: &RepoCoordinate, suffix: &str) -> String {
        self.url(&format!("/projects/{}{suffix}", encode_segment(&repo.full_path())))
    }

    /// The project endpoint for a numeric id.
    fn project_id_url(&self, id: u64, suffix: &str) -> String {
        self.url(&format!("/projects/{id}{suffix}"))
    }

    /// An authorized request builder.
    ///
    /// GitLab reads the credential from `PRIVATE-TOKEN`, which accepts personal,
    /// project and group access tokens; `Authorization: Bearer` accepts only
    /// OAuth2 tokens, so it is the narrower choice, not the safer one. A CI job
    /// token is **not** an option here regardless of header: GitLab's job-token
    /// access table covers packages, releases, artifacts and environments, and
    /// lists none of repository files, commits, branches, merge requests or
    /// forking — every endpoint announce needs. It wants a real access token. The
    /// credential never enters a URL or a query string (design register X6). The
    /// header is omitted entirely when the token is empty (the tokenless `--out`
    /// path) so the request reads as unauthenticated rather than sending a
    /// rejected empty credential.
    fn request(&self, method: Method, url: &str) -> reqwest::RequestBuilder {
        let builder = self.client.request(method, url).header(ACCEPT, ACCEPT_JSON);
        if self.token.0.is_empty() {
            builder
        } else {
            builder.header("PRIVATE-TOKEN", self.token.0.as_str())
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

    /// A non-success status as an error, carrying GitLab's own reason.
    ///
    /// GitLab reports the cause in the body (`{"message": ...}` or
    /// `{"error": ...}`) and nowhere else. Keeping it is the difference between
    /// "HTTP 400" and "you are attempting to update a file that has changed".
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

    /// The project document for a coordinate, or `None` when it is not visible.
    async fn project(&self, repo: &RepoCoordinate) -> Result<Option<Value>, ForgeError> {
        self.get_json_optional(&self.project_url(repo, "")).await
    }

    /// The numeric project id for a coordinate, cached for the run.
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

    /// The authenticated account's username — the default fork namespace.
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

    /// The head commit of `branch`, or `None` when the branch does not exist.
    async fn branch_sha(&self, repo: &RepoCoordinate, branch: &str) -> Result<Option<String>, ForgeError> {
        let url = self.project_url(repo, &format!("/repository/branches/{}", encode_segment(branch)));
        let Some(body) = self.get_json_optional(&url).await? else {
            return Ok(None);
        };
        body.get("commit")
            .and_then(|commit| commit.get("id"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or(ForgeError::MissingField {
                url,
                field: "commit.id".to_string(),
            })
            .map(Some)
    }

    /// The commit that last touched `path` at `r#ref`, or `None` when the path
    /// does not exist there.
    ///
    /// This is the value a file action's `last_commit_id` must carry: GitLab
    /// compares it against the file's current last commit, **not** against the
    /// branch head, so passing a branch sha would reject every commit whose head
    /// did not happen to touch that exact file.
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

    /// How many commits `to` carries that `from` does not, comparing directly
    /// (never through a merge base).
    ///
    /// `from` is read in `from_project`, `to` in `project` — the parameter split
    /// that makes a fork-versus-upstream comparison possible at all.
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
        // `compare_timeout: true` means GitLab gave up part-way. Its docs say
        // `commits` stays complete even then and only `diffs` is truncated — so
        // refusing here is stricter than the documented contract requires, and
        // that is deliberate. The claim is unverified against a real timeout on a
        // large repository, the commit list is the ONLY input to the ahead/behind
        // verdict, and being wrong in the permissive direction classifies a live
        // branch as spent and force-rebuilds it, discarding unmerged work. A
        // hard error costs a retry; the other answer costs someone's commits.
        if body.get("compare_timeout").and_then(Value::as_bool) == Some(true) {
            return Err(ForgeError::UnknownCompareStatus {
                url,
                status: "compare_timeout".to_string(),
            });
        }
        // An absent or non-array `commits` is the same hazard one step earlier:
        // `map_or(0, …)` would read a response that cannot classify ancestry as
        // "no commits", and a `(0, 0)` verdict reads as `Identical`, which is
        // what condemns a live branch to be force-rebuilt. A comparison that
        // did not come back in the documented shape is refused, not counted.
        body.get("commits")
            .and_then(Value::as_array)
            .map(Vec::len)
            .ok_or_else(|| ForgeError::MissingField {
                url,
                field: "commits".to_string(),
            })
    }

    /// Bounded readiness wait on a freshly created fork.
    ///
    /// GitLab imports a fork in a background job and reports progress on the
    /// project's `import_status`. A `failed` import is terminal, so it fails fast
    /// rather than burning the whole deadline on a state that can never change.
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

    /// One readiness probe: `Ok(true)` when the import finished, `Ok(false)` when
    /// it is still running or the probe did not answer, `Err` when it failed
    /// terminally.
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
            // `none` is what a project that was never imported reports, and
            // `finished` what a completed import reports. Both mean ready.
            Some("finished" | "none") | None => Ok(true),
            Some("failed") => Err(ForgeError::ForkNotReady { deadline_secs: 0 }),
            Some(_) => Ok(false),
        }
    }

    /// Find the authenticated account's own fork of `upstream_id` by enumerating
    /// the upstream's forks — never by guessing a path.
    ///
    /// The 409 reuse path. A fork that was renamed, or created concurrently under
    /// a path the caller did not predict, is found here and nowhere else; the
    /// listing is the authoritative answer to "where is my fork", and a
    /// `{username}/{basename}` guess is not.
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
                    // The listing is scoped to forks OF the upstream, but the
                    // parent guard is re-run from the project's own document
                    // rather than trusted from the listing's context.
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

    /// One attempt at the single-request atomic commit.
    async fn commit_files_once(
        &self,
        repo: &RepoCoordinate,
        branch: &str,
        base: CommitBase<'_>,
        message: &str,
        files: &BTreeMap<String, Vec<u8>>,
        update: RefUpdate,
    ) -> Result<String, ForgeError> {
        let target_id = self.project_id(repo).await?;
        let base_id = self.project_id(base.repo).await?;
        let branch_exists = self.branch_sha(repo, branch).await?.is_some();

        let mut actions = Vec::with_capacity(files.len());
        for (path, contents) in files {
            // Whether a path already exists decides `create` versus `update`;
            // GitLab has no upsert, and guessing wrong fails the whole commit.
            // The question is asked at the BASE, because that is the tree the new
            // commit is built on.
            let existing = self.file_last_commit(base_id, path, base.sha).await?;
            let mut action = json!({
                "file_path": path,
                "content": BASE64_STANDARD.encode(contents),
                "encoding": "base64",
            });
            match existing {
                Some((last_commit_id, _)) => {
                    action["action"] = json!("update");
                    // The C4 compare-and-swap. Omitting this is what turns a
                    // concurrent announce into a silent overwrite.
                    action["last_commit_id"] = json!(last_commit_id);
                }
                None => action["action"] = json!("create"),
            }
            actions.push(action);
        }

        let mut body = json!({
            "branch": branch,
            "commit_message": message,
            "actions": actions,
        });
        // `start_*` names where the branch begins. It is sent when the branch does
        // not exist yet (create it at the base) and when a spent branch is being
        // rebuilt (`Reset`), and NOT when accumulating onto a live branch, where
        // GitLab would refuse it as "branch already exists".
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
        // The trait speaks git ref paths; GitLab's branches endpoint takes a bare
        // branch name. Only `heads/` is ever asked for by announce.
        let branch = r#ref.strip_prefix("heads/").unwrap_or(r#ref);
        self.branch_sha(repo, branch).await
    }

    async fn compare_branch(
        &self,
        repo: &RepoCoordinate,
        base: &str,
        head: &RepoCoordinate,
        head_branch: &str,
    ) -> Result<BranchComparison, ForgeError> {
        let base_id = self.project_id(repo).await?;
        let head_id = self.project_id(head).await?;
        // GitLab has no single "ahead/behind/diverged" verdict the way GitHub's
        // compare does, so it is derived from the two directed comparisons. Both
        // are needed: one alone cannot tell `Ahead` from `Diverged`, and reading
        // a diverged branch as ahead is what re-proposes squash-merged work.
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
        let index_id = self.project_id(index).await?;
        let head_id = self.project_id(head).await?;
        // A merge request is listed on its TARGET project; `source_project_id`
        // narrows it to the one opened from this fork, so an unrelated fork
        // proposing the same deterministic branch name is never adopted.
        let url = self.project_id_url(
            index_id,
            &format!(
                "/merge_requests?state=opened&source_branch={}&source_project_id={head_id}",
                encode_segment(branch)
            ),
        );
        let (status, body) = self.send(self.request(Method::GET, &url), &url).await?;
        if !status.is_success() {
            return Err(self.status_error(&url, status, &body));
        }
        let list = Self::parse_json(&url, &body)?;
        let Some(existing) = list.as_array().and_then(|requests| requests.first()) else {
            return Ok(None);
        };
        merge_request_from_body(&url, existing, true).map(Some)
    }

    /// Whether merge request `number` (an `iid`, project-local to `index`)
    /// merges cleanly.
    ///
    /// GitLab computes mergeability asynchronously, so the single-request GET
    /// can legitimately answer "still checking"; that is
    /// [`Mergeability::Unknown`], never a guess, and never a poll.
    ///
    /// # Errors
    ///
    /// Returns a [`ForgeError`] on transport failure, a project that is not
    /// visible to the credential, or a non-success status other than 404.
    async fn pull_request_mergeability(&self, index: &RepoCoordinate, number: u64) -> Result<Mergeability, ForgeError> {
        let index_id = self.project_id(index).await?;
        let url = self.project_id_url(index_id, &format!("/merge_requests/{number}"));
        let Some(body) = self.get_json_optional(&url).await? else {
            return Ok(Mergeability::Unknown);
        };
        Ok(mergeability_from_merge_request(&body))
    }

    async fn find_fork(
        &self,
        upstream: &RepoCoordinate,
        fork: &RepoCoordinate,
    ) -> Result<Option<ForkIdentity>, ForgeError> {
        let upstream_id = self.project_id(upstream).await?;
        let Some(project) = self.project(fork).await? else {
            return Ok(None);
        };
        // A same-named stranger project is "no fork here", not a hard error: the
        // create path is what refuses it (X5).
        let Ok(identity) = verify_gitlab_fork(&project, upstream_id) else {
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
            None => self.authenticated_username().await?,
        };
        // A namespace cannot fork a project it already owns; GitLab answers that
        // POST with a 409 whose reuse path would then hunt for a fork-of-itself
        // that cannot exist, spending the whole enumeration budget to fail.
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
            // The fork already exists somewhere the conventional path did not
            // predict — renamed, or created concurrently. Enumerate rather than
            // guess (the donor's basename guess fails exactly here).
            self.find_owned_fork(upstream_id, &expected_namespace)
                .await?
                .ok_or_else(|| self.status_error(&url, status, &response))?
        } else {
            return Err(self.status_error(&url, status, &response));
        };
        verify_fork_namespace(&identity, &expected_namespace)?;
        // Every GitLab project document carries `id`, so an absent one means the
        // response was not the document it claimed to be. Skipping the readiness
        // wait on that basis would be a green that never ran: the announce would
        // commit into a fork whose import may still be in flight, and the
        // failure would surface later as an unexplained 404.
        let id = identity.id.ok_or_else(|| ForgeError::MissingField {
            url: url.clone(),
            field: "id".to_string(),
        })?;
        self.wait_fork_ready(id).await?;
        Ok(identity)
    }

    /// Nothing to do on GitLab.
    ///
    /// The GitHub client syncs a fork so that a base commit read from the
    /// upstream is reachable when the commit is written to the fork. GitLab's
    /// commits API takes `start_project`, so the base is named explicitly and
    /// reachability is the server's problem — there is no stale-fork hazard to
    /// pre-empt, and syncing the fork's default branch would only rewrite history
    /// the announce never reads.
    async fn sync_fork(&self, fork: &RepoCoordinate, branch: &str) {
        tracing::debug!(
            fork = %fork.full_path(),
            branch,
            "fork sync is a no-op on GitLab: the commit names its start project"
        );
    }

    async fn ensure_push_access(&self, repo: &RepoCoordinate) -> Result<(), ForgeError> {
        // `permissions` is only present on an authenticated read, and a project
        // the credential cannot see answers 404 rather than 403 — both mean the
        // same thing to the caller, so both land on the same error.
        let access = self
            .project(repo)
            .await?
            .and_then(|body| {
                let permissions = body.get("permissions")?.clone();
                let level = |key: &str| {
                    permissions
                        .get(key)
                        .and_then(|access| access.get("access_level"))
                        .and_then(Value::as_u64)
                };
                Some(
                    level("project_access")
                        .unwrap_or(0)
                        .max(level("group_access").unwrap_or(0)),
                )
            })
            .unwrap_or(0);
        if access >= ACCESS_LEVEL_DEVELOPER {
            return Ok(());
        }
        Err(ForgeError::PushAccessDenied { repo: repo.full_path() })
    }

    async fn commit_files(
        &self,
        repo: &RepoCoordinate,
        branch: &str,
        base: CommitBase<'_>,
        message: &str,
        files: &BTreeMap<String, Vec<u8>>,
        update: RefUpdate,
    ) -> Result<String, ForgeError> {
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

    async fn open_or_update_pull_request(
        &self,
        index: &RepoCoordinate,
        head: &RepoCoordinate,
        branch: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<PullRequest, ForgeError> {
        let index_id = self.project_id(index).await?;
        let head_id = self.project_id(head).await?;
        // A cross-project merge request is created on the SOURCE project and
        // names its target by id — the mirror image of GitHub, where it is
        // created on the target and names its source as `owner:branch`.
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
        // GitLab answers "an open merge request already exists for this source
        // branch" with 409, and 400 when the branch has nothing to propose. Only
        // the former may fall through to reuse; treating the latter as reuse
        // would report a missing merge request instead of the real reason.
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

/// The REST base URL for a GitLab host — always `/api/v4` on the instance.
///
/// Always https: the credential rides these requests as a header, and a
/// plaintext scheme would put it on the wire.
fn api_base_url(host: Option<&str>) -> String {
    format!("https://{}/api/v4", host.unwrap_or(DEFAULT_HOST))
}

/// Whether a failed commit means the base moved under it.
///
/// GitLab reports a stale `last_commit_id` as a 400 whose body names the
/// condition, and a lost branch-create race as a 400 saying the branch already
/// exists. Both are the C4 compare-and-swap firing, and both are answered by the
/// caller re-reading the winning head and regenerating — never by a blind replay
/// of the same commit, which would either fail identically or, with `force`,
/// discard the concurrent announce.
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

/// Whether a failed merge-request create means one is already open.
fn merge_request_already_exists(body: &[u8]) -> bool {
    String::from_utf8_lossy(body).to_lowercase().contains("already exists")
}

/// Whether a failed commit is worth replaying.
///
/// A 429 or a 5xx is throttling or a server-side fault; the commit is atomic and
/// content is addressed by path, so a repeat is safe. A 400 is never replayed:
/// GitLab spends it on genuine validation failure as well as on the stale-base
/// case, which is classified before this is reached.
fn is_retryable(error: &ForgeError) -> bool {
    matches!(
        error,
        ForgeError::Status { status, .. }
            if *status == StatusCode::TOO_MANY_REQUESTS.as_u16() || (500..600).contains(status)
    )
}

/// Build a [`PullRequest`] from a merge-request response body.
///
/// `iid`, not `id`: the `iid` is the per-project number a human sees in the UI
/// and in `!123` references, while `id` is a global database key that means
/// nothing to a reader. Reporting the wrong one sends people to an unrelated
/// merge request.
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

/// A merge-request body's merge status as a [`Mergeability`].
///
/// Two fields answer overlapping questions, and only their intersection is
/// trustworthy. `has_conflicts` is the direct one and decides first;
/// `detailed_merge_status` also spells a conflict, and is read as a fallback so
/// a response that carries only one of the pair still lands on the same
/// verdict.
///
/// `broken_status` — GitLab's "can not merge the source into the target
/// branch, potential conflict" — is a conflict for this detector's purpose: the
/// contract is that an unchanged run never reports success over a request that
/// cannot merge, and a benign `Unknown` here would do exactly that. The
/// no-verdict states are only the two *in-progress* values. Every
/// other value — a failed pipeline, a missing approval, a draft — is a reason
/// the request cannot merge *right now*, which is not the
/// base-moved-under-the-branch conflict this detector exists to catch, so it
/// reads as mergeable rather than raising an error the caller cannot act on. An
/// absent field is the older API shape and carries no conflict either.
fn mergeability_from_merge_request(value: &Value) -> Mergeability {
    if value.get("has_conflicts").and_then(Value::as_bool) == Some(true) {
        return Mergeability::Conflicting;
    }
    match value.get("detailed_merge_status").and_then(Value::as_str) {
        Some("conflict" | "broken_status") => Mergeability::Conflicting,
        Some("checking" | "unchecked") => Mergeability::Unknown,
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
    use super::*;

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
}
