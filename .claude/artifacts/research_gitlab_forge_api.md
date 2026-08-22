# Research: GitLab REST v4 for the announce forge

<!--
Gathered 2026-08-22 while implementing `GitLabForge`
(`crates/ocx_lib/src/forge/gitlab.rs`). Sources: docs.gitlab.com (fetched, not
recalled) and a read of the grimoire donor (`../grimoire/src/catalog/forge.rs`,
`src/catalog/index_announce.rs`). Decisions taken from these facts are recorded
in `adr_announce_gitlab_forge.md`; this file is the evidence behind them.

Where a fact was NOT confirmed against the docs it is marked **unverified** and
the implementation treats it defensively.
-->

**Date:** 2026-08-22 · **Consumers:** `forge/gitlab.rs`, `test/tests/fake_gitlab.py`

## The one fact that decided the design

`POST /projects/:id/repository/commits` accepts a batch of file `actions[]`, and
each `update` / `move` / `delete` action takes **`last_commit_id`**, documented as
"Last known file commit ID" for conflict detection.

That is the compare-and-swap. It matters because announce's C4 contract requires
a concurrent announce to be preserved rather than overwritten, GitLab has **no**
ref-level CAS, and the obvious alternative — `force: true` — is a silent
clobber. The donor takes exactly that clobbering path (`git push --force`,
`index_announce.rs:566`), so this is the single largest divergence from it.

Two consequences the implementation depends on:

1. `last_commit_id` names the **file's** last commit, not the branch head. A
   branch sha would be rejected whenever the head did not happen to touch that
   file. The value is read from the JSON (non-raw) files endpoint, which returns
   `last_commit_id` alongside the content.
2. Guessing `create` versus `update` wrong fails the whole batch — GitLab has no
   upsert. Existence is therefore probed at the base ref before the batch is
   built.

## Endpoint reference (as used)

| Operation | Endpoint | Notes |
|---|---|---|
| Project document | `GET /projects/:id` | `id`, `path_with_namespace`, `default_branch`, `forked_from_project`, `import_status`, `permissions`. `permissions` appears only on an authenticated read; an invisible project answers 404, not 403. |
| Branch head | `GET /projects/:id/repository/branches/:branch` | `commit.id`; 404 when absent. |
| File bytes | `GET /projects/:id/repository/files/:path/raw?ref=` | `:path` is one URL-encoded segment. |
| File metadata | `GET /projects/:id/repository/files/:path?ref=` | Carries `last_commit_id` — the CAS value. |
| Compare | `GET /projects/:id/repository/compare?from=&to=&from_project_id=&straight=` | Returns `commits`, `compare_timeout`, `compare_same_ref`. `from` is read in `from_project_id`, `to` in `:id` — the split that makes a fork-vs-upstream comparison possible. |
| Atomic commit | `POST /projects/:id/repository/commits` | `branch`, `commit_message`, `actions[]`, `start_branch` XOR `start_sha`, `start_project`, `force`, `allow_empty`. 201 on success. |
| Merge request create | `POST /projects/:id/merge_requests` | Created on the **source** project; `target_project_id` names the upstream. Mirror image of GitHub. |
| Merge request list | `GET /projects/:id/merge_requests?state=opened&source_branch=&source_project_id=` | Listed on the **target** project. |
| Fork | `POST /projects/:id/fork` | `namespace_path` / `namespace_id` / `path` / `name`. Asynchronous — "the request returns immediately". |
| Fork listing | `GET /projects/:id/forks?owned=true&per_page=&page=` | The authoritative answer to "where is my fork". |
| Identity | `GET /user` | `id`, `username`. |

`:id` is a numeric project id **or** the percent-encoded `path_with_namespace`.
A nested group path (`acme/platform/tooling/index`) therefore needs no special
casing — it is one encoded segment. This is why `RepoCoordinate` had to grow a
multi-segment namespace, and why the acceptance suite asserts on the encoded form
appearing in the request log.

## Ahead / behind / diverged

GitLab publishes **no** ahead/behind verdict — unlike GitHub's compare, which
returns `status: identical|ahead|behind|diverged` directly. It returns a commit
list, so the verdict is derived from two directed comparisons:

```
ahead  = |commits in head_branch not in base|   (compare on head project, from=base via from_project_id)
behind = |commits in base not in head_branch|   (compare on base project, from=head_branch)
```

Both are required: one alone cannot separate `Ahead` from `Diverged`, and reading
a diverged branch as ahead is precisely what re-proposes squash-merged work
(ocx-sh/ocx#228).

`compare_timeout: true` reports a comparison GitLab gave up on, with an **empty
commit list** — indistinguishable from "no commits" unless checked. Reading it as
"no commits" would classify a live branch as spent and rebuild it, discarding
unmerged work, so it is refused rather than interpreted.

## Fork readiness

Forking is a background job. Readiness is reported by `import_status` on the new
project, **not** by the fork POST's status code. Values seen documented:
`none`, `scheduled`, `started`, `finished`, `failed`. `none` is what a project
that was never imported reports, so `none` and `finished` both mean ready;
`failed` is terminal and is failed fast rather than polled to the deadline.

**Unverified:** the complete enumeration of `import_status` values, and whether a
fork of a small project can report `none` immediately rather than passing through
`started`. The implementation treats any unrecognised value as "not ready yet",
which degrades to the bounded wait rather than to a false ready.

**Unverified:** the exact status code when a fork already exists. The
implementation treats `409` as the reuse trigger and falls through to
enumeration; any other non-success is a hard error carrying the body.

## Authentication

`PRIVATE-TOKEN` accepts a personal access token and a CI job token;
`Authorization: Bearer` accepts OAuth2 tokens. `PRIVATE-TOKEN` is therefore the
broader header, not the less safe one, and is what the client sends.

**Unverified:** exactly which of the endpoints above a CI job token may call —
in particular whether it can create a fork. This does not block the
implementation (a PAT covers every path), but a CI-job-token announce should be
treated as untested.

## Rate limiting

**Unverified in detail.** GitLab documents `RateLimit-*` headers and `Retry-After`
on throttled responses, and answers `429`. The client currently replays a failed
commit on 429 and 5xx with a fixed 3/9/27s backoff and does **not** read
`Retry-After`. That is a known gap, shared with the GitHub client, and is
recorded as a follow-up rather than silently omitted.

## What was taken from the donor, and what was refused

**Taken** (grimoire `src/catalog/forge.rs`):

- Fork parent verification on both the create and the reuse path.
- Identity read from response bodies only, never composed from a basename.
- Identity-based fork reuse by enumerating the upstream's owned forks (the
  donor's own D6 refinement, which replaced its earlier basename guess).
- Bounded readiness with a wall-clock deadline, a per-request timeout, and a
  fast-fail on a failed import.
- No-redirect HTTP client, so a cross-host 3xx cannot replay the credential.
- Capped response body in status errors.
- Exhaustive matching over the forge kind, so a future third forge cannot be sent
  an unauthenticated mutation by falling through a wildcard.

**Refused:**

- **`git push --force`** as the write path — it discards a concurrent announce.
  Replaced by `last_commit_id` (see above). This is the load-bearing difference.
- **The git-subprocess transport** (clone/commit/push) — OCX is REST-only
  (design register S1), so the donor's git-side hardening (`protocol.ext.allow`,
  push-URL validation) is moot; its *intent* survives as "rebuild every endpoint
  from a verified identity".
- **The tri-state push-permission probe that degrades to a direct push** — OCX's
  `--out` / `--fork` / fork-free trichotomy is the explicit equivalent, and every
  remote mode ends in a reviewed pull request.
- **`remove_source_branch` on the merge request.** The announce branch is per
  package and is reused across announces (C4); deleting it on merge would be a
  behaviour the GitHub path does not have.
- **`squash: true` on the merge request.** The index project's own merge settings
  decide that; a publisher's client should not impose it.

## Reviewed against the implementation (2026-08-22)

An adversarial cross-model pass over the finished client confirmed two facts
above and found one place where the code did not honour a third.

- **Confirmed:** the no-redirect policy is correctly configured, and every
  dynamic project, file, ref and branch segment travels through
  `encode_segment` — no traversal or endpoint-confusion path was found.
- **Confirmed:** the `last_commit_id` CAS is the right guard and is applied on
  the commit path.
- **Found:** the *read* preceding that guard was not atomic. The root was read
  through a ref **name** and the base SHA resolved in a second call, so an
  upstream advance between the two produced a commit whose `last_commit_id`
  named a version the client had never seen — and GitLab accepts it, because the
  guard compares against the newer commit. Recorded as D12 in the ADR.
- **Found:** `compare?straight=true` returning 200 without a `commits` array was
  counted as zero commits. The docs describe `commits` as always present; the
  client now treats its absence as an error rather than trusting the shape.
  Recorded as D13.

**Still unverified** and unchanged by the review: the complete `import_status`
enumeration, the exact status code for an already-existing fork, the CI-job-token
capability set, and the `Retry-After` handling.

## Delivered late by the research subagents (2026-08-22)

Two `worker-researcher` agents returned after the implementation had shipped.
Their findings are recorded here because three of them **correct** statements
made above or in the code, and the rest independently confirm decisions that had
been taken on thinner evidence.

### Corrections

1. **`compare_timeout: true` does NOT empty the commit list.** GitLab's own
   parameter table says `commits` is *"Always complete, even when
   `compare_timeout` is `true`"* — only `diffs` is truncated. The claim above,
   and the code comment that repeated it, were wrong. The client still refuses
   the response, and that is now a stated over-strictness rather than a
   misunderstanding: the documented guarantee is unverified against a real
   timeout, the commit list is the only input to the ahead/behind verdict, and
   erring permissive force-rebuilds a live branch.
2. **A CI job token cannot drive announce.** The "Authentication" section above
   left this unverified. GitLab's job-token access table enumerates Container
   Registry, Deployments, Environments, Jobs, Job artifacts, Packages and
   Releases — and lists none of repository files, commits, branches, merge
   requests or forking. Announcing from GitLab CI needs a real access token, not
   `CI_JOB_TOKEN`. Recorded in the code and in the environment reference.
3. **`last_commit_id` conflict detection may be branch-scoped, not file-scoped.**
   [gitlab-org/gitlab#3138](https://gitlab.com/gitlab-org/gitlab/-/work_items/3138)
   reports a false-positive conflict when an *unrelated* file in the same branch
   moved. If that is current behaviour, the CAS is stricter than intended — which
   fails safe (a spurious 400 triggers the regenerate-and-retry path) but would
   raise the retry rate on a busy index. Not observed here; worth watching.

### Confirmations, with the evidence that was missing before

- **`straight=true` is the right parameter** for a directed count: *"If `true`,
  comparison method is direct comparison between `from` and `to` (`from..to`).
  If `false`, compare using merge base (`from...to`)."* The merge-base form
  cannot express "ahead" alone. GitLab's own tracker calls the equivalent UI
  behaviour "backwards and undocumented"
  ([#382619](https://gitlab.com/gitlab-org/gitlab/-/issues/382619)), and the
  researcher got it inverted on their first pass — which is why the code carries
  a comment naming the semantics rather than just the flag.
- **Requiring `--forge` for a self-hosted host is the ecosystem norm, not a
  workaround.** Of the tools surveyed, `gh` and `glab` are single-forge by
  design; `goreleaser` takes the kind as a per-forge YAML key; `renovate`
  requires an explicit `platform:`; `git-pkgs/forge` requires `type =` in config
  for anything not on its known-domain list. The one attempt at unauthenticated
  probing found —
  [git-credential-manager#1434](https://github.com/git-ecosystem/git-credential-manager/issues/1434)
  — was closed as not-planned. GitLab's `/version` and `/metadata` both appear to
  require authentication, so there is no unauthenticated "is this GitLab" probe
  to reach for even if we wanted one.
- **`[HOST/]NAMESPACE/PROJECT` is the precedented grammar.** Both `gh` and `glab`
  spell it `-R HOST/OWNER/REPO`, with a bare two-segment form falling back to a
  default host — the shape shipped here.
- **GitHub is strictly two segments; GitLab nests to 20 subgroup levels**
  (21 counting the top-level group), soft-enforced. The flatness rule living in
  the GitHub client, and nowhere in the coordinate type, is right.
- **GHES is `https://<host>/api/v3`**, hostname unchanged — matching the
  implementation. `gh`'s `RESTPrefix()` does exactly this. GHES `/meta` exposes
  `installed_version` unauthenticated, which confirms GHES once already assumed
  but does not discover the forge kind.
- **`Retry-After` is unreliable on GitLab 429s**
  ([#365728](https://gitlab.com/gitlab-org/gitlab/-/issues/365728),
  [#230914](https://gitlab.com/gitlab-org/gitlab/-/issues/230914)) — headers go
  missing, notably on the 429 itself. The fixed backoff already in place is the
  correct fallback; honouring `Retry-After` when present stays a follow-up, not a
  fix for a bug.

### New known limitation

**A GitLab instance mounted under a path prefix is unaddressable.** An install
served at `https://example.com/gitlab/` has its API at
`https://example.com/gitlab/api/v4`, and `[HOST/]NAMESPACE/PROJECT` has nowhere
to put the prefix. `glab` has the same gap open
([#7920](https://gitlab.com/gitlab-org/cli/-/work_items/7920),
[#8146](https://gitlab.com/gitlab-org/cli/-/issues/8146)), so this is not a
solved problem being ignored. Closing it needs a separate API-base input, which
is a grammar decision, not a bug fix. Documented as a limitation.

### Still unverified after this pass

GitLab documents no status code or body for a stale `last_commit_id`, for a
missing target branch with no `start_*`, or for an empty-commit attempt without
`allow_empty`. The 400 the client classifies on is corroborated only by issue
threads. The `409` for both "merge request already exists" and "fork already
exists" is likewise secondary-source. The live e2e driver exists to close exactly
these.

## Open follow-ups

1. Honour `Retry-After` on 429 for both forges (currently fixed backoff).
2. A live end-to-end run against a real GitLab instance. Not performed here:
   `glab` is not installed on this machine and no GitLab credential is available
   in this environment.
3. CI-job-token announce path: untested, see Authentication above.
4. Group-membership fork reuse (a fork created into a group the credential is a
   member of but does not own) — the `owned=true` listing does not return it.
   Same v1 scope limit the donor records.
