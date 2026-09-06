# Research: index claim command — operability & cost

## Metadata

- Date: 2026-09-04
- Expires: 2027-03-04
- Domain: devops
- Triggered by: /hex-architect index claim command (dossier .agents/discussions/index-claim-command.md)
- Author: hex-architect research lane, axis operability & cost

## Direct Answer

Every job-token failure mode `ocx package claim`/`announce --transport git` will hit is either **preflight-checkable** (query `ci_push_repository_for_job_token_allowed` and the allowlist before ever pushing) or **already covered by an existing generic branch protection error** GitLab raises regardless of credential kind — so the ADR does not need new forge-specific error text for most of decision 34's "version gating," only a preflight and a mapping of the generic 403 body to the named error. On exit codes: the discussion's own recommendation (`EX_UNAVAILABLE`/69 for a version/allowlist-rejected push) undersells the distinction a pipeline needs between "retry me" and "an admin must change a setting" — `crates/ocx_lib/src/cli` already has the right precedent (`ReferrersUnsupported = 84`, a dedicated code for "the server lacks a capability, no fallback exists") and claim/announce should follow it rather than reuse 69. On live-forge testing, no comparable CLI (`glab` itself included) has solved this cheaply — the ecosystem's answer is the same one already in the discussion's Verification section: a fixture for CI, a human's live run as the only real-server gate.

## Trends

- **GitLab's job-token surface is still hardening.** `allow_push_repository_for_job_token` went from a disabled-by-default flag (17.2) to GA with the flag removed (18.4) in about two years; cross-project push followed the same shape one release later (19.0 flagged → 19.1 GA). The 18.0 release also *tightened* an existing behavior (public/internal projects lost their implicit cross-project job-token access and now need explicit allowlisting like private ones) — vintage: [GitLab job token docs](https://docs.gitlab.com/ci/jobs/ci_job_token/), fetched 2026-09-04; [issue #468320](https://gitlab.com/gitlab-org/gitlab/-/issues/468320).
- **CLI exit-code taxonomies in this space are ad hoc, not sysexits-aligned.** `gh` documents a handful of codes (0/1/2/4, plus a couple of per-command extras like `gh pr checks`'s 8) in a manual page most scripts never read; `glab` and `oras` publish no dedicated exit-code reference at all — [`gh` exit-codes](https://cli.github.com/manual/gh_help_exit-codes). OCX's own sysexits-aligned enum (`.claude/rules/quality-rust-exit_codes.md`) is already ahead of every comparable tool surveyed; nothing found argues for changing that direction, only for placing this specific failure correctly within it.
- **`gh pr create` still has no `--json` output**, years into repeated feature requests ([#11558](https://github.com/cli/cli/issues/11558), [#11247](https://github.com/cli/cli/issues/11247), [#6366](https://github.com/cli/cli/issues/6366)) — the single biggest "don't repeat this" data point for claim/announce's `--format json` commitment.
- **No OSS tool in this space has cracked cheap live-forge verification.** `glab`'s own maintainers have an *open, unresolved* issue asking for exactly this ([gitlab-org/cli#1161](https://gitlab.com/gitlab-org/cli/-/issues/1161)); `python-gitlab` is the one project that actually runs a real `gitlab/gitlab-ce` in CI, at real cost (see Q4 below).

## Key Findings

### 1. GitLab job-token push failure modes — observable text

- **Setting off / version too old.** No single documented error string exists for this case — GitLab's job-token push capability simply doesn't exist pre-17.2, and is opt-in (`ci_push_repository_for_job_token_allowed`, default off) through 18.4 GA. Expect the generic authorization-denied shape below, not a capability-specific message. This is exactly the gap decision 34's "named error" closes, and exactly why a **preflight** (see Finding 7) beats parsing push failures: [job token docs](https://docs.gitlab.com/ci/jobs/ci_job_token/).
- **Cross-project push, publisher not on the index project's allowlist.** Conceptually "This project is not in the job token scope" per GitLab's own allowlist model; the scope API docs do not publish an exact wire error string or status code for the rejection ([job token scope API](https://docs.gitlab.com/api/project_job_token_scopes/), fetched 2026-09-04 — confirms the endpoints exist, explicitly does not document the rejection's error body). Treat the exact text as **unverified — flag for the live GitLab 19.3 run** already planned in the discussion's Verification section.
- **Protected-branch push, regardless of credential kind.** GitLab's generic branch-protection rejection — commonly reported as `You are not allowed to push code to protected branches on this project` over HTTP, a `remote: GitLab: ...` line in git's stderr — fires for a job token exactly as it would for a PAT with insufficient role ([community reports](https://lucaberton.medium.com/fixing-the-gitlab-error-you-are-not-allowed-to-push-code-to-protected-branches-on-this-project-e82b0d6de61a); GitLab's own protected-branches feature docs). **This is structurally avoided by design**: claim/announce push to a dedicated `indexbot-claim-*`/`indexbot-announce-*` branch, never the index's protected default branch — worth one sentence in docs so an operator who wildcard-protects `indexbot-*` recognizes their own misconfiguration rather than filing an ocx bug.
- **REST reads under a job token, wrong endpoint.** The job-token permission table is read-only and enumerates: branches (`GET /projects/:id/repository/branches`), commits including `/commits/:sha/merge_requests`, raw files (`GET /projects/:id/repository/files/:file_path/raw`), merge requests (list + single), and tags. No compare endpoint, no fork endpoints, no MR-create endpoint — this matches and confirms what the discussion's Threads section already fact-checked. GraphQL cannot be authenticated with a job token at all — a corner not yet in the ADR draft; if any future observe path used GraphQL under `--transport git`, it must fall back to REST rather than silently 401 ([job token docs](https://docs.gitlab.com/ci/jobs/ci_job_token/)).
- **Auth header.** Confirmed: `JOB-TOKEN: $CI_JOB_TOKEN` is GitLab's own recommended header (form/data params also accepted, query string explicitly discouraged); PAT/project/group/OAuth tokens use `Authorization: Bearer`. This matches decision 32 exactly.
- **Deploy tokens and the REST API.** No GitLab doc found grants deploy tokens general REST access; their documented scopes (`read_repository`, `read_registry`, etc.) are consumed by git/registry auth, not `Authorization` header REST calls — corroborates decision 9's "a deploy token can push but not read" framing, but note this is an absence-of-evidence finding, not a documented refusal string, so the same live-run caution applies if ocx ever needs to produce a *named* error for "you put a deploy token in `OCX_ANNOUNCE_TOKEN`."

### 2. Exit-code doctrine — precedent and the version-gating gap

| Tool | Approach | Note |
|---|---|---|
| `gh` | 0 success / 1 generic / 2 cancelled / 4 auth-required, plus rare per-command codes | No sysexits alignment; codes invented locally, thinly documented ([exit-codes](https://cli.github.com/manual/gh_help_exit-codes)) |
| `glab`, `oras`, `cosign` | No dedicated exit-code reference found | Cobra/cli-framework defaults (0/1) — nothing to borrow |
| `cargo` | No sysexits reference found in this pass | Not a comparable "server capability" case |
| ocx (existing) | Owns a sysexits-aligned enum, private range 79–84 already in use (`NotFound`, `AuthError`, `PolicyBlocked`, `DirtyRcBlock`, `TransparencyLogUnavailable`, `ReferrersUnsupported`) | Strongest applicable precedent is **internal**, not external |

The discussion's own open question recommends `EX_UNAVAILABLE` (69) for "a version/allowlist-rejected job-token push," reasoning "the instance lacks the capability." That reasoning is sound but the code choice undercuts it: `Unavailable` in `quality-rust-exit_codes.md` is documented as "Required resource unavailable: network down, registry unreachable" — a **retry-shaped** failure, and `forge/error.rs` already uses it exactly that way (5xx, transport errors). A version/allowlist rejection is the opposite: retrying the identical command will fail identically until an administrator flips a project setting or the instance is upgraded. OCX already has the right-shaped code for this: `ReferrersUnsupported = 84` exists precisely because "registry does not implement the OCI Referrers API ... discovery fails hard rather than silently returning empty results" is the same failure *class* — present, reachable, but missing a capability, no fallback. Recommend a sibling in the same family rather than overloading 69 (see Recommendation table).

### 3. CI recipe shapes

- **Minimal GitLab job-token flow.** Preconditions, in the order a pipeline author hits them: (1) GitLab ≥ 18.4 (self-managed) or GitLab.com; (2) on the **target index project**, Settings → CI/CD → Job token permissions → "Allow Git push requests to the repository" (`ci_push_repository_for_job_token_allowed = true`, queryable via `GET /projects/:id`); (3) for cross-project (publisher ≠ index project, GitLab ≥ 19.1), the index project's job-token inbound allowlist (`GET/POST /projects/:id/job_token_scope/allowlist`) must include the publisher project; (4) the runner image needs `git` — most default images do, Alpine-based ones need `apk add git`. No GitLab-side change is needed on the *publisher's* project.
- **Minimal GitHub Actions REST flow.** `GITHUB_TOKEN` (workflow-default) has read+write scope for API calls, but a PR it opens **will not retrigger** other `pull_request`-triggered workflows — a deliberate anti-cascade feature, not a bug ([GitHub docs — `GITHUB_TOKEN`](https://docs.github.com/en/actions/concepts/security/github_token); confirmed across multiple `orgs/community` discussions, e.g. [#55906](https://github.com/orgs/community/discussions/55906)). This is exactly why posture (d) in the discussion needs a machine-account PAT whenever owner-gated auto-merge must fire on the claim/announce PR itself.
- **GHES specifics.** `GITHUB_API_URL` (and `GITHUB_SERVER_URL`/`GITHUB_GRAPHQL_URL`) are auto-populated by GHES-hosted runners; ocx's forge client already takes the host from `--index-repo`'s coordinate rather than hardcoding `api.github.com`, so nothing new is needed there. The one real GHES operability question the discussion doesn't mention: a GHES instance behind an internal CA needs its cert trusted by whatever TLS backend the forge HTTP client uses (`reqwest`) — this is an environment/OS trust-store concern, not an ocx flag, and is worth one line in docs so a GHES operator doesn't file a false "ocx can't reach GHES" bug.
- **Self-managed GitLab, nested groups.** GitLab's REST API requires the full path percent-encoded (`%2F` for each `/`) when addressing a project by path — `python-gitlab` has an open ergonomics issue about exactly this ([python-gitlab#1498](https://github.com/python-gitlab/python-gitlab/issues/1498)). The coordinate parser in the ADR must do this encoding itself; the discussion's own nested-group example in `command-line.md` already models the right UX (operator writes a plain path, ocx worries about encoding).
- **What comparable tools document as preconditions.** Renovate's GitLab platform docs scatter minimum-version notes inline per feature ("Draft: MR prefix supported since GitLab v13.2.0") rather than one table — lighter to write, harder to audit. Homebrew's `bump-formula-pr` action docs are the strongest structural precedent found for a **two-credential** story: `COMMITTER_TOKEN` (must be a real PAT, not the default token, because it forks and opens a PR) is documented separately from the workflow's own `GITHUB_TOKEN` (limited, read-only use) — this maps almost exactly onto decision 9's `OCX_ANNOUNCE_TOKEN` (API reads) vs `OCX_ANNOUNCE_GIT_TOKEN` (push) split ([Homebrew discussion #3383](https://github.com/orgs/Homebrew/discussions/3383)).

### 4. Live-forge verification for maintainers

- **`gitlab/gitlab-ce` Docker**: GitLab's own install docs cite "1GB or more" RAM as a floor, but that is a bare-minimum-to-boot number, not a usable-for-testing one; community operators routinely report multi-minute startup and meaningfully higher memory for a responsive instance. Treat any number here as a floor, not a working budget, and validate empirically before relying on it in CI.
- **GDK / "GDK-in-a-box"**: built for developing GitLab itself, not for testing a client against it — 30GB disk, 8GB+ image download. Wrong tool for this job.
- **`python-gitlab`'s own answer**: `tests/functional/fixtures/docker-compose.yml` brings up `gitlab/gitlab-ce`, with `GITLAB_IMAGE`/`GITLAB_TAG` overridable so the pinned version can be bumped deliberately — the one working precedent for "run acceptance tests against a real GitLab," at real infrastructure cost, run on a schedule rather than per-PR by that project.
- **`glab`'s own gap**: GitLab's own CLI team has an *open, unresolved* issue asking for a real-instance integration job ([gitlab-org/cli#1161](https://gitlab.com/gitlab-org/cli/-/issues/1161)) — confirmation that this is a genuinely unsolved, not just ocx-specific, problem.
- **Recommendation for ocx**: don't chase a per-PR live GitLab in CI. Keep the enhanced `fake_forge.py` + `git http-backend` fixture (already planned) as the PR-gating check; if a real-instance smoke test is wanted at all, follow `python-gitlab`'s pattern — a scheduled (not per-PR) job against a pinned `gitlab/gitlab-ce` tag, bumped on a cadence — and treat the `#411` reporter's offered live run against their self-managed **19.3** instance exactly as the discussion's Verification section already does: the one authoritative real-server signal, not a thing to reproduce in CI.

### 5. Observability — `--format json` report fields

- `gh pr create` shipping with **no** `--json` support for years, despite repeated requests ([#11558](https://github.com/cli/cli/issues/11558), [#6366](https://github.com/cli/cli/issues/6366)), is the strongest negative precedent found: whatever else changes in the ADR, claim/announce must not ship `--format json` as an afterthought — the discussion's requirement that it exist from day one is validated by this gap, not merely convenient.
- `gh`'s working `--json` commands (e.g. `gh pr list --json number,title,author`) establish the shape worth reusing: flat, named, independently selectable fields — not a nested envelope.
- ocx's own existing `announce` report is the actual baseline to extend, not invent: `package`, `status`, `pull_request_url`, `pull_request_number`, `fork`, `desc_status`, `written_paths`, `reserved_tags_dropped` (`website/src/docs/reference/command-line.md:2734-2747`). For `claim`, and for the `--transport git` case on both commands, the discussion's own Requirements section already names most of what operability needs on top: forge, transport, and rendered owners. From this axis, add:
  - `credential_kind` — one of `pat` / `job-token` / `deploy-token` / `oauth` (never the secret itself), so a pipeline can assert "yes, this ran as the job token" without parsing logs.
  - `author` — the identity that actually authored the PR/MR (may differ from `owners`; this is exactly the discussion's "authorship vs ownership" split made machine-legible).
  - `branch` — the claim/announce branch name, so a script can act on `PullRequestUnmergeable`-style failures without re-deriving the naming convention.
  - `capability_checks` — an array or map of the version/allowlist preflight results actually performed (see Finding 7), so a pipeline can assert the preflight ran rather than trusting a bare success.

### 6. Docs structure precedent

- `gh`'s authentication docs separate "which token is used and in what precedence" from "what scope it needs" from "how to introspect the active identity" (`gh auth status`) — three distinct concerns, three distinct sections.
- Homebrew's two-token split (Finding 3) is the closest analog to ocx's four-posture story and is worth citing directly in the use-case page as "this is not a novel pattern — Homebrew's own bump-formula action draws the same line between an API credential and a push/PR credential."
- Renovate's inline-version-callout style is lighter-weight but harder to audit than a single table; given the discussion already commits to a version-gating table (decision 34), a **single canonical minimum-version table** (forge × capability × version) is the better fit for ocx's audience (platform engineers auditing a self-managed instance) than scattering callouts through prose.
- Recommended outline for the use-case page, informed by the above:
  1. One sentence per posture naming who authors the PR/MR (already drafted in the discussion's Requirements).
  2. A table: posture → env vars → author identity → works with owner-gated auto-merge? → minimum forge version.
  3. One copy-paste CI recipe per posture (GitLab job-token `.gitlab-ci.yml` snippet; GitHub Actions `GITHUB_TOKEN` snippet; GHES note; self-managed GitLab nested-group note).
  4. A troubleshooting section keyed to the named errors this ADR introduces, each mapped to the pitfall in Finding 7 that produces it.

### 7. Support burden and a cheap preflight

- **Cross-project allowlist confusion is the single most likely support ticket.** GitLab's own 18.0 migration *removed* the implicit access public/internal projects used to get, meaning an operator who tested this months ago on an old GitLab version may find it silently stops working after an upgrade — worth a specific callout in docs, not just the general allowlist explanation.
- **Protected-branch defaults**: avoided by construction (claim/announce never targets the index's default branch) — see Finding 1. Document the one way an operator can still self-inflict this (a wildcard protected-branch pattern that happens to match the `indexbot-*` branch prefix).
- **Nested-group path encoding**: `python-gitlab`'s own open issue confirms this bites every GitLab API client, not just ocx; the coordinate parser owning the encoding (rather than asking the operator to pre-encode) is the correct design and is already implied by the discussion's coordinate model.
- **A cheap preflight is concretely possible today.** `GET /projects/:id` on the target index project returns `ci_push_repository_for_job_token_allowed` directly — no push attempt required to learn the setting is off. `GET /projects/:id/job_token_scope/allowlist` on the same project answers the cross-project question just as cheaply. Both are read-only, both are reachable with the same job token that will attempt the push, and both let `ocx package claim --check` (the open question in the discussion) name the exact missing setting *before* any write is attempted — turning today's "push fails with an opaque 403" into "GitLab project `acme/index` has job-token push disabled; ask a Maintainer to enable it in Settings → CI/CD → Job token permissions," which is precisely decision 34's "names which setting or version is missing" doctrine, made achievable with two REST calls instead of parsing push failure text whose exact wording GitLab does not document.

## Design Patterns Worth Considering

- **Preflight over failure-parsing.** Query the two capability fields above before attempting a git push under `--transport git`; only fall through to a generic named error if the preflight itself is unreachable (e.g., the job token can't even read its own project — should not happen, but must fail closed with a distinguishable message from "the setting is off").
- **A dedicated exit code for "forge lacks capability," not `Unavailable`.** Matches `ReferrersUnsupported`'s existing precedent; keeps "retry me" (69/75) discriminable from "an admin must act" (new code) the same way OCX already discriminates them for the OCI referrers case.
- **Two-credential documentation split modeled on Homebrew's `COMMITTER_TOKEN`/`GITHUB_TOKEN` pattern**, not a single-token-with-modes story — this is already decision 9's shape; cite the precedent rather than re-derive it.
- **Scheduled, not per-PR, live-forge testing**, following `python-gitlab`'s pinned-image pattern, reserved for a capability the fixture genuinely cannot fake (the actual push-option → MR-creation server behavior); the fixture stays the PR gate.
- **`capability_checks` in the JSON report** as the mechanism that lets a pipeline assert a preflight actually ran, closing the same "was this really checked or did it silently skip" gap `quality-core.md`'s Unchecked Green principle warns about — applied here to the *pipeline's* view of ocx's own preflight, not just to ocx's internal tests.

## Sources

- [GitLab CI/CD job token docs](https://docs.gitlab.com/ci/jobs/ci_job_token/) — fetched 2026-09-04, read-only endpoint table, push versions, header names
- [GitLab CI/CD job token scope API](https://docs.gitlab.com/api/project_job_token_scopes/) — fetched 2026-09-04, allowlist endpoints
- [GitLab Projects API](https://docs.gitlab.com/api/projects/) — fetched 2026-09-04, `ci_push_repository_for_job_token_allowed` field
- [Rollout of `allow_push_repository_for_job_token` (#468320)](https://gitlab.com/gitlab-org/gitlab/-/issues/468320)
- [Job token scope work item #419625](https://gitlab.com/gitlab-org/gitlab/-/work_items/419625) — allowlist behavior
- [`ci_push_repository_for_job_token_allowed` MR (client-go)](https://gitlab.com/gitlab-org/api/client-go/-/merge_requests/2108/commits)
- [Fixing "You are not allowed to push code to protected branches"](https://lucaberton.medium.com/fixing-the-gitlab-error-you-are-not-allowed-to-push-code-to-protected-branches-on-this-project-e82b0d6de61a)
- [`gh` exit-codes manual](https://cli.github.com/manual/gh_help_exit-codes)
- [GITHUB_TOKEN docs](https://docs.github.com/en/actions/concepts/security/github_token) and [community discussion #55906](https://github.com/orgs/community/discussions/55906) — anti-cascade behavior
- [`gh pr create` lacks `--json` (#11558)](https://github.com/cli/cli/issues/11558), [(#11247)](https://github.com/cli/cli/issues/11247), [(#6366)](https://github.com/cli/cli/issues/6366)
- [Homebrew `bump-formula-pr` discussion #3383](https://github.com/orgs/Homebrew/discussions/3383) — two-credential precedent
- [Renovate GitLab platform docs](https://docs.renovatebot.com/modules/platform/gitlab/)
- [python-gitlab functional test fixtures](https://github.com/python-gitlab/python-gitlab) — `tests/functional/fixtures/docker-compose.yml`, `GITLAB_IMAGE`/`GITLAB_TAG`
- [python-gitlab nested-path encoding issue #1498](https://github.com/python-gitlab/python-gitlab/issues/1498)
- [glab CLI open live-instance testing issue #1161](https://gitlab.com/gitlab-org/cli/-/issues/1161)
- [GitLab Development Kit / GDK-in-a-box docs](https://docs.gitlab.com/development/contributing/first_contribution/configure-dev-env-gdk-in-a-box) and [GitLab Docker install docs](https://docs.gitlab.com/install/docker/installation/)
- Local: `.agents/discussions/index-claim-command.md`; `.claude/artifacts/research_index_claim_council_transport.md`; `.claude/rules/quality-rust-exit_codes.md`; `crates/ocx_lib/src/forge/error.rs`; `crates/ocx_lib/src/announce/error.rs`; `website/src/docs/reference/command-line.md:2693-2778`; `website/src/docs/reference/environment.md:117-137`

## Recommendation

**Error → exit code table** (additions/clarifications this axis found, against the existing `ExitCode` enum in `quality-rust-exit_codes.md`):

| Condition | Exit code | Rationale |
|---|---|---|
| `--transport git` on GitHub, or `--fork` with `--transport git` | `UsageError` (64) | Malformed invocation — matches existing `ForgeKindUnknown`-class handling |
| No credential reachable under either transport (`AuthError` today) | `AuthError` (80) | Unchanged — credential problem, not a capability problem |
| Preflight (or a live push) shows job-token push disabled on the target project | **New**: sibling of `ReferrersUnsupported`, e.g. `ForgeCapabilityUnavailable` (85) | Not retry-shaped — an admin must flip a setting or upgrade GitLab; reusing `Unavailable` (69) tells a CI wrapper "try again" when trying again will never help |
| Preflight shows the publisher project missing from the index project's job-token allowlist | Same new code (85), message names the allowlist and both project paths | Same class: fixed by an admin action on the *target* project, not by the caller |
| Protected-branch push rejection surfaces despite the dedicated claim/announce branch | `PermissionDenied` (77) or a named `ForgeError` variant, not `AuthError` | The credential is fine; a branch-protection *rule* is the blocker — distinct remediation from a bad token |
| Missing `git` binary under `--transport git` | `Unavailable` (69), checked before any network call | Matches the discussion's own open-question recommendation; this one genuinely is "the tool isn't there," a local-environment gap, unlike a version/allowlist gate |
| Job-token push succeeds against a protected branch that a wildcard rule unexpectedly covers | Same `PermissionDenied`/`ForgeError` path above | No new code needed — same failure shape as ordinary protected-branch rejection |

**`--format json` report fields to add** (on top of the existing `announce` report's `package`/`status`/`pull_request_url`/`pull_request_number`/`fork`/`desc_status`/`written_paths`/`reserved_tags_dropped`): `forge`, `transport`, `credential_kind` (`pat`/`job-token`/`deploy-token`/`oauth`, never the secret), `author` (identity that authored the PR/MR — distinct from `owners`), `branch`, `owners` (rendered `login`/`id` pairs), and `capability_checks` (which version/allowlist preflight checks ran and passed, so a pipeline can assert the preflight actually executed rather than trusting a bare success).

**Preflight**: implement `ocx package claim --check` (and reuse the same preflight inside a live `--transport git` run before the push) as two REST calls — `GET /projects/:id` for `ci_push_repository_for_job_token_allowed`, `GET /projects/:id/job_token_scope/allowlist` for cross-project — both reachable with the same job token that will do the push, both documented today, and both able to name the exact missing setting before any write happens.

**Live-forge verification**: do not add a per-PR real-GitLab job. Keep the planned `fake_forge.py` + `git http-backend` fixture as the CI gate; if a scheduled real-instance job is wanted later, follow `python-gitlab`'s pinned `GITLAB_IMAGE`/`GITLAB_TAG` pattern rather than inventing one. Treat the `#411` reporter's offered run against their live GitLab 19.3 exactly as the discussion's Verification section already does — the one authoritative real-server signal.

## Negative findings / dead ends

- No documented exact wire error string exists for "job-token push disabled" or "allowlist rejection" — GitLab's docs describe the settings, not the rejection text. Do not hard-code a string match against forge response bodies for these cases; use the preflight fields instead, and treat any push-time text as best-effort/unverified pending the live 19.3 run.
- `gitlab/gitlab-ce`'s documented "1GB RAM" floor is not a usable testing budget in practice — don't cite it as a CI resourcing plan without an empirical check first.
- No sysexits-aligned precedent exists in `gh`, `glab`, `oras`, or `cosign` for the version/allowlist-rejection case — this question has no external answer to borrow; the internal `ReferrersUnsupported` precedent is the strongest available anchor.
- GDK / GDK-in-a-box is not a fit for ocx's live-verification needs at any cost tier considered — it is a GitLab-development tool, not a client-testing fixture.
- Deploy-token REST-API refusal is an absence-of-evidence finding (no doc grants deploy tokens API access), not a confirmed refusal string — do not write a named error message quoting specific text for this case without a live check.

## Self-check

- Every claim above cites a URL or a local `path:line`; two claims are explicitly flagged as unverified (exact rejection wire-text, deploy-token API refusal) rather than presented as confirmed.
- Sources older than ~18 months: none knowingly used — all GitLab docs fetched live 2026-09-04; `gh`/Homebrew/Renovate/python-gitlab pages are current project docs, not dated snapshots, but exact version numbers cited (18.4, 19.1, 18.0 migration) are traceable to the fetched pages and should be re-checked if this report is read past a GitLab minor-release boundary — hence the six-month expiry above.
- The one recommendation that most changes the discussion's current draft (a new exit code instead of reusing `Unavailable`/69) is flagged as a recommendation with rationale, not asserted as already-decided; the discussion's own open question is quoted verbatim so the ADR author can weigh the tradeoff directly.
