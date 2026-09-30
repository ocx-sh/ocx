# Discussion: Snapshot lifecycle — rolling-window pre-releases with automatic removal

State: handed-off → architect · Updated: 2026-09-29
Ratified: 2026-09-29 → architect
Confidence: owner ratified; research vintages 2026-09-29 (codebase recon @ 59b8b579d, prior-art web scan)
Participants: owner + hex-discuss; 2026-09-29

## Intent

Company setup: CI pushes packages and announces them to a *dev* index; a promote job copies dev → production. Beyond releases, CI produces pre-release snapshots on main (`canary` channel) and per merge request. Wanted: push + announce those snapshots, and have an automated, legitimate path that removes old ones from the registry **and** from the index. Out of scope: production-index semantics (stay additive-only, yank manual).

## Requirements

- A rolling channel tag (name configurable; company uses `canary`, not `main`) always points at the newest snapshot.
- Every snapshot also gets a unique, immutable build tag (pipeline id or timestamp) — fast-iterating test environments hit stale caches when they re-pull a moving tag, so they pin the build tag.
- A rolling window: keep the last N build tags per channel; older ones are removed from the registry and the index automatically.
- MR channels disappear entirely when the MR closes/merges.
- Scale: large monorepo, thousands of MRs — the index must not grow with removed snapshots.
- Removal must be honoured by every other command that reads or writes tags (owner, 2026-09-29, during the ADR run): `ocx index sync`/`update`/`regenerate`, `announce --tags-from-registry`/`--refresh`, `package copy` and registry→registry sync, cascade repair, ocx-mirror sync, local index collection — none may resurrect or keep a pruned ephemeral tag.
- **The ADR must carry human-readable, concrete examples** (owner, 2026-09-29): the CLI invocations, the GitLab CI jobs, the index-root excerpt, and any config — written as a user would type/read them, not only prose or grammar. The sketches below are **illustrative starting points, not decisions**; flag names, the marker name and the prune shape are the ADR's to settle.

  CI — canary on main (push, announce as ephemeral, keep last 10):

  ```yaml
  canary:
    rules: [{ if: '$CI_COMMIT_BRANCH == "main"' }]
    script:
      - ocx package push --cascade ghcr.example/acme/tool:0.5.0-canary_${CI_PIPELINE_ID} ./bundle
      - ocx package announce acme/tool --tags 0.5.0-canary_${CI_PIPELINE_ID},0.5.0-canary --ephemeral
      - ocx package prune acme/tool --channel 0.5.0-canary --retain 10
  ```

  CI — per-MR snapshot plus teardown on MR close/merge:

  ```yaml
  mr-snapshot:
    rules: [{ if: '$CI_PIPELINE_SOURCE == "merge_request_event"' }]
    environment: { name: "snapshot/mr-${CI_MERGE_REQUEST_IID}", on_stop: mr-snapshot-stop }
    script:
      - ocx package push --cascade ghcr.example/acme/tool:0.5.0-mr${CI_MERGE_REQUEST_IID}_${CI_PIPELINE_ID} ./bundle
      - ocx package announce acme/tool --tags 0.5.0-mr${CI_MERGE_REQUEST_IID}_${CI_PIPELINE_ID},0.5.0-mr${CI_MERGE_REQUEST_IID} --ephemeral
      - ocx package prune acme/tool --channel 0.5.0-mr${CI_MERGE_REQUEST_IID} --retain 5

  mr-snapshot-stop:
    rules: [{ if: '$CI_PIPELINE_SOURCE == "merge_request_event"', when: manual }]
    environment: { name: "snapshot/mr-${CI_MERGE_REQUEST_IID}", action: stop }
    script:
      - ocx package prune acme/tool --channel 0.5.0-mr${CI_MERGE_REQUEST_IID} --all   # builds + rolling tag
  ```

  Index root excerpt — marked vs unmarked tags:

  ```json
  "tags": {
    "0.5.0":                  { "content": "sha256:aa…", "observed": "2026-09-20T10:00:00Z" },
    "0.5.0-canary":           { "content": "sha256:c3…", "observed": "2026-09-29T08:12:00Z", "ephemeral": true },
    "0.5.0-canary_184467":    { "content": "sha256:c3…", "observed": "2026-09-29T08:12:00Z", "ephemeral": true },
    "0.5.0-canary_184402":    { "content": "sha256:b7…", "observed": "2026-09-28T17:40:00Z", "ephemeral": true }
  }
  ```

  Consumer — a test environment pins the immutable build tag, never the rolling one:

  ```toml
  # ocx.toml
  [tools]
  tool = "acme/tool:0.5.0-canary_184467"
  ```

  Dry run output the prune should give a human:

  ```text
  $ ocx package prune acme/tool --channel 0.5.0-canary --retain 2 --dry-run
  keep    0.5.0-canary_184467   sha256:c3…  (newest)
  keep    0.5.0-canary_184402   sha256:b7…
  remove  0.5.0-canary_184311   sha256:9e…  index + registry
  remove  0.5.0-canary_184290   sha256:41…  index + registry
  skip    0.5.0                 not ephemeral
  ```

## Decisions

- Tag shape reuses existing pre-release-with-build versioning: `<ver>-<channel>_<buildid>` cascades to `<ver>-<channel>` (rolling). No new tag grammar. (Owner confirmed this matches the intended "only roll the build identifier" behaviour.)
- No tombstones: owner rejected them on growth (thousands of MRs). Supported by recon — `ocx.lock` pins digests and is index-free, so a tombstone would only buy a nicer error on re-resolve. Removed ephemeral tags are deleted from the index root outright.
- Automatic removal is an opt-in per-index policy scoped to ephemeral tags; production indexes keep D-additive-only / D-yank-not-delete / C7 unchanged.
- Outcome: ADR (reverses recorded announce decisions for the opted-in scope).
- First-class ocx support, not a registry-policy workaround (owner: "this is a very common use case"). An `ocx package prune` with a retain count (keep last N builds per channel, ordered by the existing version sort — build ids are timestamps/pipeline ids, so lexicographic build order = age), plus announcing the deletion so the index drops the tags.
- Ephemeral is **marked at announce time** by the publisher (per-tag marker in the index root) — not a pattern, not version shape. Only marked tags may ever be removed automatically; unmarked tags keep D-additive-only / D-yank-not-delete / C7.
- MR channel teardown: GitLab environment `on_stop` action job (runs when the MR is merged/closed) calls prune for the whole MR channel. Canary channel: prune with retain N after each push.

## Threads

- Who deletes registry tags — closed: ocx (`package prune`).
- How "ephemeral" is declared — closed: announce-time marker.
- MR teardown trigger — closed: GitLab `environment:on_stop` job.
- Command shape — open: prune does registry + index in one call, vs prune (registry) + `announce --remove` (index).

## Research

- `.agents/research/snapshot-lifecycle-codebase-recon.md`
- `.agents/research/snapshot-lifecycle-prior-art.md`

## Related

- `.claude/artifacts/adr_announce_publisher_surface.md` (D-additive-only, D-yank-not-delete, `--tags-from-registry`)
- `.claude/artifacts/design_spec_announce_initiative.md` (C7 yank never automatic)
- `.claude/artifacts/adr_announce_gitlab_forge.md` (company forge)
- `.claude/artifacts/adr_index_indirection.md` (index format, yank semantics)
- `crates/ocx_cli/src/command/package_announce.rs`, `crates/ocx_index/src/regenerate.rs`, `crates/ocx_index/src/ocx_index.rs`

## Open questions

- [NEEDS CLARIFICATION: is a bare `canary` tag (no version) wanted, or `<ver>-canary`?] Recommended: `<ver>-canary` — bare non-version tags get no cascade and need new grammar.
- `__ocx.keep.*` tags block storage reclaim; snapshots likely push with `--no-keep-tag` or the prune removes them.
- OCI digest DELETE hits every tag sharing the digest — the newest build shares its digest with the rolling tag. Prune must delete by tag, or by digest only when no retained tag references it.
- Registry tag DELETE is optional in the distribution spec (400/405 allowed); GitLab supports it. Fallback behaviour for registries that refuse it — ADR decides.
- Removal order: index first, then registry (a registry-first delete leaves a dangling index entry, today an index-bot anomaly).
- [NEEDS CLARIFICATION: does the company run the index bot, or only `ocx package announce` against a forge?] Recommended: design the bot-side acceptance rule anyway (ocx-sh/index needs it) — removal of a tag is allowed iff it carries the ephemeral marker.
- Name of the marker and whether it carries an expiry/channel — ADR decides (wire-format change to the index root; published roots must keep parsing).
- Rolling tag after its channel is fully pruned (MR close): removed too, not left dangling.

## Verification

- Acceptance tests against the local registry + fake forge: push N+k canary builds, prune `--retain N` → registry and index hold exactly the newest N build tags plus the rolling tag; rolling tag digest unchanged.
- MR teardown: prune whole channel → no `-mr<id>` tags left in registry or index root; unrelated channels and release tags untouched.
- Guard: prune/remove of an **unmarked** tag is refused (exit code per `quality-rust-exit_codes.md`); production-style index without markers is byte-identical after a prune run.
- A consumer locked to a pruned build digest still installs from local store; re-resolve of a pruned tag fails with a clear not-found.
- Index root parse: roots without the new marker still parse (published-package read-path compatibility).
