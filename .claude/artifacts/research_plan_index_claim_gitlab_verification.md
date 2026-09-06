# Research: GitLab job-token, push-option and partial-clone verification

**Date:** 2026-09-05
**Expires:** 2027-03-05
**Axis:** domain
**Consumer:** plan_index_claim_command.md

## Summary

- **The async-post-receive claim (D-T4's whole justification) is now CONFIRMED at the source-code level, not just cited from docs/issues.** `PostReceiveService#execute` — called synchronously from the internal API `post_receive` endpoint that `gitlab-shell` hits at the end of every `git push` — calls `Repositories::PostReceiveWorker.perform_async(...)`, a genuine Sidekiq enqueue. Push-option handling (including `MergeRequests::CreateService`) runs inside that worker, later and independently. A push can return to the client before the merge request exists. This is stronger evidence than the ADR cites.
- **The "unresolved" partial-clone gate is over-stated as a live risk.** The three Gitaly issues the ADR cites ([gitaly#2510](https://gitlab.com/gitlab-org/gitaly/-/issues/2510), [gitaly#2553](https://gitlab.com/gitlab-org/gitaly/-/issues/2553), [gitaly#1553](https://gitlab.com/gitlab-org/gitaly/-/issues/1553)) are **all closed**, dated 2019–2020, and record `uploadpack.allowFilter`/blob-filter partial clone being wired up and then defaulted to enabled roughly around GitLab 12.10–13.0. GitLab's own maintenance policy supports only the three most recent minor releases (currently 19.1–19.3) — six years newer than that default flip. No evidence of any *currently supported* self-managed version shipping with the filter off or silently ignoring it was found. Recommend downgrading this from "named release gate, unresolved" to "verify once on the reporter's live 19.3 instance, expect a pass."
- **Re-push idempotency is confirmed at the source level, not just from docs.** `MergeRequests::PushOptionsHandlerService#execute_for_branch` looks up an existing MR for the branch first; if found, it calls `update!` (which still applies `.title`/`.description`) instead of `create!`. `.create` on a branch with an open MR is a true no-op with respect to creating a duplicate, and `.title`/`.description` unconditionally flow into either the create or the update path. This directly underwrites D-C6.
- **The push-option universe is fully anchored.** Current GitLab docs (`doc/topics/git/commit.md`) list exactly 15 `merge_request.*` push options. Both options the design forbids for governance reasons — `merge_request.merge_when_pipeline_succeeds` (deprecated 17.11 in favor of `auto_merge`, but still present/functional) and `merge_request.remove_source_branch` — are confirmed live options today.
- **Job-token REST capability matrix confirmed, endpoint-by-endpoint, from the primary doc's own table.** The docs page enumerates accessible endpoints per resource as an explicit allowlist; Users API, the compare endpoint, and any `POST` on Merge Requests are absent from that list. A live GitLab feature request, [gitlab-org/gitlab#545150](https://gitlab.com/gitlab-org/gitlab/-/issues/545150), is *still open*, independently confirming job tokens cannot touch the Merge Requests API beyond the two documented `GET`s.
- **Nothing found that changes the design's decisions.** Current GitLab release is **19.3** (2026-08-20). Versions after 18.4 add: JWT-format-by-default (19.0), cross-project job-token push GA (19.1, already cited in the ADR), Badges API job-token access (19.1), and CI_JOB_TOKEN repository-archive access. None of this touches the four decisions (D-T4/T5/T9/T10) or the closed push-option set.
- **GitHub blobless-clone support for `ocx-sh/index` is confirmed**, but measuring against it proves only GitHub's server behavior, never GitLab's — the ADR's own caveat holds and is repeated below with the concrete recipe.
- **`git rev-list --objects --missing=print`** is git's own documented mechanism for proving a filter excluded objects (git-scm.com/docs/partial-clone), matching exactly what the ADR's Validation section asks a test to assert. The pack-protocol "filter" capability line is a second, independent confirmation available via `GIT_TRACE_PACKET=1`.

## Verdict table

| ADR claim | Status | Evidence |
|---|---|---|
| Job token: reads branches, commits, raw files, tags, MRs (read-only); cannot compare, write, fork, create MR | **Confirmed** | `doc/ci/jobs/ci_job_token.md` master, full endpoint table (below); [gitlab-org/gitlab#545150](https://gitlab.com/gitlab-org/gitlab/-/issues/545150) open, confirming MR-API gap persists |
| Job token cannot reach Users API (`/user`, `/users?username=`) | **Confirmed by omission** | Users API absent from the doc's exhaustive per-resource allowlist table; no GitLab doc or issue found granting it |
| Job token cannot call the compare endpoint | **Confirmed by omission** | Compare/Repository-compare API absent from the same table |
| Job token cannot create a merge request over REST | **Confirmed** | Table lists only the two `GET` MR endpoints; open feature request #545150 |
| Job-token `git push` GA in 18.4, setting `ci_push_repository_for_job_token_allowed` | **Confirmed** | `doc/ci/jobs/ci_job_token.md`: "Generally available in GitLab 18.4. Feature flag `allow_push_repository_for_job_token` removed." Field is a `boolean` on `GET /projects/:id` |
| Job-token allowlist endpoint `GET /projects/:id/job_token_scope/allowlist`, cross-project meaning | **Confirmed** | `docs.gitlab.com/api/project_job_token_scopes/`: "Lists all projects in the CI/CD job token allowlist"; Maintainer/Owner role required |
| Cross-project job-token push GA 19.1 | **Confirmed** | `doc/ci/jobs/ci_job_token.md`: "Generally available in GitLab 19.1. Feature flag `allow_push_to_allowlisted_projects` removed." |
| GitLab processes `merge_request.*` push options in an asynchronous post-receive worker | **Confirmed — strengthened** | Source-level trace: `PostReceiveService#execute` → `Repositories::PostReceiveWorker.perform_async` (Sidekiq); MR creation happens inside that worker, not inline with the push |
| Re-pushing `merge_request.create` to a branch with an open MR is a no-op; `.title`/`.description` update the existing MR | **Confirmed** | `MergeRequests::PushOptionsHandlerService#execute_for_branch`: finds existing MR → `update!`, else `create!`; title/description params flow into both paths |
| `merge_request.merge_when_pipeline_succeeds` and `merge_request.remove_source_branch` exist as supported push options | **Confirmed** | `doc/topics/git/commit.md` full table (15 rows), quoted below |
| Full push-option universe is exactly 15 keys (as of 2026-09) | **Refined — now enumerated** | See Findings §4 for the full table; the ADR's own "closed set of four" is a chosen subset, not a claim about the universe's size, so this is additive, not a correction |
| `uploadpack.allowFilter` / partial clone self-managed support is an open, unresolved risk | **Refuted as "unresolved"; refined to "resolved, verify once"** | All three cited Gitaly issues are closed 2019–2020; feature defaulted-on generation predates every currently-supported self-managed minor version (19.1–19.3) by ~6 years |
| GitHub supports `--filter=blob:none` (blobless clone) | **Confirmed** | GitHub Blog: "On github.com and GitHub Enterprise Server 2.22+, blobless clones... are available" |
| Measuring against `ocx-sh/index` (GitHub) does not prove GitLab behavior | **Confirmed, restated** | Distinct server implementations (GitHub Git infra vs. Gitaly); no primary source claims parity |

## Findings

### 1. Job token capability matrix, current

The canonical, current source is `doc/ci/jobs/ci_job_token.md` on `gitlab-org/gitlab@master` (rendered at [docs.gitlab.com/ci/jobs/ci_job_token/](https://docs.gitlab.com/ci/jobs/ci_job_token/)). The page is structured as an explicit **allowlist**: it enumerates, per API resource, exactly which endpoints a job token may call. Anything not named is refused. Verbatim table (fetched 2026-09-05):

| Resource | Accessible endpoints |
|---|---|
| Badges API | "Can access all endpoints in this API." |
| Branches API | `GET /projects/:id/repository/branches` |
| Commits API | `GET /projects/:id/repository/commits/:sha`, `.../merge_requests`, `.../refs` |
| Container registry | Used as `$CI_REGISTRY_PASSWORD` |
| Package registry | Authentication only |
| Terraform module registry | Authentication only |
| Secure files | Used by `glab securefile` |
| Container registry API | Scoped to the job's own project registry |
| Deployments API | All endpoints |
| Environments API | All endpoints |
| Files API | `GET /projects/:id/repository/files/:file_path/raw` |
| Jobs API | `GET /job` only |
| Job artifacts API | Download endpoints only |
| **Merge requests API** | `GET /projects/:id/merge_requests`, `GET /projects/:id/merge_requests/:merge_request_iid` — **no others** |
| Notes API | `GET .../notes`, `GET .../notes/:note_id` |
| Packages API | All endpoints |
| Pipeline trigger tokens API | `POST /projects/:id/trigger/pipeline` only |
| Pipelines API | `PUT /projects/:id/pipelines/:pipeline_id/metadata` only |
| Release links API | All endpoints |
| Releases API | All endpoints |
| Repositories API | `GET /projects/:id/repository/archive`, `GET /projects/:id/repository/changelog` |
| Tags API | `GET /projects/:id/repository/tags`, `GET /projects/:id/repository/tags/:tag_name` |

**Per-endpoint verdict:**

- Branches, commits, raw files, tags, merge-requests (read): **allowed**, exactly as the ADR states.
- **Create a merge request over REST**: **refused.** Only the two `GET` MR endpoints are listed; nothing under Merge Requests API grants `POST`. Independently confirmed live: [gitlab-org/gitlab#545150 "Add support for job token to access merge request endpoints"](https://gitlab.com/gitlab-org/gitlab/-/issues/545150) is an **open** feature request as of this research, meaning the gap the ADR relies on has not closed.
- **Compare endpoint** (`GET /projects/:id/repository/compare`): **refused** — not present anywhere in the table, and no separate "Repository Compare API" row exists in the doc at all under the accessible list.
- **Users API** (`GET /user`, `GET /users?username=`): **refused** — absent from the table; a GitLab forum thread and a python-gitlab issue both report `401`/`403` when attempting Users-API calls with `CI_JOB_TOKEN`, consistent with the doc's omission.
- Everything else the ADR did not ask about (Jobs, Pipelines, Packages, etc.) is either fully open or narrowly scoped as shown — no surprises relevant to this design.

### 2. Job-token `git push` — GA version, setting, allowlist endpoint

- **GA version confirmed: 18.4.** `doc/ci/jobs/ci_job_token.md`: *"Generally available in GitLab 18.4. Feature flag `allow_push_repository_for_job_token` removed."* Matches the ADR exactly.
- **Setting name confirmed:** `ci_push_repository_for_job_token_allowed`, a **boolean** field on the `GET /projects/:id` response ([docs.gitlab.com/api/projects/](https://docs.gitlab.com/api/projects/)), description: *"Whether pushing to the repository is allowed using a job token."*
- **UI path confirmed:** Project → **Settings > CI/CD**, expand **Job token permissions**, then in the Permissions section select **"Allow Git push requests to the repository."**
- **Allowlist endpoint confirmed:** `GET /projects/:id/job_token_scope/allowlist` — *"Lists all projects in the CI/CD job token allowlist of a specified project."* Requires Maintainer or Owner role. "Cross-project" here means: which *other* projects' job tokens may authenticate against *this* project (inbound scope) — the allowlist is the mechanism that authorizes a job running in project A to push/read against project B.
- **Cross-project push GA: 19.1**, confirmed: *"Generally available in GitLab 19.1. Feature flag `allow_push_to_allowlisted_projects` removed."*
- **Current GitLab release: 19.3** (released 2026-08-20; end-of-life 2026-11-19). Supported minor versions at time of writing: 19.1, 19.2, 19.3 (bug fixes only on 19.3, security fixes on all three — [docs.gitlab.com/policy/maintenance/](https://docs.gitlab.com/policy/maintenance/)).
- **What changed after 18.4 that's relevant:**
  - **19.0** — JWT-format job tokens become the default (was previously opt-in/feature-flagged in some deployments). Does not change any capability the ADR relies on.
  - **19.1** — cross-project job-token push GA (already in the ADR's D-T9); Badges API job-token access added.
  - **19.3** — `CI_JOB_TOKEN` gained the ability to fetch repository archives (Repositories API row above), unblocking private Composer package downloads. Irrelevant to the claim/announce path.
  - **19.4** (documented ahead of release, not yet GA at time of writing) — planned addition: permission to list all refs a commit was pushed to (Commits API). Does not affect this design.
  - No change found to the `merge_request.*` push-option surface, to job-token MR-creation refusal, or to the async post-receive worker architecture.

### 3. Push options and asynchronous merge-request creation

**Confirmed at the source-code level — stronger than the ADR's citation.** Traced the full call chain on `gitlab-org/gitlab@master`:

1. `lib/api/internal/base.rb`'s `post_receive` endpoint (hit by `gitlab-shell` at the tail end of every `git push`) calls `PostReceiveService.new(...).execute` **synchronously** and returns its response — this is what makes the git push itself return promptly.
2. `PostReceiveService#execute` → private `schedule_post_receive_worker`, which calls:
   ```ruby
   worker.perform_async(params[:gl_repository], params[:identifier],
     params[:changes], push_options.as_json, worker_params)
   ```
   where `worker` is `Repositories::PostReceiveWorker`. **`perform_async` is a genuine Sidekiq enqueue** — it returns immediately; the job runs whenever a Sidekiq process picks it up.
3. `Repositories::PostReceiveWorker` (an `ApplicationWorker`, confirmed via `include ApplicationWorker`, `sidekiq_options retry: 3`) is the job that actually processes push options, eventually reaching `MergeRequests::PushOptionsHandlerService`.
4. `MergeRequests::PushOptionsHandlerService#create!` calls `::MergeRequests::CreateService.new(...).execute` **synchronously within that worker** — so MR creation itself is not further deferred once the worker runs, but the worker's *scheduling* relative to the push's HTTP/SSH response is genuinely asynchronous and unbounded by anything the client can see.

**Conclusion: the git push can complete (ref updated, response sent to the client) before the merge request exists.** An immediate `GET /merge_requests?source_branch=` is a real race, not a theoretical one — this directly underwrites D-T4's bounded-poll design and the exit-75 path. This is a stronger, source-level confirmation than the docs/issue citations the ADR uses (`gitlab-ce#21451`, `gitlab#441944`), which only speak to the `remote:` line's instability, not to the scheduling mechanism itself.

**Re-push idempotency, also confirmed at the source level.** `MergeRequests::PushOptionsHandlerService#execute_for_branch`:
```ruby
def execute_for_branch(branch)
  merge_request = merge_requests[branch]
  if merge_request
    update!(merge_request)
  else
    create!(branch)
  end
end
```
An open MR already existing for the branch routes to `update!`, not `create!` — so `.create` is a true no-op with respect to duplication (GitLab does not error, and does not create a second MR). `title`/`description` push-option values flow into `base_params`, which both `create!` and `update!` consume — so **yes**, `.title`/`.description` update an existing open MR exactly as D-C6 requires, with no extra call needed.

### 4. The closed push-option set

Full, current `merge_request.*` push-option table, from `doc/topics/git/commit.md` on `gitlab-org/gitlab@master` (fetched 2026-09-05) — **15 options**:

| Push option | Notes |
|---|---|
| `merge_request.create` | Creates MR for pushed branch; needs `.target` from the default branch |
| `merge_request.target=<branch>` | Sets target branch |
| `merge_request.target_project=<project>` | Sets target upstream project |
| `merge_request.merge_when_pipeline_succeeds` | **Deprecated in 17.11** in favor of `auto_merge`, but still present/functional |
| `merge_request.auto_merge` | Successor to the above |
| `merge_request.remove_source_branch` | Removes source branch on merge |
| `merge_request.squash` | Introduced in 17.2 |
| `merge_request.title="<title>"` | — |
| `merge_request.description="<description>"` | — |
| `merge_request.draft` | Marks MR draft |
| `merge_request.milestone="<milestone>"` | — |
| `merge_request.label="<label>"` | Repeatable; creates label if absent |
| `merge_request.unlabel="<label>"` | Repeatable |
| `merge_request.assign="<user>"` | Repeatable; username or ID |
| `merge_request.unassign="<user>"` | Repeatable |

**Both governance-relevant options exist and are live:** `merge_request.merge_when_pipeline_succeeds` (deprecated but not removed — still functional, redirects effectively to auto-merge behavior) and `merge_request.remove_source_branch` (not deprecated at all). The ADR's four-key closed set (`create`/`target`/`title`/`description`) is a **deliberate subset of 15**, not a claim that only four exist — the "exactly four, no fifth" fixture assertion is correctly anchored against this known, larger universe. No other GitLab-recognized `merge_request.*` key exists outside this table as of 2026-09.

### 5. Partial clone on self-managed GitLab

**The three Gitaly issues the ADR cites are all closed, and all date to 2019–2020:**

| Issue | Title | Opened | Closed |
|---|---|---|---|
| [gitaly#1553](https://gitlab.com/gitlab-org/gitaly/-/issues/1553) | "Allow upload pack filters" | 2019-03-18 | 2019-10-01 |
| [gitaly#2510](https://gitlab.com/gitlab-org/gitaly/-/issues/2510) | "Enabling the gitaly_upload_pack_filter feature flag isn't enough for Partial clone" | 2020-02-28 | 2020-03-17 |
| [gitaly#2553](https://gitlab.com/gitlab-org/gitaly/-/issues/2553) | "Enable partial clone with blob size filter by default" | 2020-03-18 | 2020-05-05 |

gitaly#2553's own description states the rationale for defaulting blob filters on: *"we should enable blob filters by default since they are low risk and make it easy for customers to try out partial clone,"* proposing to flip the existing `gitaly_upload_pack_filter` flag to enabled-by-default (with a *separate*, still-off flag reserved for the riskier sparse/path filters). Its closure in May 2020 is consistent with that flip having shipped around GitLab 12.10–13.0.

**GitLab's own maintenance policy** ([docs.gitlab.com/policy/maintenance/](https://docs.gitlab.com/policy/maintenance/)) supports only the **three most recent minor releases** at any time — currently 19.1, 19.2, 19.3. That is roughly six years and ~130 minor releases past the point where blob-filter partial clone defaulted on. No source was found — issue, changelog, or doc — describing any regression of that default, any self-managed deployment mode that ships it off, or any case of GitLab silently ignoring an unsupported filter (git's own behavior for an *unsupported* server is a loud client-side **warning**, not silence — see gitaly#1553: *"clients receive a 'filtering not recognized by server' warning"* — so even a hypothetical old/misconfigured instance fails loudly, not silently).

**Verdict: downgrade from "unresolved release gate" to "verify once, expect pass."** The risk the ADR is guarding against — an actively-supported self-managed GitLab silently ignoring `--filter=blob:none` — has no supporting evidence and strong evidence against it. The Validation item calling for a live check against the `[#411]` reporter's self-managed 19.3 instance remains worth keeping (cheap, and it is the authoritative signal for two other unrelated failure texts per the ADR's own D-T9/Validation reasoning) but should not be framed as adjudicating a live open question — it is a confirmation run, not a discovery run.

**What a test must assert to prove the filter APPLIED rather than was ignored** (concrete mechanisms, in order of strength):

1. **`git rev-list --objects --all --missing=print | grep -c '^\?'` returns > 0** on the cloned repository. Git's own docs (git-scm.com/docs/partial-clone) describe `--missing=print` exactly for this purpose: objects the filter excluded are listed with a `?` prefix. A count of zero on a repository known to contain blobs proves the filter did **not** exclude anything (either ignored, or the repo has no blobs reachable from the cloned refs — control for that separately).
2. **`git cat-file --batch-check --batch-all-objects` emits no object of type `blob`** in the fresh clone, before any checkout. This is a direct absence check, independent of git's own bookkeeping in (1).
3. **The pack-protocol "filter" capability actually appears in the negotiation**, captured via `GIT_TRACE_PACKET=1`. This is the mechanism-level proof that the *server* advertised support (the ref/capability advertisement in protocol v2 carries a `filter` line — git-scm.com/docs/protocol-common and gitprotocol-pack(5)), as distinct from (1)/(2), which only prove the *client's resulting repository* lacks blobs. All three together rule out "client silently fell back to a full clone but happens to be small" as well as "server silently ignored the filter and sent everything, but the test only checked a small subset of blobs."
4. **Negative control:** the same three checks run against a full (`--filter=` omitted) clone must show zero missing objects and every blob type present — proving the assertions are discriminating, not vacuously true (per `quality-core.md`'s Unchecked Green principle).

### 6. Measuring a blobless clone

**GitHub confirmed to support `--filter=blob:none`:** *"On github.com and GitHub Enterprise Server 2.22+, blobless clones using `git clone --filter=blob:none` are available"* ([github.blog — Get up to speed with partial clone and shallow clone](https://github.blog/open-source/git/get-up-to-speed-with-partial-clone-and-shallow-clone/)). `ocx-sh/index` is a public GitHub repository, so the measurement recipe below will run and produce real numbers there — **but this only characterizes GitHub's server (a different codebase from GitLab/Gitaly), and per the ADR's own framing, is not evidence about GitLab.** The live-instance run against the `#411` reporter's self-managed GitLab 19.3 (already in Validation) is what actually needs to happen for a GitLab claim; the GitHub measurement below is a legitimate stand-in only for **cost characterization of the index's own commit/tree graph**, not for **GitLab server behavior**.

## Measurement recipe

Copy-pasteable; run by a human against the real `ocx-sh/index` repository (not run here, per constraints).

```sh
# --- setup ---
WORKDIR=$(mktemp -d)
URL=https://github.com/ocx-sh/index.git

# (a) wall-clock time + (b) on-disk size — bare + --no-checkout so no blobs
#     are pulled by an implicit working-tree checkout, which would corrupt
#     both the size and the "filter applied" measurement below
/usr/bin/time -v git clone --filter=blob:none --no-checkout --bare \
  "$URL" "$WORKDIR/index-blobless.git" 2> "$WORKDIR/clone.log"
# "Elapsed (wall clock) time" line in clone.log is (a); alternatively:
#   time git clone --filter=blob:none --no-checkout --bare "$URL" "$WORKDIR/index-blobless.git"

du -sh "$WORKDIR/index-blobless.git"                     # (b) on-disk size

# (c) bytes transferred — two independent ways, use both:
#     (c1) git's own pack-receive report, always available, no extra tooling
grep -E "Receiving objects|Resolving deltas" "$WORKDIR/clone.log"
#     (c2) exact wire bytes via curl tracing (HTTP(S) transport only)
GIT_TRACE_CURL=1 git clone --filter=blob:none --no-checkout --bare \
  "$URL" "$WORKDIR/index-blobless-curltrace.git" 2> "$WORKDIR/curl-trace.log"
grep -i "content-length\|Recv data" "$WORKDIR/curl-trace.log" | tail -20

# (d) proof the filter APPLIED, not merely a small repo or a silent fallback
cd "$WORKDIR/index-blobless.git"

# (d1) missing (promisor) objects — must be > 0
git rev-list --objects --all --missing=print | grep -c '^\?'

# (d2) no blob objects present in the fresh clone
git cat-file --batch-check --batch-all-objects \
  | awk '{print $2}' | sort | uniq -c
#   expect: only "commit" and "tree" counts, zero "blob"

# (d3) server actually advertised/honored the "filter" capability
#      (rules out "client silently fell back to a full clone")
GIT_TRACE_PACKET=1 git -c protocol.version=2 clone --filter=blob:none \
  --no-checkout --bare "$URL" "$WORKDIR/index-blobless-packettrace.git" \
  2> "$WORKDIR/packet-trace.log"
grep -i "filter" "$WORKDIR/packet-trace.log"
grep -i "filtering not recognized by server" "$WORKDIR/clone.log" \
  "$WORKDIR/packet-trace.log"   # MUST be empty — its presence means the
                                 # filter was silently ignored server-side

# (d4) negative control — full clone must show the opposite results,
#      proving (d1)-(d3) actually discriminate rather than being vacuous
git clone --no-checkout --bare "$URL" "$WORKDIR/index-full.git"
cd "$WORKDIR/index-full.git"
git rev-list --objects --all --missing=print | grep -c '^\?'   # expect 0
git cat-file --batch-check --batch-all-objects \
  | awk '{print $2}' | sort | uniq -c                           # expect blobs present

# --- cleanup ---
rm -rf "$WORKDIR"
```

Report in the PR body: (a) wall-clock seconds, (b) `du -sh` result, (c) the "Receiving objects" MiB/KiB figure (and, if available, the curl content-length sum), and (d) the missing-object count from (d1) plus confirmation that (d3) showed no "filtering not recognized" warning. State plainly in the PR body that this number characterizes GitHub's server and the index's own object graph, not GitLab's Gitaly implementation — the authoritative GitLab-side signal is the separate live run against the `#411` reporter's self-managed instance (already a Validation item).

## Negative findings

- **Could not find a primary GitLab source pinning down the exact GitLab (not Gitaly-internal) release number where `uploadpack.allowFilter` became default-enabled** (e.g., "GitLab 12.10" or "GitLab 13.0" verbatim in a changelog entry). The closure dates of gitaly#2510 (2020-03-17) and gitaly#2553 (2020-05-05) bound it to that window, and GitLab's release cadence at the time was monthly (12.9 → 13.0 in that span), but no changelog text was retrieved that names the exact shipping version. This does not weaken the verdict (the six-year-plus margin over the current support window swallows any imprecision here) but is stated as a gap rather than rounded up.
- **Could not retrieve the current `doc/topics/git/partial_clone.md` (or equivalent) page's exact text on self-managed vs. GitLab.com support**, because `docs.gitlab.com/topics/git/partial_clone/` 302-redirects to an internal auth gateway and the direct raw-source path returned 404 (the page appears to have been merged into `doc/topics/git/clone.md` under the current docs IA). The `clone.md` and a mirrored mid-2020s copy of the old `partial_clone.md` were both fetched instead and neither mentions any self-managed-specific caveat or manual-enablement requirement — consistent with (but not a direct citation of) "on by default everywhere today."
- **No primary source found for exactly which GitLab minor version will remove `merge_request.merge_when_pipeline_succeeds` entirely** (deprecated 17.11, no removal version stated in the docs at time of writing). Irrelevant to this design since the option is forbidden either way, but flagged since a future removal would shrink the forbidden-option list from two entries to one without any action needed from ocx.
- **Did not find a GitLab-published document explicitly stating "an unsupported filter is silently ignored."** The only primary-source text found on client behavior against a non-supporting server is the opposite: a loud, visible warning (`"filtering not recognized by server"`, per gitaly#1553's own description of the pre-fix behavior). No case of *silent* server-side ignoring was found anywhere (docs, issues, or blog posts) for either GitLab or GitHub. This is a negative finding worth keeping explicit: the ADR's phrasing ("a server that silently ignores an unsupported filter") describes a failure mode with no found precedent, not a documented GitLab behavior.
- **Did not attempt to verify GitLab 19.4's unreleased status independently beyond the docs page mentioning it ahead of the 19.3 release under evaluation** — flagged as likely pre-release documentation (common GitLab practice of merging docs before the version ships), not confirmed against an official release calendar.
