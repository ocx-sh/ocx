# Plan: Transparent Tag Fallback

## Overview

**Status:** Draft
**Date:** 2026-04-12
**GitHub Issue:** [ocx-sh/ocx#41](https://github.com/ocx-sh/ocx/issues/41)
**Research:** [research_tag_fallback.md](./research_tag_fallback.md)

## Objective

When online (not `--offline`), resolve against the remote registry on local miss. `ocx install cmake:3.28` should just work on a fresh machine without prior `ocx index update`.

## Scope

### In Scope

- `ChainedIndex` implementing `IndexImpl` — `LocalIndex` cache + ordered `Vec<Index>` source chain
- Refactoring `LocalIndex::update_tag` from `&mut self` to `&self` (prerequisite)
- `Index::from_chained(cache, sources)` (general) and `Index::from_cached_remote(cache, remote)` (convenience for #41)
- `Context::try_init` wiring: online + not `--remote` → `ChainedIndex` with single remote source
- Acceptance tests for the three friction scenarios (fresh, new tag, different repo)
- Unit tests for ChainedIndex behavior, including multi-source chain cases (proving the Vec shape)

### Out of Scope

- Negative caching (tracking tags confirmed absent) — future optimization
- Fixing `RemoteIndex::fetch_manifest_digest` to return `Ok(None)` on 404 — separate issue
- Lock file integration (#33) — complementary, not dependent
- `--remote` flag behavior changes
- Stale tag auto-refresh — explicit `ocx index update` remains the tool for this

## Technical Approach

### Architecture Changes

```
BEFORE:
  Context::try_init → Index::from_local(local) OR Index::from_remote(remote)
  → PackageManager receives single Index
  → resolve() → index.select() → NotFound = error

AFTER:
  Context::try_init → Index::from_cached_remote(local, remote)  [online, default]
                     | Index::from_local(local)                    [--offline]
                     | Index::from_remote(remote)                  [--remote]
  → PackageManager receives single Index (unchanged)
  → resolve() → index.select() → ChainedIndex handles miss transparently
```

```
ChainedIndex { cache: LocalIndex, sources: Vec<Index> }::fetch_manifest(id):
  1. cache.fetch_manifest(id) → Some(data) → return (fast path, no source walked)
  2. cache.fetch_manifest(id) → None → walk sources in order:
     for source in &sources:
       a. cache.update_tag(source, id) succeeds → retry cache → return result
       b. cache.update_tag(source, id) errors → log warn, try next source
  3. all sources exhausted → Ok(None) → NotFound (warn logs provide diagnostics)
```

### Key Decisions

| Decision | Rationale |
|----------|-----------|
| Composite `ChainedIndex` (not retry-at-caller) | Zero changes to PackageManager, resolve(), or any CLI command. Mirrors Cargo sparse / Go modules pattern. |
| Refactor `update_tag` to `&self` (not `Mutex<LocalIndex>`) | All internal mutations already use `Arc<RwLock>` or filesystem writes. Simpler than adding locking. |
| Log source errors at `warn` and propagate when the chain is exhausted | Revised after Codex adversarial review. Source errors are logged at `warn` for diagnostics AND the last error is propagated as `Err` when every source failed. Only an explicit "not found" response from a source (which is `update_tag → Ok(())` followed by an empty cache retry) yields `Ok(None)`. Collapsing source errors into `NotFound` would break the trust boundary between "package doesn't exist" and "registry outage / auth failure," and silently defeat retry logic in automation. |
| `ChainedIndex` only on `fetch_manifest` path (not `list_tags`) | `select()` flows through `fetch_candidates()` → `fetch_manifest()`. Tag listing is only used by `ocx index catalog` which has its own semantics. |
| `list_tags` delegates to local only (no remote fallback) | Tag listing is for discovery/browsing, not resolution. Creates catalog asymmetry (install succeeds but catalog is empty) — document in user guide. |
| `list_repositories` delegates to local only | Same reasoning — repository listing is explicit index content, not resolution. |

### Component Contracts

#### `ChainedIndex` (`crates/ocx_lib/src/oci/index/chained_index.rs`)

```rust
pub(super) struct ChainedIndex {
    /// Persistent cache: read first, write to on miss.
    cache: LocalIndex,
    /// Read-only fallback sources, queried in order on cache miss.
    /// First hit wins; result is persisted into `cache`.
    /// For #41 this contains exactly one entry: the remote index.
    /// Future scenarios (CI cache, org mirror) extend the chain by appending sources.
    sources: Vec<super::Index>,
}
```

**Two-role model:**

- **`cache`** — exactly one persistent destination. Read first AND written to on any successful source fetch. This is always a `LocalIndex` (the only `IndexImpl` that persists to disk).
- **`sources`** — ordered read-only chain. Walked in order on cache miss. First source to return data wins; the result is persisted into `cache` via `update_tag`. For #41 this is `[remote_index]`. Future scenarios (CI pre-warmed cache, corporate mirror) extend the chain by appending sources.

**`IndexImpl` behavior:**

| Method | Behavior |
|--------|----------|
| `list_repositories(registry)` | Delegate to `cache` only |
| `list_tags(id)` | Delegate to `cache` only |
| `fetch_manifest(id)` | Cache first → on `None`: walk `sources` in order, persist first hit → retry cache |
| `fetch_manifest_digest(id)` | Same chain as `fetch_manifest` |
| `box_clone()` | Clone cache + clone all sources (shared caches via `Arc`) |

**Chain logic (shared by `fetch_manifest` and `fetch_manifest_digest`):**

```rust
async fn with_chain<T>(
    &self,
    identifier: &oci::Identifier,
    query: impl Fn(&LocalIndex, &oci::Identifier) -> /* future returning Result<Option<T>> */,
) -> Result<Option<T>> {
    // Fast path: check the persistent cache
    if let Some(result) = query(&self.cache, identifier).await? {
        return Ok(Some(result));
    }

    // Only fall through the chain for tagged identifiers
    // (digest-only lookups can't be discovered by tag listing)
    let Some(_tag) = identifier.tag() else {
        return Ok(None);
    };

    // Walk the source chain in order. First source that successfully
    // syncs the tag into the cache wins. Errors are logged at warn but
    // do not abort the chain — the next source gets a chance.
    let mut last_error: Option<crate::Error> = None;
    for source in &self.sources {
        match self.cache.update_tag(source, identifier).await {
            Ok(()) => {
                log::info!("Fetched tag '{}' from chained source, retrying cache lookup.", identifier);
                // Retry against the cache; if the source actually had the tag,
                // it's now persisted and the cache returns Some.
                if let Some(result) = query(&self.cache, identifier).await? {
                    return Ok(Some(result));
                }
                // Source returned without error but didn't persist a usable tag —
                // continue to the next source.
            }
            Err(e) => {
                // AC6: surface the underlying cause so users can distinguish
                // "manifest not found" vs "connection timed out" vs "401 unauthorized"
                log::warn!("Could not fetch tag '{}' from chained source: {e}", identifier);
                last_error = Some(e);
            }
        }
    }

    // All sources exhausted. If at least one ran cleanly we return Ok(None)
    // (cache retry will reveal whatever is — or isn't — there). If every
    // source errored, propagate the last error so callers can distinguish
    // a real outage from a clean not-found.
    match last_error {
        Some(e) => Err(e),
        None => Ok(None),
    }
}
```

**Error taxonomy:**

| Scenario | Behavior |
|----------|----------|
| Tag in cache | Return immediately (no source walked) |
| Tag not cached, source has it | `update_tag` persists it, retry succeeds |
| Tag not cached, source doesn't have it | `update_tag` returns `Ok(())` (clean miss) → cache retry → `Ok(None)` → `NotFound` |
| Tag not cached, network failure | `update_tag` errors → `warn` log "connection timed out" → recorded as `last_error` → propagate `Err` if no later source runs cleanly |
| Tag not cached, auth failure | `update_tag` errors → `warn` log "401 unauthorized" → recorded as `last_error` → propagate `Err` if no later source runs cleanly |
| All sources exhausted, at least one ran cleanly | `Ok(None)` → `NotFound` (the chain never errored) |
| All sources exhausted, every source errored | `Err(last_error)` propagated to caller (preserves "outage vs not-found" trust boundary) |
| `--offline` mode | `ChainedIndex` not constructed; `LocalIndex` used directly |
| `--remote` mode | `RemoteIndex` used directly (unchanged) |

#### `Index::from_chained` (`crates/ocx_lib/src/oci/index.rs`)

```rust
/// Construct an index that reads from `cache` first, falling through to
/// `sources` in order on miss. Successful source fetches are persisted
/// into `cache` via `update_tag`.
///
/// For #41 callers pass exactly one source (the remote index). The Vec
/// shape is forward-compatible with future N-source scenarios (CI cache,
/// org mirror) without an API rename.
pub fn from_chained(cache: LocalIndex, sources: Vec<Index>) -> Self {
    Self {
        inner: Box::new(chained_index::ChainedIndex::new(cache, sources)),
    }
}
```

A convenience constructor for the common single-source case keeps the call site clean:

```rust
impl Index {
    /// Convenience: cache + single remote source. Equivalent to
    /// `Index::from_chained(cache, vec![Index::from_remote(remote)])`.
    pub fn from_cached_remote(cache: LocalIndex, remote: RemoteIndex) -> Self {
        Self::from_chained(cache, vec![Index::from_remote(remote)])
    }
}
```

#### `LocalIndex` refactoring (`crates/ocx_lib/src/oci/index/local_index.rs`)

Change `&mut self` to `&self` on these methods (no other changes):
- `update()` (line 44)
- `update_tag()` (line 53)
- `update_all_tags()` (line 62)
- `sync_tag()` (line 77)
- `update_manifest()` (line 124)

All internal mutations are already via `Arc<RwLock<Cache>>` (interior mutability) or filesystem writes (no self mutation). This is a safe, mechanical refactoring.

**Breaking change for callers:** `Context::local_index_mut()` is used by `index_update.rs` to call `local_index.update()`. After refactoring, `local_index()` (shared ref) suffices. `local_index_mut()` can be deprecated or removed.

#### `Context::try_init` change (`crates/ocx_cli/src/app/context.rs`)

```rust
// BEFORE (lines 65-73):
let selected_index = if options.remote {
    index::Index::from_remote(remote_index.clone())
} else {
    index::Index::from_local(local_index.clone())
};

// AFTER:
let selected_index = if options.remote {
    if let Some(remote_index) = &remote_index {
        index::Index::from_remote(remote_index.clone())
    } else {
        return Err(anyhow::anyhow!("Remote index is not available in offline mode."));
    }
} else if let Some(remote_index) = &remote_index {
    index::Index::from_cached_remote(local_index.clone(), remote_index.clone())
} else {
    index::Index::from_local(local_index.clone())
};
```

Three branches: `--remote` → remote only, online → fallback, offline → local only.

> **Revised 2026-04-12 after Codex adversarial review**: error propagation instead of collapsing, to preserve the trust boundary between "package not found" and "registry outage / auth failure." `walk_chain` now returns `Result<()>`; sole-source or all-source errors propagate through `fetch_manifest` / `fetch_manifest_digest` instead of degrading to `Ok(None)`. Bare-identifier short-circuit kept as-is and documented inline (`ocx install <pkg>` on an empty index still requires a prior `ocx index update <pkg>`).

### User Experience Scenarios

| User Action | Expected Outcome | Error Cases |
|-------------|------------------|-------------|
| `ocx install cmake:3.28` (fresh machine, empty index) | Fetches tag from remote, persists locally, installs | Network failure → "Package not found" (same as today) |
| `ocx install cmake:3.28` (second run, tag cached) | Installs from local index (no network) | — |
| `ocx install cmake:3.29` (3.28 cached, 3.29 is new) | Fetches 3.29 from remote, persists, installs | Tag doesn't exist on registry → "Package not found" |
| `ocx install cmake:3.28 ninja:1.12` (batch, both new) | Both fetched in parallel via ChainedIndex | One fails → error for that package, other succeeds |
| `ocx install --offline cmake:3.28` (empty index) | NotFound error immediately | Expected behavior — no fallback in offline mode |
| `ocx install --remote cmake:3.28` | Resolves from remote directly (unchanged) | — |
| `ocx install cmake:3.28` (tag cached pointing to old digest) | Uses cached digest (no refresh) | Intentional — refresh is `ocx index update`'s job |

### Edge Cases

| Edge Case | Behavior |
|-----------|----------|
| Parallel fallback for same tag (batch install) | Safe — `update_tag` merges idempotently, shared cache via `Arc<RwLock>` means second call sees the first's result |
| Identifier with digest but no tag (`@sha256:...`) | No fallback — digest lookups go through local only |
| Identifier with both tag and digest | Digest takes precedence in local lookup — no fallback needed |
| `fetch_candidates` returns Some but no platform matches | `SelectResult::NotFound` at platform matching, NOT at index level — fallback doesn't trigger (correct, not an index miss) |
| Registry requires auth | `oci::Client` handles auth via `ensure_auth()` before any request — ChainedIndex inherits this |

## Implementation Steps

### Phase 1: Stubs

- [ ] **Step 1.1:** Refactor `LocalIndex` methods from `&mut self` to `&self`
  - Files: `crates/ocx_lib/src/oci/index/local_index.rs`
  - Methods: `update`, `update_tag`, `update_all_tags`, `sync_tag`, `update_manifest`
  - No behavior change — purely mechanical signature change

- [ ] **Step 1.2:** Create `ChainedIndex` struct with `IndexImpl` stubs
  - Files: `crates/ocx_lib/src/oci/index/chained_index.rs`
  - Public API: `ChainedIndex::new(cache: LocalIndex, sources: Vec<Index>)`, all `IndexImpl` methods as `unimplemented!()`
  - Doc comment explains the cache+sources two-role model and the forward path to N sources

- [ ] **Step 1.3:** Add `Index::from_chained` and `Index::from_cached_remote` constructors
  - Files: `crates/ocx_lib/src/oci/index.rs`
  - Public API:
    - `pub fn from_chained(cache: LocalIndex, sources: Vec<Index>) -> Self` (general)
    - `pub fn from_cached_remote(cache: LocalIndex, remote: RemoteIndex) -> Self` (convenience for #41)

### Phase 2: Architecture Review

Review stubs against this design record. Verify:
- ChainedIndex fields match the contract (local: LocalIndex, remote: Index)
- `from_chained` constructor matches the documented signature
- `update_tag` refactoring compiles with `&self`

Gate: `cargo check` passes with stubs.

### Phase 3: Specification Tests

- [ ] **Step 3.1:** Unit tests for ChainedIndex
  - Files: `crates/ocx_lib/src/oci/index/chained_index.rs` (inline `#[cfg(test)]`)
  - Cases (single-source chain — the #41 shape):
    - Cache hit returns immediately (no source walked)
    - Cache miss + source has tag → update_tag called → retry succeeds
    - Cache miss + source doesn't have tag → returns None, warn logged
    - Cache miss + source network error → returns None, warn logged
    - Digest-only identifier (no tag) → no chain walk, cache result returned
    - Batch: one chain walk succeeds, one fails → partial results
    - `box_clone` shares caches across cloned chain
  - Cases (multi-source chain — proves the Vec shape works):
    - Two sources, first has the tag → second source not queried
    - Two sources, first errors but second has the tag → tag persisted, success
    - Two sources, both error → returns None, both errors logged
    - Empty sources Vec → behaves like LocalIndex alone (no fallback)

- [ ] **Step 3.2:** Acceptance tests for the three friction scenarios + AC6
  - Files: `test/tests/test_tag_fallback.py`
  - Scenarios:
    - Fresh install: `ocx install <pkg>:<tag>` with empty local index succeeds (AC1)
    - Tag persisted: second install with `--offline` succeeds (proves local persistence) (AC2)
    - Offline mode: `ocx install --offline <pkg>:<tag>` with empty index returns NotFound (AC3)
    - Cached tag not refreshed: install with stale cached tag uses the cached digest (AC4)
    - Batch install: `ocx install <pkg1>:<tag1> <pkg2>:<tag2>` with empty index, both succeed (AC5)
    - Non-existent tag: `ocx install <pkg>:nonexistent` returns NotFound, stderr contains diagnostic (AC6)
    - Network failure during fallback: install against unreachable registry, stderr contains network error context (AC6)

Gate: Tests compile and fail with `unimplemented!()`.

### Phase 4: Implementation

- [ ] **Step 4.1:** Implement ChainedIndex `IndexImpl` methods
  - Files: `crates/ocx_lib/src/oci/index/chained_index.rs`
  - `list_repositories` and `list_tags`: delegate to `self.cache`
  - `fetch_manifest` and `fetch_manifest_digest`: cache-first via `with_chain` helper
  - `box_clone`: clone cache + clone all sources (shared caches via Arc)

- [ ] **Step 4.2:** Wire ChainedIndex into Context
  - Files: `crates/ocx_cli/src/app/context.rs`
  - Change `selected_index` construction: online + not remote → `from_cached_remote`

- [ ] **Step 4.3:** Update `index_update.rs` and remove `local_index_mut()`
  - Files: `crates/ocx_cli/src/command/index_update.rs`, `crates/ocx_cli/src/app/context.rs`
  - `index_update.rs:26`: `let mut context = context.clone()` → `let context = context.clone()`
  - `index_update.rs:28`: `context.local_index_mut().update(...)` → `context.local_index().update(...)`
  - `context.rs`: remove `local_index_mut()` accessor (sole caller eliminated), remove `#[allow(dead_code)]` on `local_index()`
  - Update `subsystem-cli.md` accessor list to remove `local_index_mut()`

Gate: All unit tests and acceptance tests pass. `task verify` succeeds.

### Phase 5: Review

- [ ] **Step 5.1:** Spec compliance review (design record vs tests vs implementation)
- [ ] **Step 5.2:** Code quality review (Rust quality checklist, error handling)
- [ ] **Step 5.3:** Verify acceptance criteria from issue #41

## Files to Modify

| File | Action | Description |
|------|--------|-------------|
| `crates/ocx_lib/src/oci/index/local_index.rs` | Modify | Refactor `&mut self` → `&self` on update methods |
| `crates/ocx_lib/src/oci/index/chained_index.rs` | Create | New `ChainedIndex` (cache + sources) implementing `IndexImpl` |
| `crates/ocx_lib/src/oci/index.rs` | Modify | Add `from_chained` + `from_cached_remote` constructors, `mod chained_index` |
| `crates/ocx_cli/src/app/context.rs` | Modify | Wire fallback index; remove `local_index_mut()`, un-deadcode `local_index()` |
| `crates/ocx_cli/src/command/index_update.rs` | Modify | `local_index_mut()` → `local_index()`, remove `mut` on context clone |
| `.claude/rules/subsystem-cli.md` | Modify | Update Context accessor list |
| `test/tests/test_tag_fallback.py` | Create | Acceptance tests for fallback scenarios (all 6 ACs) |

## Dependencies

No new crate dependencies required. All building blocks exist.

## Testing Strategy

### Unit Tests (from component contracts)

| Component | Behavior | Expected | Edge Cases |
|-----------|----------|----------|------------|
| ChainedIndex::fetch_manifest | Cache hit | Return data immediately | — |
| ChainedIndex::fetch_manifest | Cache miss, source has tag | update_tag, retry → data | Concurrent walks for same tag |
| ChainedIndex::fetch_manifest | Cache miss, source miss | Ok(None) | Error caught, warn logged |
| ChainedIndex::fetch_manifest | Cache miss, network error | Ok(None) | Error caught, warn logged |
| ChainedIndex::fetch_manifest | Multi-source, first hits | second never queried | Stops on first success |
| ChainedIndex::fetch_manifest | Multi-source, first errors, second hits | persists, returns data | Errors don't abort the chain |
| ChainedIndex::fetch_manifest | Empty sources Vec | Cache result returned (no walk) | Behaves like LocalIndex |
| ChainedIndex::fetch_manifest_digest | Same matrix as above | Same expected | Digest-only id → no chain walk |
| ChainedIndex::list_tags | Any | Delegates to cache only | — |
| ChainedIndex::list_repositories | Any | Delegates to cache only | — |
| ChainedIndex::box_clone | Clone | Shares caches via Arc | — |

### Acceptance Tests (from user experience)

| User Action | Expected Outcome | Error Cases | AC |
|-------------|------------------|-------------|----|
| `ocx install <pkg>:<tag>` (empty index) | Installs successfully | — | 1 |
| Second `ocx install <pkg>:<tag>` with `--offline` | Uses local (proves persistence) | — | 2 |
| `ocx install --offline <pkg>:<tag>` (empty index) | NotFound error | Expected | 3 |
| `ocx install <pkg>:<stale_tag>` (cached, old digest) | Uses cached digest | — | 4 |
| `ocx install <pkg1>:<tag1> <pkg2>:<tag2>` (empty) | Both install | — | 5 |
| `ocx install <pkg>:nonexistent` | NotFound, stderr has "not found" context | Expected | 6 |
| `ocx install <pkg>:<tag>` (unreachable registry) | NotFound, stderr has network error context | Expected | 6 |

## Risks

| Risk | Mitigation |
|------|------------|
| `update_tag` error semantics differ from expected | Catch all errors from update_tag on fallback path; degrade to Ok(None) |
| Parallel `update_tag` calls for same tag race on disk | `TagLock` write is idempotent for same digest; shared in-memory cache deduplicates |
| Docker Hub rate limiting from many fallback HEADs | Single HEAD per unknown tag is cheaper than `list_tags`; future negative cache if needed |
| `&mut self` → `&self` refactoring breaks callers | Only caller is `index_update.rs` which is updated in the same change |

## Checklist

### Before Starting

- [x] Issue context checked (ocx-sh/ocx#41)
- [x] Related issues checked (#33 complementary, not blocking)
- [x] Research completed (research_tag_fallback.md)
- [x] Branch exists (evelynn worktree)

### Before PR

- [ ] All tests passing
- [ ] `task verify` succeeds
- [ ] Acceptance criteria from #41 verified
- [ ] Self-review complete
