# Bug Fix Plan: prepare matrix legs re-crawl the source (N+1 crawls, issue #160)

## Status

- **Plan:** bugfix_prepare_recrawl
- **Active phase:** 7 — Commit & Document
- **Step:** awaiting /finalize
- **Last update:** 2026-06-11 (after 1146678: fix(mirror): stop prepare legs re-crawling the source)

---

## Overview

**Status:** Complete
**Author:** Claude
**Date:** 2026-06-11
**GitHub Issue:** #160
**Severity:** High

## Bug Report

### Observed Behavior

A mirror run discovering N new versions performs N+1 concurrent full upstream
crawls (1 × `discover` + N × `prepare` matrix legs). For GraphQL-backed,
asset-heavy sources (`mirror-cpython`), concurrent crawls exhaust the GitHub
GraphQL points budget (5000 pts/hr shared per installation token):
`"API rate limit already exceeded for site ID installation."` → hard red.

### Expected Behavior

One crawl per run. `prepare` consumes `discover`'s already-resolved output.

### Reproduction Steps

1. Mirror spec with `url_index` generator source over asset-heavy upstream
2. Upstream publishes ≥ a few new versions
3. Scheduled run: `discover` crawls, emits N versions; N `prepare` legs each
   re-run the source generator → rate-limit red

### Frequency

Always when N ≥ ~3 new versions on GraphQL-backed asset-heavy source.

## Root Cause Analysis

1. **Symptom**: GraphQL `RATE_LIMIT` errors in `prepare`/`discover` legs
2. **Proximate cause**: `prepare.rs::build_tasks_for_version` calls
   `list_upstream_versions(spec, spec_dir)` — full source crawl per leg
3. **Root cause**: `plan.json` carries only `{version, platforms, kind}` —
   it discards the per-platform asset URLs `discover` already resolved, so
   `prepare` has no choice but to re-crawl
4. **Introduced by**: original pipeline design (`prepare` designed standalone)

### Root Cause Statement

> N+1 crawls happen because `plan.json` does not carry the resolved asset
> URLs, forcing each `prepare` leg to re-run the source generator —
> a gap in the original two-phase pipeline design.

### Related Code

| File | Role |
|------|------|
| `crates/ocx_mirror/src/command/pipeline/plan.rs` | builds `PlanReport`; has `ResolvedVersion.platforms` (assets) in hand, drops them |
| `crates/ocx_mirror/src/command/pipeline/prepare.rs` | `build_tasks_for_version` re-crawls |
| `crates/ocx_mirror/src/command/pipeline/generate/templates/workflow.yml` | prepare leg invokes bare `pipeline prepare --version` |

### Pattern Check

- [x] Other callers: `sync.rs` single-process — one crawl, fine. `push`/`notify` don't crawl.
- [x] Not a regression — design gap since pipeline introduction.

## Regression Test Specification

| Test | File | Asserts |
|------|------|---------|
| `plan_entry_carries_resolved_assets` | `plan.rs` | entry serializes `source_version`, `variant`, `assets[{platform,asset_name,url}]`; schema_version 2 |
| `build_tasks_from_plan_does_not_call_source` | `prepare.rs` | spec with failing generator source + plan doc → tasks built OK (would error if crawled) |
| `build_tasks_from_plan_resolves_variant_and_asset_type` | `prepare.rs` | variant lookup + per-platform asset_type from spec |
| `build_tasks_from_plan_errors_on_missing_version` | `prepare.rs` | version absent from plan → `SpecInvalid` |
| `rendered_workflow_prepare_consumes_plan_artifact` | `ci.rs` | mirror.yml uploads `plan.json` artifact; prepare leg downloads + passes `--plan` |

## Fix Approach

### Proposed Change

Issue option 1 (structural): crawl once, ship resolved index through the run.

1. **`plan.rs`** — extend `PlanVersionEntry` with `source_version: String`,
   `variant: Option<String>`, `assets: Vec<PlanAssetEntry { platform, asset_name, url }>`.
   Add `Deserialize` to plan types. Bump `schema_version` → 2.
2. **`prepare.rs`** — new `--plan <path>` flag. When set: read plan, find entry
   by tagged version, build `MirrorTask`s from entry + spec (variant lookup,
   asset_type resolve, `platform_applies` re-check) — zero source calls.
   Missing entry or empty assets → `SpecInvalid` with actionable message.
   Without flag: existing crawl path (standalone use preserved).
3. **`workflow.yml` template** — discover uploads `plan.json` artifact;
   `versions` output projected via jq to `{version, platforms, kind}` (matrix
   stays lean); prepare downloads `plan` artifact, runs
   `pipeline prepare --version V --plan plan.json`; drop now-dead
   `GITHUB_TOKEN` env + quota comment from prepare leg.

### Alternatives Considered

| Approach | Rejected Because |
|----------|-----------------|
| Run-scoped HTTP cache via `actions/cache` (option 2) | mitigation, not fix; cache races between concurrent legs; relates #42 |
| Serialize prepare legs | wall-clock regression; still N+1 crawls |

### Risk Assessment

| Risk | Mitigation |
|------|------------|
| Old binary + new plan / new binary + old plan | plan is intra-run artifact; binary + workflow co-pinned via `OCX_MIRROR_RELEASE_TAG`; prepare validates assets present with actionable error |
| Matrix JSON bloat from asset URLs | jq projection keeps `versions` output to current shape |
| Backfill-partial: plan assets = missing platforms only (crawl path built all applicable) | test job already skips out-of-set platforms; push gates per (V,P) on junit — narrowing matches published set |

## Verification Checklist

- [x] Regression tests fail on current code (flag/fields absent → compile-fail; no-crawl property structurally proven via unreachable-source spec)
- [x] Fix applied — regression tests pass (8 new tests green)
- [x] `task rust:verify` green (2486 pass); `task verify` final gate
- [x] subsystem-mirror.md plan/prepare contract rows + PlanError row updated

## Notes

Related: #42 (TTL caching) stays open — orthogonal mitigation for `discover`'s
own single crawl cost.
