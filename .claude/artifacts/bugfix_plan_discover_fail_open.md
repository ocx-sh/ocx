# Bug Fix Plan: discover fail-open re-flags published versions as New (#157)

## Status

- **Plan:** bugfix_discover_fail_open
- **Active phase:** 7 — Commit & Document
- **Step:** awaiting /finalize
- **Last update:** 2026-06-11 (implemented + verified, review loop clean)

---

## Overview

**Status:** In Progress
**Author:** Claude (issue analysis by maintainer in #157)
**Date:** 2026-06-11
**GitHub Issue:** #157
**Severity:** High (digest-pin breakage / GC hazard; destructive republish)

## Bug Report

### Observed Behavior

`ocx-mirror pipeline plan` spuriously re-flags already-published versions as `New` when a transient
target-registry read fails during platform discovery. Versions are fully rebuilt and re-pushed; with
build-content drift across a toolchain bump, the re-push re-points the version tag to a new digest and
orphans the previous digest (GC can reap → `@sha256:` pins 404).

### Expected Behavior

Transient target-registry failures must never be treated as "version absent". The plan either aborts
(exit 69 Unavailable, like upstream-source failures already do) or excludes affected versions. Only an
authoritative registry "not found" response may classify a version/repo as absent.

### Reproduction Steps

1. Mirror repo fully published (e.g. cmake 4.3.3 on `ocx-contrib/mirror-cmake`).
2. `ocx-mirror pipeline plan` while target registry read fails transiently (5xx / rate-limit / auth blip)
   for `list_tags` or per-tag `fetch_manifest`.
3. Plan output: published version(s) re-flagged `kind: new` with full platform set.

### Environment

| Factor | Value |
|--------|-------|
| Platform | any |
| OCX version | current main (c3650db) |
| Registry | any OCI registry (observed: ocx.sh/cmake) |
| Configuration | `build_timestamp: none` amplifies impact (tag re-point on rebuild) |

### Frequency

Intermittent — requires transient registry read failure during discover. Observed live 2026-06-09
(run 27182941667, 5 versions re-flagged = `new_per_run` cap), self-corrected next day.

## Root Cause Analysis

### Investigation Log

1. **Symptom**: published versions re-flagged `New` → rebuilt → (would be) re-pushed.
2. **Proximate cause**: `build_version_entries` classifies `New` when `declared_platform_count == missing_platforms.len()` — true whenever `platform_info` is empty for a published version.
3. **Root cause**: fail-open handling of target-registry reads. Two sites, one pattern:
   - `plan.rs:133` / `sync.rs:56` — `publisher.list_tags(..).unwrap_or_default()`: ANY error → empty tag list → every version looks absent. (Live incident: exactly 5 = `new_per_run` cap — consistent with this path or with cascaded per-tag failures.)
   - `plan.rs:156` — `if let Ok((_, manifest)) = ..fetch_manifest(..)`: error silently swallowed; `sync.rs:150` — warn + continue. Either way `platform_info` stays empty → `filter_versions` trims nothing → `New` with full platform set.
4. **Introduced by**: original implementation; `plan.rs` copied `sync.rs`'s fail-open pattern. Comment at plan.rs:131 mis-attributes safety to a later `SourceError` that only covers the *upstream* source, not the target registry.

### Root Cause Statement

> Published versions are re-flagged `New` because discover/sync treat transient target-registry read
> failures (`list_tags`, per-tag `fetch_manifest`) identically to authoritative "nothing published",
> introduced when the registry-state loading was written fail-open (`unwrap_or_default` / `if let Ok`
> swallow) with no typed not-found distinction available from `ocx_lib`'s `list_tags` path.

### Related Code

| File | Lines | Role |
|------|-------|------|
| `crates/ocx_mirror/src/command/pipeline/plan.rs` | 133, 154-164 | Root cause (discover path) |
| `crates/ocx_mirror/src/command/sync.rs` | 56, 146-164 | Same pattern (direct push path — also hazardous) |
| `crates/ocx_mirror/src/command/pipeline/plan.rs` | 259-286 | Where symptom manifests (`build_version_entries` → `New`) |
| `crates/ocx_lib/src/oci/client/native_transport.rs` | 111-124 | `list_tags` maps ALL errors → opaque `Registry` (no not-found distinction) |
| `crates/ocx_lib/src/oci/client/error.rs` | 13-58 | `ManifestNotFound` exists; no `RepositoryNotFound` |

### Pattern Check

- [x] Searched similar code with same defect — `sync.rs` carries both sites; no other `unwrap_or_default()` on registry reads in `ocx_mirror`
- [x] Regression from recent change? No — original implementation
- [x] Other callers affected? `RemoteIndex::list_tags` propagates errors (no swallow) — unaffected. Exit-code classification for missing-repo `list_tags` changes 69→79 (more correct per taxonomy)

## Regression Test Specification

> Tests written BEFORE fix. Fail on current code (helpers stubbed contract-first).

### Unit Tests

| Test | File | Asserts |
|------|------|---------|
| `transient_list_tags_error_aborts` | `crates/ocx_mirror/src/command/target_registry.rs` | `Registry` error from `list_tags` → `Err(TargetError)`, NOT empty tag list |
| `repository_not_found_means_no_tags` | same | `RepositoryNotFound` → `Ok(vec![])` (first-publish bootstrap keeps working) |
| `transient_manifest_error_aborts` | same | `Registry` error from `fetch_manifest` → `Err(TargetError)`, NOT skipped tag |
| `manifest_not_found_skips_tag` | same | `ManifestNotFound` (authoritative absence) → tag skipped, no abort |
| `target_error_maps_to_unavailable` | `crates/ocx_mirror/src/error.rs` | `TargetError` → exit 69 |
| `list_tags_not_found_maps_to_repository_not_found` | `crates/ocx_lib/src/oci/client/native_transport.rs` | NAME_UNKNOWN/404 envelope → `ClientError::RepositoryNotFound`; other → `Registry` |

## Fix Approach

### Proposed Change

Fail-safe, not fail-open (issue #157 proposed fix 1):

1. **`ocx_lib`**: add `ClientError::RepositoryNotFound` (classify → `NotFound` 79); map `list_tags`
   404/NAME_UNKNOWN/NOT_FOUND → it in `native_transport` (mirrors existing
   `manifest_not_found_or_registry_error`).
2. **`ocx_mirror`**: new `MirrorError::TargetError` (exit 69 Unavailable). New shared module
   `command/target_registry.rs`: `list_target_tags` (not-found → empty; other error → `TargetError`)
   and `fetch_published_platforms` (not-found → skip tag; other error → `TargetError`). Wire into
   `plan.rs` + `sync.rs`, deleting both fail-open sites.

### Files to Modify

| File | Change |
|------|--------|
| `crates/ocx_lib/src/oci/client/error.rs` | `RepositoryNotFound` variant + classify arm |
| `crates/ocx_lib/src/oci/client/native_transport.rs` | not-found-aware error mapper for `list_tags` + tests |
| `crates/ocx_mirror/src/error.rs` | `TargetError` variant (Display, exit code, test) |
| `crates/ocx_mirror/src/command/target_registry.rs` | NEW — fail-safe registry-state helpers + regression tests |
| `crates/ocx_mirror/src/command.rs` | register module |
| `crates/ocx_mirror/src/command/pipeline/plan.rs` | replace fail-open sites with helpers |
| `crates/ocx_mirror/src/command/sync.rs` | replace fail-open sites with helpers |
| `.claude/rules/subsystem-mirror.md` | error table + module map rows |

### Alternatives Considered

| Approach | Rejected Because |
|----------|-----------------|
| Mark version "present, platforms unknown" (skip instead of abort) | Needs third state in `VersionPlatformMap` + filter changes; discover is a scheduled job — abort + retry next run is simpler and equally safe (issue lists abort as first option) |
| Map `list_tags` 404 → `Ok(vec![])` in transport | Silently changes `ocx index update` UX for typo'd repos (error → empty success); typed variant lets callers choose |
| End-to-end regression test via injected transport | `Client::with_transport` is `#[cfg(test)] pub(crate)`, `OciTransport` module private — exposing a cross-crate test seam is scope creep; unit tests on extracted classification helpers cover the decision logic |

### Risk Assessment

| Risk | Mitigation |
|------|------------|
| Discover/sync now red on transient registry blips | Correct per issue (fail-safe); scheduled runs self-heal; exit 69 distinguishes from real failures |
| `list_tags` missing-repo exit code changes 69 → 79 for `ocx` CLI surfaces | More correct per exit-code taxonomy (79 = resource not found); note in commit body |
| First publish of new mirror repo must still work | `RepositoryNotFound` → empty tag list preserved; covered by regression test |

## Verification Checklist

- [x] Regression tests specified before fix (contract-first; `-D warnings` forbids compiling `todo!()` stubs, so red phase = contract spec, not a panicking run)
- [x] Fix applied — regression tests pass (7 in `target_registry.rs`, 5 in `native_transport.rs`, 1 in `error.rs`, 1 in `resolve.rs`)
- [x] All existing tests still pass — full `task verify` green (2477 Rust unit + 1236 acceptance)
- [x] Review-fix loop: worker-reviewer pass — correctness/regression/minimality clean; auth-error coverage added on its suggestion
- [x] No scope creep — issue's proposed fixes 2 (guarded push) + 3 (retention) deferred to follow-ups

## Notes

Issue #157 proposed fixes 2 (idempotent/guarded `pipeline push`, `--republish`) and 3 (GC retention /
build-stamped tags) are defense-in-depth features, not part of this root-cause fix — follow-up issues
to be proposed after landing.
