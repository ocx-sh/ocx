# Research: `publish-to-bcr` → OCX Announce Transfer Map

<!--
Long-lived planning reference. The "we want something similar — what do we
take?" companion to research_publish_to_bcr_anatomy.md. Each verdict cites the
exact OCX surface (existing or planned). Write for a reader with zero session
context.
-->

## What this document is

The transfer layer over
[`research_publish_to_bcr_anatomy.md`](./research_publish_to_bcr_anatomy.md): for
every design problem BCR solves, what OCX does or should do, and whether we
**adopt / adapt / avoid / are already stronger**. The design authority this maps
into is the publisher-side ADR
[`adr_announce_publisher_surface.md`](./adr_announce_publisher_surface.md) (D1–D5)
and the index-side ADR-6
[`adr_fork_pr_announce.md`](../../index/.claude/artifacts/adr_fork_pr_announce.md)
(FP-1…FP-9, G-19/G-20); the Rust client is tracked as
[ocx#216](https://github.com/ocx-sh/ocx/issues/216).

**Date:** 2026-07-18

**OCX surfaces referenced (by their ADR names):**

- `ocx package announce` — the Rust client command (ADR D5, ocx#216).
- `ocx package push --announce-file <path>` — appends pushed primary+cascade tags
  to a per-package file (ADR D5, constraint).
- **index `validate.yml`** — the two-job CI on the index repo: `verify-claims`
  (unprivileged, PR-head, re-derives from registry) + `governance-gate`
  (`pull_request_target`, GITHUB_TOKEN only) (ADR-6 FP-1/FP-4/FP-5/FP-7).
- **G-19 machine lane** — auto-merge iff PR author's `github_id` ∈ committed
  `owners[]` and no G-05 key touched (ADR-6 FP-5).
- **G-20 maintainers-YAML** — human-lane reviewer assignment (ADR-6 FP-6).
- **CONTRACTS §14 serializer** — the canonical byte-exact root/observation
  serializer; the Rust port lives in a shared `oci/index/wire.rs` (ADR D5, ADR-6
  FP-4).
- **CI units (D1)** — GitHub reusable workflow (in `setup-ocx`) + GitLab CI/CD
  Component wrapping `ocx package announce`.
- **`OCX_ANNOUNCE_TOKEN`** — forge-neutral publisher token, ambient CI env only,
  never in `auth/store.rs` (ADR D3).

---

## Verdict table

| # | Problem (from artifact 1) | BCR mechanism | OCX equivalent | Verdict | One-line reason |
|---|---|---|---|---|---|
| 1 | Trigger + version derivation | release event → `tag_prefix`-strip → VERSION | `ocx package push --announce-file` accumulates pushed+cascade tags; announce unions with committed root | **ADAPT** | OCX derives tags from the push it just did, not a tag-name convention — no prefix-stripping guesswork |
| 2 | Publisher contract surface (`.bcr/` templates) | 4 hand-edited template files per repo | committed package root `owners[]`/`repository`; the index bot re-derives everything else from registry truth | **ALREADY-STRONGER** | Registry is the source of truth; almost nothing stays publisher-authored (see deep-dive) |
| 3 | Entry generation + integrity | download archive, re-hash SRI into `source.json` | observation objects built from `oci::Client`/`Publisher` (`list_tags`/`fetch_manifest`); CAS content-addressed | **ADAPT** | Same re-derive-and-hash discipline, but content-addressed CAS (`o/sha256/<hex>`) instead of a single SRI field |
| 4 | Metadata merge + yanked preservation | `clearVersions()` keeps `yanked_versions`; BCR wins on collision | announce reads committed root @ main via forge contents API, merges curated tags; yank is a distinct G-05 human-lane key, survives regeneration (FP-2) | **ADAPT** | Same "registry-history-wins, never un-yank" invariant; OCX makes yank≠delete a first-class curation split |
| 5 | Fork + branch + force-push idempotency | deterministic `<module>-<tag>` + `git push --force` (workflow) / random branch (App) | stable branch `ocx-announce-<ns>-<pkg>`; commit via forge REST contents/commits API, no local git (ADR D2) | **ADAPT** | Stable-branch idempotency, but REST-API commit transport — no `git2`/`gitoxide` dep, ports to GitLab later |
| 6 | PR creation/update + dedupe | create-only, no dedupe (App) / 422-as-success (workflow) | `open_or_update_pull_request`: dedupe via hidden HTML marker + stable branch (ADR NFR, mirrors Homebrew) | **ADOPT (stronger)** | We copy Homebrew's `check_for_duplicate_pull_requests`, not winget's/BCR's missing-dedupe gap |
| 7 | Token model + fine-grained-PAT gap | classic PAT `workflow`+`repo`; FG-PAT can't PR public repos | `OCX_ANNOUNCE_TOKEN`, ambient CI env, forge-inferred; document `open_pull_request:false` fallback; never in `auth/store.rs` | **ADOPT** | Same platform ceiling (roadmap#600); inherit the PAT + machine-account guidance verbatim |
| 8 | Self-approval draft trick | draft PR "Ready for review" = author sign-off | not needed — G-19 machine lane auto-merges on `owners[]` membership, no author self-approval (see deep-dive) | **AVOID** | Our authorization is committed data checked by a privileged job, not a human approval GitHub forbids |
| 9 | Registry-side presubmit validation | `bcr_presubmit.py` dynamic Buildkite; rc 42 = block | `validate.yml` `verify-claims` re-derives every claimed tag from registry, byte-compares (FP-1) | **ADAPT** | Same "re-derive, never trust the claim" gate; ours is GitHub Actions + canonical-byte-compare, not Buildkite matrix |
| 10 | Maintainer approval w/o write access + bazel-io merge | `metadata.json` maintainers approve; bazel-io merges | owners auto-merge (G-19); non-owners → human lane, reviewers from `maintainers.yml` (G-20) | **ADOPT** | Trust-cached-as-in-repo-data is exactly ADR-6's `owners[]`/`maintainers.yml` model |
| 11 | Add-only immutability + escape hatch | immutable versions; `<v>.bcr.<N>`; yank metadata-only | verify-only reconcile flags drift, never rewrites (FP-3); yank = grace marker (FP-2) | **ADAPT** | Immutability via content-addressed CAS + verify-only reconcile, no `.bcr.N` republish concept needed |
| 12 | Attestation chain + hardcoded builder-id | throwaway-entry two-pass; 2-entry builder-id allowlist | none in v1 — future work (see deep-dive) | **AVOID (defer)** | Attestation is a later track; record the builder-id-allowlist lesson before designing it |
| 13 | Multi-module repos | `moduleRoots`, one PR per release | announce is per-package; G-19 evaluated per-root so a multi-root PR needs owner on every root | **ADAPT** | Reference tool emits single-package PRs; per-root G-19 already handles arbitrary multi-root fork PRs |
| 14 | Failure observability | one PR-URL step summary; rest is raw log | structured announce report (`DataInterface`) + `UserInterface` diagnostics from day one (ADR NFR) | **ADOPT (stronger)** | The explicit BCR lesson — build structured surfacing first, not issue-by-issue |
| 15 | Tool versioning/distribution | committed 11 MB JS bundles, drift-checked, `uses:@sha` | `ocx` is a single Rust binary; CI unit installs a pinned `ocx` via `setup-ocx`, SHA-pinned | **ALREADY-STRONGER** | No committed-bundle problem — the logic ships as a versioned binary, the CI unit stays ~15 lines |
| 16 | E2E testing strategy | faked App path; current transport untested | live sandbox: `michael-herwig/ocx-index-e2e` + fork, real `pull_request`/`pull_request_target`, real GHCR (ADR-6 E2E) | **ADOPT (stronger)** | We test the *real* transport end-to-end, the exact gap BCR left open |

---

## Deep-dives (only where the transfer is non-obvious)

### The day-1 / day-N cost we must beat

BCR's genius is that the recurring cost is "push a tag." Its whole friction is
front-loaded into an irreducible day-1 setup. We must beat day-1, because our
day-N is already at parity (a push).

**BCR day-1 (per repo, human, one-time):**

1. Make the ruleset bzlmod-compatible (prerequisite, not the tool's job).
2. Copy `.bcr/` and hand-edit `metadata.template.json` + `source.template.json`.
3. Point `presubmit.yml` at a real bzlmod test workspace + matrix.
4. Author the publish workflow YAML (trigger, `permissions`, `registry_fork`).
5. Fork `bazelbuild/bazel-central-registry`.
6. Mint a classic PAT (optionally a machine account); store as `BCR_PUBLISH_TOKEN`.
7. Decide `draft` true/false based on human-vs-bot PAT.
8. If attesting: wire `release_ruleset.yaml`, `draft: true`, add a
   `gh release edit --draft=false` finalize job.
9. Multi-module: author `config.yml` `moduleRoots`, replicate templates per root.
10. Optional `.bcr/patches/*.patch`.

**OCX day-1 (target):**

1. Ensure the package root exists on the index with `owners[].github_id` (already
   required by ND-8; the human-lane first-claim, not this tool's job).
2. Fork `ocx-sh/index` — or let announce **auto-create** it (ADR open item,
   proposed yes; wingetcreate pattern).
3. Mint a forge token as `OCX_ANNOUNCE_TOKEN` (same PAT ceiling as BCR; a machine
   account is the same recommendation).
4. Drop the CI unit: GitHub reusable workflow (`setup-ocx`) or GitLab CI/CD
   Component — ~15 lines: install pinned `ocx`, run `ocx package announce`.

Steps 2/3/4 in BCR (`.bcr/` triad, presubmit workspace, workflow authoring) and
step 8/9/10 (attestation wiring, multi-module template replication) **collapse to
nothing** for OCX because the index bot re-derives entry content from registry
truth — there is no per-package entry file for the publisher to author. What
remains publisher-authored is exactly: `owners[]`/`repository` (committed once at
first-claim), a fork (or auto-create), one token, one CI unit. That is the bar to
hold in the plan: **do not reintroduce a per-package template file.**

### What replaces `.bcr/` templates

BCR's four templates exist because the tool cannot see the registry — it must be
told the homepage, the maintainers, the archive URL shape, the test targets. OCX's
index bot **can** see the registry (it re-derives observations from OCI manifests),
so the equivalent surface shrinks to what genuinely cannot be inferred:

| BCR template field | OCX equivalent | Who authors it |
|---|---|---|
| `metadata.json` homepage/description | desc blobs `o/sha256/<hex>.{md,svg,png}` in the committed root | owner, once (optional) |
| `metadata.json` maintainers + `github_user_id` | `owners[].github_id` in the committed root (ND-8) | owner, at first-claim (human lane) |
| `metadata.json` `repository` | root `repository` field (G-05 key) | owner, at first-claim |
| `metadata.json` `versions[]`/`yanked_versions` | `tags` map, owner-curated via announce (FP-2) | tool-managed via curated announce |
| `source.template.json` url/strip_prefix/integrity | derived from the OCI manifest — CAS observation objects | **nobody** — bot re-derives |
| `presubmit.yml` test matrix | no analogue — OCX does not run publisher build/test in the index | **nobody** |
| `config.yml` moduleRoots | no analogue — announce is per-package | **nobody** |

Net: the publisher-authored surface is `owners[]` + `repository` + optional desc
blobs, all committed **once** at first-claim through the human lane, plus the
per-release curated `--tags`/`--tags-file`. Nothing per-release except the tag set.
This is the ALREADY-STRONGER verdict for row 2 made concrete.

### Draft-PR self-approval trick vs the G-19 machine lane

BCR needs the draft trick because its migration made the *human publisher* the PR
author, and GitHub forbids a PR author approving their own PR (issue #261). The
draft "Ready for review" click is a synthetic sign-off standing in for an approval
the platform disallows. It cost a coordinated two-repo rollout and remains
workaround debt.

OCX does not need it, for a precise reason: **authorization is not a human
approval at all.** Under ADR-6 FP-5/G-19 the privileged `governance-gate` job reads
the PR author's `github_id` from PR metadata and checks membership in the
**base-branch** committed `owners[]` — never PR-head content, never a GitHub review
event. If the author is an owner and no G-05 key is touched, the machine lane arms
`gh pr merge --auto`. There is no author-approving-own-PR step to be forbidden,
because the "approval" is a data lookup a privileged job performs, not a review a
human clicks. The draft state carries no meaning in our lane.

Consequence: OCX's lane is strictly simpler than BCR's *because* it kept
author≠approver-as-identity (identity = GitHub account) but moved the approval
decision to committed data. The only place a human approval re-enters is the
**human lane** (non-owner, new package, or G-05 key), where G-20 assigns a real
reviewer from `maintainers.yml` — which is BCR's maintainer-approval model, minus
the self-review contortion.

### Attestation track (future work) — carry the builder-id lesson

Attestation is explicitly **not** in the v1 announce lane (ADR D-scope; ADR-6 lists
signing as a deferred ADR). Recorded here so the future track starts from BCR's
scars, not a blank page:

- BCR's chicken-and-egg (attestation refs need the entry, the entry needs the
  attestations) forced a **two-pass throwaway-entry dance** and hardcoded action
  SHAs. OCX's content-addressed CAS already means every observation/desc blob is
  self-certifying by path digest (`o/sha256/<hex>` hashes to `<hex>`, FP-4) — so an
  OCX attestation would attest the *root* and *CAS objects*, and there is no
  single mutable `source.json` integrity field to create a cycle. The two-pass
  dance likely does not transfer.
- The **hardcoded 2-entry builder-id allowlist** (`slsa.py`, issue #262) is the
  single most-cited BCR adoption blocker: any publisher not using the one blessed
  release workflow is rejected. The lesson for an OCX attestation ADR is explicit:
  **decide up front whether builder provenance is an allowlist or a single
  mandatory workflow, make it config-driven from day one, and say so loudly.** A
  fixed basename lookup with two `TODO`s is where BCR is stuck.
- BCR's attestation-history ratchet (a version regressing from attested→unattested
  fails, minus a temporary opt-out set) shows how to phase mandatory provenance in
  without a flag day — but only once the allowlist question is answered.

### Observability requirements for v1 — concrete list

BCR's observability was underbuilt from day one and bolted on issue-by-issue
(#176/#174/#94/#38); the one structured signal in the whole pipeline is the success
PR-URL step summary. The ADR NFR already says "structured failure surfacing from
day one." Concretely, the OCX CI unit and `ocx package announce` report
(`DataInterface`) must print, in a machine-readable step summary:

1. **Resolved target** — index source-repo, fork owner/repo, branch name.
2. **Curated tag set** — the tags announced (from `--tags`/`--tags-file`/root
   union), and which were added/removed/unchanged vs the committed root.
3. **Diff outcome** — empty-diff (idempotent re-announce, no-op) vs changed, with
   the changed tag list. Unchanged tags keep their `observed` timestamp (ocx#216
   item 3) — the report states "no change" explicitly, not silence.
4. **PR result** — created URL, or updated-in-place URL, or the manual-PR-URL
   fallback when `open_pull_request:false`, or "duplicate PR skipped" — never a
   bare exit code.
5. **Failure reason** — the specific failed precondition (token lacks fork
   capability; ambient `GITHUB_TOKEN` rejected; forge contents read failed;
   serializer conformance mismatch) as a typed diagnostic, **not** "unknown error."
6. **Byte-conformance note** — on a serializer mismatch, which file diverged, so a
   publisher knows it is a client-version problem, not their data.

The test for "did we beat BCR here" is: a publisher reading only the step summary
(never the raw log) can tell what happened and what to do next.

### E2E testing strategy transfer

BCR has two harnesses and the wrong one is complete: the fully-faked sandbox
(`mockttp` GitHub + local git + local SMTP + real process boundary) exercises the
**deprecated** App path, while the **current** transport's `Push to fork` and
`Open pull request` steps have zero e2e coverage. The lesson: fake-everything is a
fine pattern, but if it tests the retired transport it protects nothing.

OCX's ADR-6 E2E strategy already avoids this trap by going the other way — a
**live** sandbox that exercises the real transport, because fork PR →
`pull_request`/`pull_request_target` → auto-merge only exists across real repos:

- Topology: `michael-herwig/ocx-index-e2e` plays `ocx-sh/index` (content copy, not
  a GitHub fork); `ocx-contrib/ocx-index-e2e` is its fork playing the publisher
  (GitHub forbids same-account self-forks); `michael-herwig/ocx-e2e-publisher`
  ORAS-pushes the pseudo package to `ghcr.io/michael-herwig/ocx-e2e-dummy` using an
  Actions `GITHUB_TOKEN` with `packages: write` — **no PAT**, proving the
  credential-free-publish claim concretely.
- Positive path: push a tag → `announce --fork ocx-contrib/ocx-index-e2e` → poll PR
  checks → assert auto-merge → assert merged root byte-matches registry truth.
- Negative cases (each must fail closed): tampered digest → verify-claims red;
  non-owner author → human lane + reviewer assigned, no auto-merge (G-19);
  G-05 key change → human lane; non-canonical root bytes → verify-claims red (FP-4);
  pinned-tag digest mutation → reconcile anomaly (FP-3).

What we **take** from BCR's harness despite it testing the wrong path: the
faked-boundary technique is worth keeping for **fast unit-level** coverage of the
Rust forge REST client (a stubbed `IndexTransport`/forge seam, per ADR D2) — the
`FakeGitHub`/`mockttp` idea, scoped to the client's HTTP calls, complements the
slow live sandbox. Fast fakes for the client, live sandbox for the transport; do
not let the fast fakes become the *only* coverage the way BCR's did.

---

## Open questions this raises for the ADR

Only questions **not** already in the ADR's five open items (additive `--tags-file`
semantics, `feat/index-indirection` sequencing, CI-unit repo placement, fork
auto-create, GitLab-hosted index):

1. **Empty-diff exit contract.** BCR's 422-"already exists" is treated as soft
   success. What is `ocx package announce`'s exit code and step-summary line when a
   re-announce produces an empty diff (idempotent no-op) vs when it updates an
   existing PR in place? The report list above assumes both are success-with-a-note;
   the ADR does not fix the exit code, and CI units branch on it.
2. **`--announce-file` accumulation lifecycle.** BCR derives the version from the
   release tag each run. OCX's `--announce-file` *accumulates* pushed+cascade tags
   across pushes — when is it reset/truncated? If it is never cleared, a stale entry
   re-announces a tag the owner later curated out via `--tags` replace. The union
   semantics (D5) and replace semantics (`--tags`) interact here; needs a defined
   reset point.
3. **desc-blob authoring surface.** Row 2 keeps homepage/logo as optional committed
   desc blobs (`o/sha256/<hex>.{md,svg,png}`). Does `ocx package announce` write
   these, or are they a separate human-lane commit? BCR folds homepage into the same
   template the tool substitutes; OCX has no template, so the blob-authoring path is
   unspecified — and desc blobs are a G-05/human-lane concern (FP-4 hash-checks them
   but FP-5 does not obviously machine-lane them).
4. **Multi-root fork PR from the reference vs arbitrary tools.** G-19 is per-root,
   but `ocx package announce` emits single-package PRs by construction. Should the
   CI unit / client *refuse* to build a multi-root PR (keeping the reference tool
   single-root, as ADR-6 FP-9 notes), or is multi-root announce a supported client
   mode? BCR supports multi-module explicitly; OCX's stance is implied single-root
   but not stated for the client.
