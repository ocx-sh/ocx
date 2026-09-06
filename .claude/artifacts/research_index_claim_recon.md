# Research: index claim command — codebase recon

## Metadata

- Date: 2026-09-04
- Expires: 2027-03-04 (re-verify: GitLab job-token scope and push-option availability move per minor release)
- Author: hex-discuss recon lane (researcher, sonnet); persisted by the orchestrator — the worker had no write tool
- Discussion: `.agents/discussions/index-claim-command.md`
- Repos inspected: ocx worktree `ocx-sion` (branch `sion`, HEAD `487570fb`); ocx-indexbot (HEAD `1b1c83e6`, v0.6.0); index (local checkout HEAD `9eba2cc3`, dated 2026-08-27 — stale vs origin, see Direct answer); ocx-catalog (HEAD `1c8b90c7`, branch `perf/catalog-at-corporate-size`)

## Direct answer

Today `ocx package announce` **refuses** to create a first claim — `require_root` (`crates/ocx_lib/src/announce/pipeline.rs:123-134`) raises `AnnounceError::UnclaimedNamespace` whenever the base ref carries no committed root. A claim command is new surface, not a flag on `announce`. It would reuse almost everything downstream of "the root already exists": the `Forge` trait, both backends, the hardened HTTP client, `RepoCoordinate`/`ForgeKind` resolution, the atomic-commit + open-PR machinery, and the acceptance fixture (which already speaks both GitHub and GitLab REST). Owner-identity spelling is `login`/`id` since indexbot 0.5.0, with `github`/`github_id` derived and dual-emitted; ocx's `IndexRoot` has no `owners` field at all (fleet forward-compat), so a schema change touches only `ocx-sh/indexbot` + `ocx-sh/index`, never `ocx`/`ocx-mirror`. Three index-repo docs pages (`claim-a-namespace.md`, `namespace-policy.md`, `entry-schema.md`) still show only the pre-0.5.0 `{github, github_id}` spelling in the local checkout. Orchestrator addendum: the **live** `schema/root.schema.json` on `ocx-sh/index` main (fetched 2026-09-04 via the GitHub API) already carries `anyOf [{required: [login, id]}, {required: [github, github_id]}]`, so a `login`/`id`-only owner validates today.

## Key findings

### 1. Claim flow today, end to end

- `site/src/docs/how-to/claim-a-namespace.md:6-9` — "Claiming a namespace is opening the PR that adds the **first** `p/<namespace>/<package>.json` entry under it. There is no separate reservation step."
- Required fields at first claim (`claim-a-namespace.md:20-38`): `name`, `repository`, `owners` (≥1 pair), `status: active`, `deprecated_message: null`, `created`, `upstream` (mandatory for third-party namespaces), `desc: null`, `tags: {}` (or bot/`seed-import`-populated if batch seeding).
- Human vs bot: `owners`/`repository`/`status`/`deprecated_message`/`created`/`upstream`/`superseded_by` = human via PR; `desc`, `source`, and every `tags[*]` field except `yanked` = bot-regenerated (`site/src/docs/reference/entry-schema.md:180-190`, mirrored in `schema/root.schema.json`).
- A claim PR carrying tags: the how-to forbids hand-writing `tags[*].content`/`observed`; a claim wanting an initial tag set should let an announce or `seed-import` populate it (`claim-a-namespace.md:39-42`).
- G-04 (`governance-contracts.md:19`): a new `p/*.json` entry always gets `new-package` + a red `governance/review-required` status that **never** auto-resolves — new-package PRs are never auto-merged, however many owners are listed (`claim-a-namespace.md:87-92`).
- ND-4 reserved segments — **doc/code mismatch**: `namespace-policy.md:70` lists `p, o, c, docs, assets, config, schema, api, static, data` (10). The enforced set in `ocx-indexbot/src/ocx_indexbot/core/validate_entry.py:116-149` (`ALWAYS_RESERVED_SEGMENTS`) has 11: those plus `"index"`.
- ND-4 gates *claiming*, not *updating*: `validate.py:127-160` (`_is_announce_shaped_update`) lets an announce-shaped refresh to an already-committed root under a reserved segment through without `--allow-reserved-namespace`, scoped to `core/diff.classify_change`'s `"refresh"` verdict; `repository`/`owners`/`status` changes still route to the human lane.

### 2. Owner identity schema status per repo

- `ocx-indexbot/.claude/artifacts/adr_forge_neutral_owners.md` (Accepted 2026-08-25, landed 0.5.0): D1 renames `github`/`github_id` → `login`/`id`; D2 = read `login` wins/fallback `github`, emit both (legacy derived, never independently settable), refuse on disagreement. `model.Owner` (`src/ocx_indexbot/model.py:59-86`) is `Owner(login: str, id: int)` only; both spellings live in the codec `core/validate_entry.py:523-562` (`_owner_to_dict`/`_owner_from_dict`).
- `index/schema/root.schema.json:111-141` (`$defs.owner`) is current: `login`/`id` canonical since 0.5.0, `github`/`github_id` accepted + derived-emitted, `anyOf` requiring either pair.
- `index/.github/maintainers.yml` — migrated, `login:`/`id:` only.
- **Stale docs**: `index/site/src/docs/reference/entry-schema.md:82-87` Owner table shows only `github`/`github_id`; `how-to/claim-a-namespace.md:23,46-56` and `reference/namespace-policy.md:116-121` likewise. (Local checkout 2026-08-27 — re-verify against origin before filing.)
- ocx: `crates/ocx_lib/src/oci/index/wire.rs` `IndexRoot` (`:145-159`) has no `owners` field; `index_root_tolerates_unknown_fields_for_fleet_forward_compat` (`:475-496`) feeds a wrong-shaped `owners: ["alice"]` and asserts the parse succeeds. `announce/pipeline.rs:586` carries `owners[]` through as opaque `serde_json::Value`. Test fixtures at `pipeline.rs:842`, `:1628`, `announce.rs:781` still use `{"github": "alice", "github_id": 1}`.
- ocx-mirror: not inspected; the ADR's consequences table (`adr_forge_neutral_owners.md:98`) says "none" (`index_write.rs` preserves `owners` opaque). Unverified here.
- **ADR contradiction**: `adr_forge_neutral_owners.md:99` says `@ocx-sh/catalog` "omits `owners` entirely". False on the current `ocx-catalog` tree: `src/theme/composables/usePackageRoot.ts:9-25` defines `Owner { login?, id, github?, github_id? }` + `ownerLogin()` (`login ?? github`), and `src/theme/components/detail/MetaRail.vue:210,226-227,373-376` renders `owners[]` as `@login` links. `MetaRail.vue:222` self-documents a KNOWN GAP: the profile href is hard-coded to `https://github.com/<login>`, so a GitLab-sourced index renders wrong owner links today.
- G-19 matches on `owner.id` (numeric), never `login` — `cli/governance_check.py:130` — consistent across schema/ADR/code.
- Readers to touch when the legacy pair stops being emitted: indexbot codec, `root.schema.json`, the three stale docs pages; `ocx`/`ocx-mirror`/`ocx-catalog` need no code change (catalog already falls back across spellings).

### 3. Reuse surface in ocx for a claim

- `Forge` trait (`crates/ocx_lib/src/forge/api.rs:125-300`) — eleven async ops: `get_file_contents`, `get_ref_sha`, `compare_branch`, `find_open_pull_request`, `pull_request_mergeability`, `find_fork` (read-only), `ensure_fork`, `sync_fork`, `ensure_push_access`, `commit_files` (atomic, one commit, all files), `open_or_update_pull_request`. `get_file_contents` already returns `Option<Vec<u8>>` (`None` = absent), so the trait expresses "no root yet" without change.
- `GitHubForge`/`GitLabForge` implement it; `ForgeKind` (`forge/kind.rs:16-146`) resolves `github.com`/`gitlab.com`/declared-for-self-hosted, builds the client, validates coordinate flatness (GitHub) vs nesting (GitLab). `forge/identity.rs` — fork-parent + namespace verification, both wire spellings. `forge/http.rs:33-41` — one hardened `reqwest::Client` (no-redirect, embedded Mozilla roots), structurally pinned.
- Token: `OCX_ANNOUNCE_TOKEN` (`crates/ocx_cli/src/command/package_announce.rs:21,205-215`), read once, never forwarded to subprocesses; `subsystem-cli.md`'s credential-exemption table lists it.
- `announce/pipeline.rs:123-134` `require_root` is the exact seam a claim inverts: `None` bytes → `UnclaimedNamespace`; the claim's job is precisely that `None` case.
- `announce.rs:69-172` `announce()` — fork-identity resolution (read-only `find_fork`, C6), branch-state resolution (`BranchComparison`/`Mergeability`; #399's fix landed), atomic `commit_files`, `open_or_update_pull_request`. A claim wants a thinner slice (no accumulation history on a first commit) but the primitives are directly reusable.
- CLI placement: `ocx package announce` is a `Package` enum variant (`crates/ocx_cli/src/command/package.rs:16-20`), sibling to `Attest`, `Cascade`, `Copy`, `Create`, `Description`. Every `ocx index` subcommand (`catalog`, `list`, `update`, `sync`, `regenerate`) operates on the local `$OCX_HOME/index/` cache only — none touches a forge. A claim belongs under `ocx package`.
- `--forge`/`--index-repo`/`--fork` flag shapes and the `same_host`/`validate_coordinate` guards (`package_announce.rs:171-200`) are copy-adaptable.
- Acceptance fixture: `test/tests/fake_forge.py` (`FakeForge(GitLabRoutes, http.server.ThreadingHTTPServer)`, `:231-235`) — one HTTP fake speaking both GitHub `/repos/...` and GitLab `/projects/...` REST on one port, alongside `test/tests/fake_gitlab.py`. Simulates fork lookup/create, branch compare, commit_files (incl. GitLab's create-vs-update probe), PR/MR open, reviewer/approval, identity endpoints. It does **not** simulate a git-push transport — D0 in `adr_announce_gitlab_forge.md` ("S1: REST only, no git subprocess") means announce never shells to `git`. A git transport has zero existing ocx-side test infrastructure.

### 4. Relevant ADRs/plans

- `adr_announce_gitlab_forge.md` (Accepted 2026-08-22) — D0 amends S1/S3/S7 (S7 "GitHub-only" retired; S1 "REST only, no git subprocess" stands for both forges; S3 "always fork" → "always a reviewed PR"); D1 `Forge` is a trait so orchestration never learns which forge (rejected: an enum threaded through); D2 `RepoCoordinate = {host: Option<String>, namespace, project}`, GitLab namespaces may nest; D3 forge kind declared for self-hosted, never probed.
- `adr_announce_publisher_surface.md` (Accepted 2026-07-18/19) — "zero index-side credential, zero hosted moving part"; lib-hosts-substance/CLI-thin doctrine; ports the index repo's ADR-6 fork-PR contract.
- `adr_announce_diverged_branch_rebuild.md` — landed: `announce.rs:130-137` (`pipeline::carry_branch_tags`). Closes ocx#399. `plan_issue_batches_2026-09-04.md:255-330` still records #399 `NOT_STARTED` — stale (fix `6feb9c02` landed after the plan was written).
- `plan_issue_batches_2026-09-04.md` does not mention #410 or #411.
- `design_spec_announce_initiative.md`, `handoff_announce_initiative_state.md` — index-side governance-gate state; not about a claim command. No plan artifact scopes a claim command by name.

### 5. G-19 identity mechanism / CI env vars

- G-19 obtains the PR author's numeric id from the forge REST response body: `ocx-indexbot/src/ocx_indexbot/adapters/github_api.py:403-404` — `PullRequestInfo(author_login=payload["user"]["login"], author_id=payload["user"]["id"])`; `cli/governance_check.py:103-132` matches `info.author_id` against `{owner.id for owner in root.owners}` read from the base ref.
- `GITHUB_ACTOR_ID`, `GITLAB_USER_ID`, `GITLAB_USER_LOGIN`, `CI_JOB_TOKEN` — zero code hits in ocx, indexbot, catalog. Only prose: `website/src/docs/reference/environment.md:128` (job token cannot be `OCX_ANNOUNCE_TOKEN` — no repository-files/commits/branches/MR scope), `research_gitlab_forge_api.md`, `adr_announce_publisher_surface.md`. `ocx-indexbot/src/ocx_indexbot/adapters/gitlab_api.py:1-7`: auth is `PRIVATE-TOKEN` (PAT), "never the `CI_JOB_TOKEN`".

### 6. Documentation surfaces

- ocx: `website/src/docs/reference/command-line.md:2693-` (`ocx package announce` grammar + three examples: GitHub fork, GitLab `--forge gitlab`, GitHub direct); `reference/environment.md:117-137` (`OCX_ANNOUNCE_TOKEN`, scope table, job-token caveat). A claim command needs a `## ocx package claim` block + an `environment.md` mention.
- index repo: `site/src/docs/how-to/claim-a-namespace.md` is the canonical claim doc, written from the hand-written-PR perspective — the primary doc surface this initiative rewrites. `namespace-policy.md`, `entry-schema.md` need the `login`/`id` migration regardless.

## negative:

- `plan_issue_batches_2026-09-04.md` records #399 as `NOT_STARTED`; fixed on main as of `6feb9c02`.
- Issues #410/#411 appear nowhere in either repo's `.claude/artifacts/`.
- `adr_forge_neutral_owners.md`'s consequences table is wrong about `@ocx-sh/catalog` (renders owners; GitHub-only href is a live forge-neutrality gap, self-documented at `MetaRail.vue:222`).
- Three index-repo docs pages stale relative to the 0.5.0 rename (local checkout; re-verify against origin).
- `namespace-policy.md`'s reserved-segment table is missing `"index"`, enforced in code.
- `git fetch --dry-run` on the index checkout not run; ocx-mirror not inspected.

## leads:

- claim-inverts-require_root — the claim feature is "what happens when `require_root` gets `None`"; a sibling entry point sharing everything downstream is the smallest reuse surface.
- catalog-github-hardcoded-href — its own issue, independent of claim: a GitLab-sourced index renders wrong owner links today.
- stale-owner-docs — companion docs PR on the index repo alongside the claim command.
- no-git-push-fixture — any git-push transport needs new acceptance infrastructure; `fake_forge.py`/`fake_gitlab.py` only simulate REST.
- 399-plan-drift — re-verify every `NOT_STARTED` verdict in the 2026-09-04 batch plan against current `main` before reuse.
