# Plan: GitLab forge support for `ocx package announce`

## Status

- **Plan:** plan_announce_gitlab_forge
- **Active phase:** 1 — Contract (forge trait + coordinate grammar)
- **Step:** /swarm-plan → plan-approved
- **Last update:** 2026-08-22 (initialized)

---

## Overview

**Status:** Draft
**Author:** Claude (for Michael Herwig)
**Date:** 2026-08-22
**Related ADR:** `adr_announce_publisher_surface.md` (D5 — orchestration in `ocx_lib`),
`design_spec_announce_initiative.md` (S1/S3/S7/S12, X5/X6, C4/C6/C8/C15)
**Donor reference:** `../grimoire/src/catalog/forge.rs` + `.agents/adr/adr_announce_fork.md`
(D2/D4/D5/D6/D7 — proven GitLab MR + fork paths, same owner)

## Objective

Make `ocx package announce` open merge requests against a GitLab-hosted index
repository — fork path and fork-free path both — with the same C4/C6/C8/C15
contract the GitHub path already holds. This retires design-register S7's
"GitHub-only v1" and S1's blanket GitLab drop; the **git-subprocess** drop (S1)
and the always-a-pull-request narrowing of S3 stay.

## Scope

### In Scope

- A forge trait seam in `ocx_lib::forge`; `GitHubForge` becomes one impl.
- `GitLabForge`: REST v4 client covering the same ten operations.
- Coordinate grammar: optional host + arbitrary namespace depth (GitLab subgroups).
- Forge selection: host-derived default, `--forge` override.
- Base branch read from the forge (`default_branch`), replacing the hardcoded `main`.
- Fake-forge acceptance harness extended with a GitLab surface.
- Four gaps carried from the GitHub review (status-error body, default branch,
  `Retry-After`, self-fork guard) — fixed once in shared code, not per forge.

### Out of Scope

- Git-subprocess transport (S1 stands — REST only, both forges).
- Fork **policy** (`auto|never|always`) + push-permission probe. ocx's
  `--out` / `--fork` / fork-free trichotomy is the explicit equivalent.
- `grim`'s force-push model. ocx keeps compare-and-swap (see D3 below).
- GitLab CI job-token credential helper (git-transport-specific).
- Any change to the index wire format, root bytes, or CAS object layout.

## Research

Verified against live docs this session, not memory:

- **GitLab commits API** (`POST /projects/:id/repository/commits`): `actions[]`
  with `create`/`update`, `encoding: base64`, plus `start_branch` / `start_sha` /
  `start_project`. Critically, each action takes **`last_commit_id`** — per-file
  optimistic concurrency. That is the CAS primitive; see D3.
- **Fork API**: `POST /projects/:id/fork` is asynchronous, returns immediately;
  readiness is `import_status` on the new project. `GET /projects/:id/forks`
  supports `owned` + pagination for identity-based reuse (grimoire D6).
- **grimoire pushes with `git push --force`** (`index_announce.rs:566`). Do **not**
  port that: it clobbers a concurrent announce, which ocx's C4 forbids.

## Technical Approach

### D1 — Trait seam, not an enum

`announce()` calls exactly ten forge methods:
`find_fork`, `ensure_fork`, `sync_fork`, `ensure_push_access`, `get_ref_sha`,
`get_file_contents`, `compare_branch`, `find_open_pull_request`, `commit_files`,
`open_or_update_pull_request`.

Those ten become `trait Forge` (async, `&dyn Forge` at the `announce()` boundary —
one call site per run, so no monomorphisation win to chase). `GitHubForge` moves
under it unchanged; `announce()`'s signature becomes `Option<&dyn Forge>`.

This is a **pure refactor** and lands as its own commit before any GitLab code
(Two Hats). `ocx-mirror` links `announce()` — the signature change is the one
cross-repo ripple; internal structure has no stability, so no shim.

### D2 — Coordinate grammar grows a host and a namespace path

`RepoCoordinate` today is `owner/repo` and explicitly rejects a second slash
(`forge.rs:83`). GitLab subgroups (`group/sub/index`) make that wrong. It becomes
`{ host: Option<String>, namespace: String, project: String }` with
`full_path()` = `namespace/project`.

Host detection reuses the existing OCI-identifier heuristic
(`crates/ocx_lib/src/oci/identifier`) rather than a second parser — first segment
containing `.` or `:` is a host. `--index-repo gitlab.com/acme/tools/index`
therefore parses as host `gitlab.com`, namespace `acme/tools`, project `index`.

**Interface change** (CLI grammar + `--fork` value): `--index-repo` and `--fork`
accept a longer form. The old two-segment form still parses to the same value, so
existing invocations are unaffected — this widens the grammar, it does not break it.
Changelog line rides the commit subject.

### D3 — Concurrency on GitLab: `last_commit_id`, never `force`

Every announce commit updates `p/<package>.json` and creates CAS objects. The
`update` action for the root file carries `last_commit_id` = the commit the root
was read from. GitLab rejects the whole commit if the file moved since — which is
exactly `RefUpdate::FastForward`'s guarantee, expressed per file instead of per ref.

`RefUpdate::Reset` (the spent-branch repoint, #228) maps to the commits API's
`force: true` with `start_sha` at the index base. That is the **one** sanctioned
force, and only where the GitHub path already forces.

The rejection must surface as `ForgeError::NonFastForward` so `announce()`'s
existing re-read-and-regenerate retry fires unchanged. GitLab answers 400 with a
message naming the stale file; classify on that, and pin the classification with a
test that shows it both red and green.

### D4 — Per-operation mapping

| Trait method | GitLab REST |
|---|---|
| `get_file_contents` | `GET /projects/:id/repository/files/:path/raw?ref=` |
| `get_ref_sha` | `GET /projects/:id/repository/branches/:branch` → `commit.id` |
| `compare_branch` | `GET /repository/compare?from=&to=&straight=true` **twice** (both directions) → Identical/Ahead/Behind/Diverged; cross-project via `from_project_id` |
| `commit_files` | `POST /repository/commits` — one call, `actions[]` (D3) |
| `find_open_pull_request` | `GET /projects/:upstream/merge_requests?source_branch=&state=opened` |
| `open_or_update_pull_request` | `POST /projects/:fork_id/merge_requests` + `target_project_id`; 409 → reuse |
| `find_fork` | `GET /projects/:path` → `forked_from_project.id` == upstream id |
| `ensure_fork` | `POST /projects/:id/fork` + `namespace_path`; 409 → `GET /forks?owned=true` identity lookup (grimoire D6 — never a basename guess) |
| `sync_fork` | **No-op.** GitLab has no merge-upstream; `start_project` on the commits API lets a fork commit off an upstream SHA directly, so the object-reach problem `sync_fork` solves does not exist here. |
| `ensure_push_access` | `GET /projects/:id` → max(`permissions.project_access`, `group_access`) `access_level` >= 30 |

GitLab is *simpler* in two places (one-call atomic commit, no fork-network object
race) and harder in two (numeric project ids everywhere, URL-encoded paths).

### D5 — Identity and credentials

One env var stays: `OCX_ANNOUNCE_TOKEN`, forge-neutral. GitHub sends
`Authorization: Bearer`; GitLab sends `PRIVATE-TOKEN` (grimoire's split — a PAT
works either way, but `PRIVATE-TOKEN` also covers a CI job token). The
`ForgeToken` newtype and its redacted `Debug` are unchanged, and the token still
never enters a URL or argv (X6).

The `authorize` match over forge kinds is exhaustive — a wildcard would silently
send a future third forge's mutation **unauthenticated** (grimoire D8).

### D6 — Every X5 invariant re-proved per forge, not inherited

No-redirect client, response-body-only fork identity, parent verification, bounded
readiness poll, https-only endpoints. `poll.rs` and `identity.rs` are already
forge-neutral and are reused; the GitLab readiness classifier adds
`import_status == "failed"` fast-fail (grimoire D7) so a dead import does not eat
the 300s deadline.

### D7 — Forge selection

`--forge <github|gitlab>`, defaulted from the coordinate host: `github.com` →
GitHub, `gitlab.com` → GitLab, no host → GitHub (preserves today's default
`ocx-sh/index`). Any other host **requires** the explicit flag — inferring a forge
kind from a self-hosted hostname is a guess, and guessing wrong sends a credential
to the wrong API shape.

## Work Packages

File-disjoint, dependency-ordered. WP1 is a barrier; WP2–WP4 then run in parallel.

| WP | Files | Depends on |
|---|---|---|
| **WP1** — trait extraction + coordinate grammar (pure refactor, own commit) | `forge.rs`, `forge/github.rs`, `announce.rs`, `command/package_announce.rs`, `api/data/announce.rs` | — |
| **WP2** — shared-gap fixes (status body, `Retry-After`, self-fork guard, `default_branch`) | `forge/error.rs`, `forge/github.rs`, `forge/retry.rs` (new) | WP1 |
| **WP3** — `GitLabForge` | `forge/gitlab.rs` (new), `forge/gitlab/*.rs` | WP1 |
| **WP4** — fake-forge GitLab surface + acceptance tests | `test/tests/announce_helpers.py`, `test/tests/test_announce*.py` | WP1 |
| **WP5** — CLI wiring (`--forge`), docs, help text | `command/package_announce.rs`, `website/src/docs/reference/command-line.md` | WP2–WP4 |

## Testing Strategy

Contract-first: WP1 lands the trait with `GitLabForge` stubbed to
`todo!()`-free `Unsupported` errors; WP3's tests are written against the trait
before the impl.

- **Unit** — per-endpoint response parsing, the D3 stale-file → `NonFastForward`
  classification, fork parent/owner verification, readiness `failed` fast-fail.
  Each concurrency test must be shown **red** before green (a CAS test that cannot
  fail is the "unchecked green" class).
- **Acceptance** — the existing stdlib `ThreadingHTTPServer` fake forge grows a
  GitLab route table; every existing announce scenario runs against **both**
  surfaces from one parametrised fixture, so the two forges are held to one oracle.
- **Token-leak assertion** — `assert TOKEN not in stdout + stderr` on every GitLab
  test, matching the GitHub ones.

## Documentation Surfaces

- `website/src/docs/reference/command-line.md` — `package announce` flags.
- `website/src/docs/reference/environment.md` — confirm `OCX_ANNOUNCE_TOKEN` wording
  is forge-neutral.
- `.claude/rules/subsystem-cli-commands.md` — the `package announce` row.
- `.claude/artifacts/design_spec_announce_initiative.md` — S1/S3/S7 amendment note.
- ADR: amend `adr_announce_publisher_surface.md` (or a new `adr_announce_gitlab.md`)
  recording D1–D7.
- No `CHANGELOG.md` edit — the changelog line is the commit subject.

## Risks

| Risk | Mitigation |
|---|---|
| D3's stale-file rejection is classified from a message string; GitLab could reword it | Match on status + a narrow substring, and pin with a test that shows red; treat an unrecognised 400 as a hard error, never as "retry" |
| `RepoCoordinate` reshape touches the mirror repo | Internal structure has no stability (CLAUDE.md); update `ocx-mirror` in the same cycle |
| Two forges, one oracle, doubled acceptance runtime | Parametrised fixture, fake forge only — no real network |
| Self-hosted host → wrong forge kind | D7 refuses to guess; explicit `--forge` required |
