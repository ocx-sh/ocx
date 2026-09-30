# Research: Security of Automatic Deletion (ephemeral-channel prune)

## Metadata

**Date:** 2026-09-29
**Domain:** security
**Triggered by:** adr snapshot lifecycle
**Expires:** 2027-03-29

## Direct Answer

Workable, but three properties are missing from the codebase and must be designed in, not
retrofitted: (1) an **immutability rule for the `ephemeral` marker** — settable only at
announce time, on a non-release tag, by the index bot, never later; (2) a **credential
that can delete but cannot become anything else** — today's `ocx_announce` forge
credential ladder and `ocx_oci` auth store have no delete-scoped token concept; (3)
**blast-radius guards on the deletion path itself** (dry-run default, unmarked-tag
refusal, digest-shared-with-retained-tag refusal, prefix-collision refusal) —
`ocx_oci::client` has zero delete/DELETE call sites to build from today.

## Technology Landscape

| Tool/Pattern | Status | Relevance to OCX |
|---|---|---|
| GitLab **cleanup policies** (registry) | Mature, native | Regex allow/deny, keep-N, min-age; runs as a scheduled worker, not CI — sidesteps the CI-credential problem entirely. [docs](https://docs.gitlab.com/user/packages/container_registry/reduce_container_registry_storage/) |
| `regctl` / `crane` delete | Mature CLIs | One DELETE per manifest; "delete matching a glob" logic is left to the caller's script, same as this design |
| `snok/container-retention-policy` | Widely used Action | `dry-run: true` **default**; docs warn filters combine as OR — direct precedent for the `mr1`/`mr12_*` collision risk. [README](https://github.com/snok/container-retention-policy) |
| `actions/delete-package-versions` (GHCR) | Established | Needs `delete:packages`, a scope the default `GITHUB_TOKEN` cannot hold even with `packages: write` |
| JFrog Artifactory cleanup | Mature, commercial | Dry-run report is a first-class mode, not a flag to remember |

## Findings by axis

### 1. Credential scope

- **`ocx_oci` has no delete verb today** (checked `client.rs`/`client/*.rs` — zero
  registry-DELETE hits). Whatever token gates deletion is new surface, not an extension.
- **GitLab scopes**: deploy tokens/project access tokens top out at `write_registry`;
  registry delete needs `admin_container_registry` (or PAT `api`) — a deploy token cannot
  hold it. `CI_JOB_TOKEN` **can** delete by default (inherits the triggering user's
  registry perms) unless the project's inbound job-token allowlist restricts it (off by
  default pre-GitLab 15.x settings review). This is the laziest and most dangerous default:
  reaching for `CI_JOB_TOKEN` for both the registry DELETE and the index PR needs no
  provisioning and has the broadest blast radius.
- **Reusable precedent already in-tree**: `ForgeCredentials::resolve`
  (`crates/ocx_announce/src/forge/credentials.rs:127`) treats `CI_JOB_TOKEN` as API-scoped
  only under `WriteTransport::Git` + `GITLAB_CI`, and records `publishing_project` from
  `CI_PROJECT_PATH` "checked against the index project's job-token allowlist"
  (`credentials.rs:44`). This is the one guard that already answers "which project's job
  token may act on the index repo" — extend it to gate the registry DELETE too, which
  today has no analogous check because it has no delete path.
- **`environment:on_stop` jobs may run against a deleted ref** — GitLab's own docs warn
  `git checkout` fails in `on_stop` unless `GIT_STRATEGY: none` ([docs](https://docs.gitlab.com/ci/environments/#stopping-an-environment)).
  The channel identifier must travel as a CI variable captured at environment-creation
  time, never be re-derived from repo content at stop time.
- **GHCR**: `delete:packages` is not in `GITHUB_TOKEN`'s reach; a PAT/App token is
  required, reopening the same "which credential" question, worse — long-lived PAT.

### 2. Blast-radius guards

- **Dry-run default is the house style**: `clean.rs:19-24` (`--dry-run` previews GC),
  `package_copy.rs:71-72`, `index_sync.rs:24-30`, `config_setup.rs:92-94` (exit 82 on an
  untouched dirty fence without `--force`). `ocx package prune` should match: no delete
  without an explicit confirmation flag.
- **Enumerate by allowlist query**, never denylist-subtract: only ever select
  `ephemeral=true` tags under an exactly-matched channel. An allowlist bug returns too
  few candidates (fails safe); a denylist bug returns too many (fails unsafe).
- **Channel-name collision** (`0.5.0-mr1` matching `0.5.0-mr12_*`) is the exact failure
  `snok/container-retention-policy`'s OR-filter warning describes. Fix: exact/anchored
  match (`^{channel}(/|$)`) or a structured OCI annotation (`sh.ocx.channel=<value>`),
  never a bare glob/prefix on the tag string.
- **Digest shared with a retained/release tag**: registries are content-addressed;
  deleting a tag must not orphan a manifest another surviving tag still points at. Check
  both the registry's tag-list-by-digest **and** the local index's reverse mapping before
  DELETE — `subsystem-oci.md`'s "local index IS the package-tier lock" means a digest
  pinned by any committed root elsewhere must survive even if every ephemeral tag
  pointing at it is gone.
- **Idempotency / max-deletions cap**: refuse a single invocation whose candidate count
  exceeds what `--retain` implies — turns a mis-scoped `--channel` into a loud failure
  (`PolicyBlocked = 81`) instead of silent mass deletion.

### 2b. Tamper paths

- **Channel derived from branch/MR name → injection**: an MR author fully controls a
  branch name with no elevated permission. Sanitize to a narrow charset before any
  pattern use — same fix serves the glob-collision guard above.
- **Marking a release tag `ephemeral` to delete it later**: governance rule needed —
  `ephemeral` is set once, at announce time, by the index bot itself (never from CI-
  supplied metadata), and is immutable thereafter. Two structural rules: (a) a
  release-shaped channel must be structurally ineligible to carry `ephemeral: true` —
  enforce as a parse-time invariant in the `ocx_package` channel type, not a runtime
  warning; (b) the prune job must look up the marker from the index/registry's own
  record, never accept an `--assume-ephemeral` argument from its own CI variables.
- **Index bot accepting removals** needs the same review posture `claim/owners.rs` and
  `root.rs` already give ownership mutations — re-verify the marker against the bot's own
  history at merge time, not just trust the PR diff. This is a governance rule to design;
  `announce.rs`/`claim.rs` today write and verify ownership, not deletions.

### 3. Existing OCX surfaces to reuse

- **`ocx_oci::ssrf`** already guards every host OCX dials; the DELETE call inherits it for
  free **provided** it is added to `Client` as a normal transport call, not a bespoke
  bypass.
- **Exit codes** (`.claude/rules/quality-rust-exit_codes.md`): `PolicyBlocked = 81` fits
  "refused: unmarked tag" / "refused: digest shared with a retained tag"; `AuthError = 80`
  fits a token lacking delete scope. No new code needed — 79-84 already covers it.
- **`ocx_trust`**'s tiered `[[trust.policy]]` shape is a reasonable template for a future
  config-driven `[[prune.policy]]`, if channel/host scoping should live in config rather
  than the CI job definition.

### 4. Audit trail

- **Route the index-side removal through the same forge-PR path `announce`/`claim`
  already use** (`crates/ocx_announce/src/forge.rs`, `announce/pipeline.rs`) — every
  removal becomes an attributable, reviewable git commit, never a direct write to the
  index's default branch.
- **Registry-side DELETE has no trail by default** beyond the CI job log (finite
  retention). Put a structured record of what/when/why in the same index PR body/commit —
  reuses the durable surface that already exists rather than inventing a new log.
- **Signed provenance survives deletion**: Rekor is append-only and does not delete
  entries when the signed artifact is removed. A consumer verifying then fetching a pruned
  digest gets a clean verify followed by a 404 — expected for ephemeral artifacts, but the
  design should say explicitly that an ephemeral digest must never be pinned by an
  external `ocx.lock`, since nothing today enforces that.

## Key Findings

1. `ocx_oci::client` has zero registry-delete call sites — the DELETE path, its
   credential gate, and its SSRF-guard reuse are all new, not tested extensions.
2. `ForgeCredentials::resolve` (`crates/ocx_announce/src/forge/credentials.rs:127`)
   already implements the job-token-allowlist shape the registry DELETE needs too.
3. `on_stop` jobs may run against a deleted ref — channel identity must be a CI variable
   captured at environment creation, never re-derived from repo content.
4. Every existing OCX destructive command defaults to dry-run or requires `--force`
   (`clean.rs`, `package_copy.rs`, `index_sync.rs`, `config_setup.rs`) — match it.
5. `snok/container-retention-policy` and GitLab cleanup-policy regex reports both
   document the exact prefix/glob-collision failure this task's example describes; the
   industry-wide fix is exact/anchored matching or a structured field.

## Recommendation

Build it, gated behind four deliverables none of which exist yet:

1. **`ephemeral` is bot-set-once-immutable**, structurally impossible on a release-shaped
   channel, re-verified by the index bot at merge time from its own history.
2. **Enumerate by allowlist query** (exact-matched, sanitized channel), refuse (exit 81)
   on any digest still referenced by a retained tag or a pinned index entry.
3. **Dry-run default** matching `clean.rs`/`package_copy.rs`/`index_sync.rs`, plus a
   max-deletions cap.
4. **One credential-scope gate for both write paths** — extend `ForgeCredentials`'
   inbound-job-token-allowlist pattern to the registry DELETE, and route the index-side
   removal through the same reviewed forge-PR mechanism `announce`/`claim` use.

| Threat | Guard | Where enforced |
|---|---|---|
| Unmarked/release tag deleted | Allowlist query on `ephemeral=true`; structural ban on marking release channels ephemeral | New channel-type invariant + bot re-verification at merge |
| Channel-name glob collision | Exact/anchored match or structured annotation, never bare prefix/glob | New prune-selection code, sanitize CI-supplied string first |
| Digest shared with retained/release tag | Cross-check registry tag list **and** local index pins before DELETE | New delete path, consulting `ocx_index` per `subsystem-oci.md` |
| CI credential deletes beyond its project | Reuse `ForgeCredentials` inbound-allowlist check on the DELETE, not just the index PR | `forge/credentials.rs` pattern, extended into `ocx_oci` |
| `on_stop` runs against a deleted ref | Channel identifier as a CI variable from environment creation, never re-derived | `.gitlab-ci.yml`, not OCX code |
| MR author tampers via branch-derived channel | Sanitize to narrow charset before any pattern use | Prune command argument parsing |
| Mass/accidental deletion from mis-scoped invocation | Dry-run default + confirmation flag + max-deletions cap | New `ocx package prune`, mirroring `clean.rs` UX |
| Index bot merges a tampered/stale removal PR | Re-verify marker + eligibility at merge, same posture as `claim` | Index bot-tools, governance parallel to `claim/owners.rs` |
| Signed artifact deleted, Rekor entry survives | Document: ephemeral digests must never be pinned externally | ADR prose, not code-enforceable today |

## Sources

| Source | Type | Relevance |
|---|---|---|
| [GitLab cleanup policies](https://docs.gitlab.com/user/packages/container_registry/reduce_container_registry_storage/) | Docs | Regex allow/deny precedent |
| [GitLab deploy tokens](https://docs.gitlab.com/user/project/deploy_tokens/) | Docs | Delete-scope ceiling |
| [GitLab CI job token](https://docs.gitlab.com/ci/jobs/ci_job_token/) | Docs | Inbound allowlist, fork MR behavior |
| [GitLab stopping an environment](https://docs.gitlab.com/ci/environments/#stopping-an-environment) | Docs | `on_stop` ref/checkout caveat |
| [snok/container-retention-policy](https://github.com/snok/container-retention-policy) | README | Dry-run default, OR-filter collision |
| [actions/delete-package-versions](https://github.com/actions/delete-package-versions) | README | GHCR `delete:packages` requirement |
| `crates/ocx_announce/src/forge/credentials.rs` | Local | Job-token/allowlist pattern to extend |
| `crates/ocx_oci/src/client.rs`, `ssrf.rs` | Local | No delete path; SSRF reused if routed through `Client` |
| `.claude/rules/subsystem-oci.md` | Local rule | Index-is-the-lock digest invariant |
| `.claude/rules/quality-rust-exit_codes.md` | Local rule | `PolicyBlocked=81`, `AuthError=80` fit |
| `crates/ocx_cli/src/command/clean.rs` etc. | Local | Dry-run/`--force` UX precedent |
