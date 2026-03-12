# Plan: Cascade Hardening — Error Handling, Tests, Deduplication

**Status:** Proposed
**Date:** 2026-03-12
**Branch:** `sion`
**Depends on:** Commit `3abf96c` (cascade refactoring)

---

## Context

The cascade refactoring (commit `3abf96c`) introduced platform-aware cascade push
with a clean two-layer architecture: pure algebra (`decompose`/`cascade`) + async
orchestration (`resolve_cascade_tags`/`push_with_cascade`). The algebra layer has
thorough unit tests (17 tests). The orchestration layer has **zero unit tests** and
conflates registry errors with version-blocking. There is also code duplication
between `merge_platform_into_index` and `update_image_index`, and a pre-existing
bug in the ImageManifest→ImageIndex upgrade path.

## Objectives

1. Distinguish "blocked by version" from "blocked by registry error"
2. Add unit tests for the orchestration and merge layers using `StubTransport`
3. Deduplicate `merge_platform_into_index` / `update_image_index`
4. Fix the pre-existing `config.digest` bug in the ImageManifest upgrade path
5. Add missing algebra tests and acceptance tests for edge cases

---

## Phase 1 — Error Handling in `has_blocking_platform`

**Goal:** Stop conflating errors with version-blocking.

**Current behavior:**
```rust
Err(e) => {
    log::debug!("...");     // invisible
    return Ok(true);        // error becomes "blocked"
}
```

**Fix:** Let the `Err` propagate naturally. The caller `resolve_cascade_tags` catches
it and decides:

```rust
// has_blocking_platform — just remove the Err→Ok(true) mapping:
Err(e) => {
    return Err(e);
}

// resolve_cascade_tags — catch at the level boundary:
for level in &decomposition.levels {
    match has_blocking_platform(client, identifier, &level.blockers, platform).await {
        Ok(true) => return Ok((tags, false)),
        Ok(false) => tags.push(level.target.to_string()),
        Err(e) => {
            log::warn!(
                "Cascade stopped at {}: could not verify blocker — {e}",
                level.target
            );
            return Ok((tags, false));
        }
    }
}
// Same pattern for latest_blockers
```

No new types needed. `Result<bool>` already encodes the three states:
- `Ok(false)` — clear, no blocker has this platform
- `Ok(true)` — blocked by a concrete version
- `Err(e)` — registry error, cascade stops conservatively with a `warn!`

**Files:** `crates/ocx_lib/src/package/cascade.rs`

---

## Phase 2 — Unit Tests for Orchestration Layer (L2/L3)

**Goal:** Test `has_blocking_platform` and `resolve_cascade_tags` via `StubTransport`.

All tests use `Client::with_transport(Box::new(StubTransport::new(data)))` and seed
manifests into `data.write().manifests` to control what `fetch_manifest` returns.

### `has_blocking_platform` tests

| # | Test Name | Setup | Expected |
|---|-----------|-------|----------|
| 1 | `empty_blockers_returns_clear` | blockers = [] | `Ok(false)` |
| 2 | `blocker_with_matching_platform_blocks` | Blocker `3.28.1` manifest has `linux/amd64` index entry; platform = `linux/amd64` | `Ok(true)` |
| 3 | `blocker_without_matching_platform_passes` | Blocker `3.28.1` manifest has `linux/amd64` only; platform = `linux/arm64` | `Ok(false)` |
| 4 | `blocker_manifest_fetch_error_returns_err` | No manifest seeded for blocker | `Err(...)` |
| 5 | `first_blocker_lacks_platform_second_has_it` | Two blockers: first has `linux/arm64`, second has `linux/amd64`; platform = `linux/amd64` | `Ok(true)` |
| 6 | `all_blockers_lack_platform` | Two blockers, both only `linux/arm64`; platform = `linux/amd64` | `Ok(false)` |
| 7 | `blocker_is_image_manifest_not_index` | Blocker tag has plain ImageManifest (no platform info) | `Ok(false)` |

### `resolve_cascade_tags` tests

| # | Test Name | Setup | Expected |
|---|-----------|-------|----------|
| 8 | `no_blockers_full_cascade_with_latest` | version `3.28.0_b1`, others = {}, all manifests empty | tags = [`3.28.0`, `3.28`, `3`, `latest`], is_latest = true |
| 9 | `blocker_at_minor_lacks_platform_cascade_continues` | `3.28.1_b1` in others, manifest has `linux/arm64` only; pushing `linux/amd64` | tags includes `3.28` (cascade past blocker) |
| 10 | `blocker_at_minor_has_platform_cascade_stops` | `3.28.1_b1` in others, manifest has `linux/amd64`; pushing `linux/amd64` | tags = [`3.28.0`] only |
| 11 | `latest_blocked_by_higher_version_with_platform` | `4.0.0_b1` in others with `linux/amd64`; pushing `3.28.0_b1` for `linux/amd64` | All level tags but NOT `latest` |
| 12 | `error_on_blocker_stops_cascade_with_warning` | Blocker `3.28.1_b1` has no manifest in stub | tags stop at `3.28.0`, `Err` triggers `warn!` in resolve |
| 13 | `error_on_latest_blocker_skips_latest` | All levels clear, but latest blocker `4.0.0_b1` has no manifest | tags = [`3.28.0`, `3.28`, `3`] but NOT `latest` |

**Files:** `crates/ocx_lib/src/package/cascade.rs` (add `#[cfg(test)]` async tests)

---

## Phase 3 — Unit Tests for `merge_platform_into_index` (L5)

**Goal:** Test the fetch-modify-push cycle via `StubTransport`.

The stub needs one enhancement: `push_manifest_raw` should store the pushed data
back into `manifests` so subsequent reads see the updated index. This can be done
with a `capture_pushes: bool` flag on `StubTransportInner`.

| # | Test Name | Setup | Expected |
|---|-----------|-------|----------|
| 14 | `fresh_tag_creates_new_index` | No manifest for tag | Index with 1 entry for pushed platform |
| 15 | `existing_index_adds_platform` | Index with `linux/arm64` entry | Index with 2 entries: `arm64` + `amd64` |
| 16 | `existing_index_replaces_same_platform` | Index with `linux/amd64` entry (old digest) | Index with 1 entry: `amd64` (new digest) |
| 17 | `existing_image_manifest_upgrades_to_index` | Plain ImageManifest at tag | Index with 2 entries: old (no platform) + new (amd64) |

**Files:** `crates/ocx_lib/src/oci/client.rs` (add tests in existing `#[cfg(test)]` block)

---

## Phase 4 — Deduplicate + Fix ImageManifest Upgrade Bug

### 4a. Deduplicate `update_image_index` → delegates to `merge_platform_into_index`

`update_image_index` and `merge_platform_into_index` are near-identical. Differences:
- `update_image_index` takes `manifest_data: &[u8]` and computes `size` from it
- `merge_platform_into_index` takes `manifest_sha256: &str` and `manifest_size: i64`
- `update_image_index` returns `(Digest, ImageIndex)`

Refactor: `update_image_index` calls `merge_platform_into_index` internally, then
re-fetches the pushed index to get the digest + data (or compute digest from the
serialized index before pushing). Alternatively, change `merge_platform_into_index`
to return `(Digest, ImageIndex)` and have `update_image_index` be a thin wrapper.

### 4b. Fix `config.digest` bug in ImageManifest → ImageIndex upgrade

Both methods have:
```rust
oci::Manifest::Image(m) => {
    let entry = oci::ImageIndexEntry {
        digest: m.config.digest.clone(),  // BUG: should be manifest digest
        size: m.config.size,              // BUG: should be manifest size
        ...
    };
```

Fix: capture the digest from `pull_manifest_raw` (currently discarded as `_`):
```rust
Ok((blob, digest_str)) => {
    // ...
    oci::Manifest::Image(m) => {
        let entry = oci::ImageIndexEntry {
            digest: digest_str,
            size: blob.len() as i64,
            ...
        };
```

**Files:** `crates/ocx_lib/src/oci/client.rs`

---

## Phase 5 — Additional Algebra + Acceptance Tests

### L1 algebra tests (cascade.rs)

| # | Test Name | Scenario |
|---|-----------|----------|
| 18 | `rolling_tags_in_others_dont_self_block` | version `3.28.0_b1`, others = {`3.28.0`, `3.28`, `3`} → no blockers |
| 19 | `old_version_blocked_by_newer_at_minor` | version `3.27.0_b1`, others = {`3.28.0_b1`} → cascade stops at `3` |
| 20 | `self_in_others_doesnt_self_block` | version `3.28.0_b1`, others = {`3.28.0_b1`} → Excluded bound prevents self-block |
| 21 | `patch_without_build_cascades` | version `3.28.1` (patch) → cascades to `3.28`, `3`, `latest` |
| 22 | `minor_version_cascades` | version `3.28` (minor) → cascades to `3`, `latest` |

### Acceptance tests (test/tests/test_cascade.py)

| # | Test Name | Scenario |
|---|-----------|----------|
| 23 | `test_cascade_old_version_different_platform` | Push `3.28.0` amd64, then `3.27.0` arm64. Verify `3` has arm64 (cascade continued past `3.28` because arm64 not present). |
| 24 | `test_cascade_old_version_same_platform` | Push `3.28.0` amd64, then `3.27.0` amd64. Verify `3` still has amd64→`3.28.0` digest (cascade stopped). |
| 25 | `test_cascade_same_version_different_platform` | Push `3.28.0` amd64, then `3.28.0` arm64. Verify `3.28`, `3`, `latest` all have both platforms. |
| 26 | `test_cascade_patch_one_platform_preserves_other` | Push `3.28.0` for both platforms, then `3.28.1` for amd64 only. Verify `3.28` has amd64 (new) AND arm64 (old). |

---

## Phase 6 — Dead Code Audit

Check if `copy_manifest_data` is called anywhere. If not, remove it. It was the
old cascade mechanism (overwrite-based) replaced by `merge_platform_into_index`.

**Files:** `crates/ocx_lib/src/oci/client.rs`

---

## Execution Order

Phases 1–3 are independent and can run in parallel. Phase 4 depends on Phase 3
tests being in place. Phase 5 is independent. Phase 6 is a quick audit.

Recommended sequence: **1 → 2 → 3 → 4 → 5 → 6**

All work stays on branch `sion`. Run `task verify` after each phase.
