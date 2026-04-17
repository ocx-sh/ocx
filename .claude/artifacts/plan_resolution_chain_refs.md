---
title: Capture Full OCI Resolution Chain in Package refs/blobs/
issue: https://github.com/ocx-sh/ocx/issues/35
scope: Medium (1-2 weeks)
reversibility: Two-Way Door — pure correctness/consistency, no new on-disk format
status: plan
related:
  - .claude/artifacts/adr_three_tier_cas_storage.md
  - .claude/artifacts/plan_tag_fallback.md (ChainedIndex, landed as #41 via commits 89b0b90 / 5569658 / 68f341c)
  - .claude/artifacts/research_tag_fallback.md
  - .claude/artifacts/research_blob_retention_policy.md (prior-art for deferred #50)
breaking_changes:
  - `--remote` flag semantic: forces mutable lookups (tags, catalog) to source but still uses the chained cache for digest-addressed blob reads AND still persists fetched blobs into the cache via write-through
  - `ocx index update` no longer pre-walks manifests; only locks tag→digest pointers
  - `ocx --offline install <pkg>` after a bare `ocx index update <pkg>` now fails — users must run the first install online to populate the blob cache
follow_ups:
  - "#50 — policy-based retention for orphan blobs (shared $OCX_HOME CI scenarios)"
---

# Plan: Capture Full OCI Resolution Chain in Package `refs/blobs/`

## Problem

Three coupled gaps in the post-three-tier-CAS storage model:

**Gap 1 — Incomplete `refs/blobs/` on installed packages.** `cache_manifest_blob` at `pull.rs:488-512` persists only the final platform manifest; `link_blobs_in_temp` at `pull.rs:762-774` links only that one blob into the package's `refs/blobs/`. Every intermediate image index or child platform manifest the resolver walked is a disk orphan — present in `blobs/` but not referenced by any package. `garbage_collection.rs:48` contains a hardcoded `**tier != CasTier::Blob` skip (with the test `unreachable_blobs_skipped_by_clean` at line 200 citing this issue) to protect these orphans from `ocx clean`, because collecting them would break offline re-resolve.

**Gap 2 — Persistence and retention conflated.** `LocalIndex::update_manifest` at `local_index.rs:115-142` is the systematic write-path for manifest blobs. It is triggered from two distinct flows that *look* the same from the store's perspective but have very different user intent:

- `ChainedIndex::walk_chain` during install (cache miss, legitimate write-through)
- `ocx index update` CLI command (explicit user-level tag freeze — pre-walks the manifest chain eagerly as a side effect)

The second flow creates long-lived orphans in `blobs/` that nothing references until a later install *might* link them. In the post-#35 world where the hardcoded blob skip is gone, those orphans become visible to `ocx clean` on the next run, creating a confusing "why did my index update blobs just disappear" story. The right answer is to narrow `ocx index update` to its actual job — locking tag→digest pointers — and let install-time ChainedIndex persistence cover the legitimate write-through case.

**Gap 3 — Latent bug in `update_tags` skip logic.** `local_index.rs:77-85` short-circuits the entire tag refresh when `seed.get(&tag) == Some(&digest)`, which is correct for the tag file but ALSO skips the `update_manifest` call that would re-fetch a missing manifest blob. Today unreachable because nothing deletes blobs. Post-#35, once `ocx clean` can collect orphan blobs, a user who deletes `blobs/` (or whose blob was never persisted in the first place) would hit: tag file says `3.28 → sha256:abc`, manifest file missing, `fetch_manifest` returns `None`, `walk_chain` sees seed match and skips, infinite resolve loop returns `None`. Must be fixed as part of this feature — either by decoupling the checks in `update_tags` or (my preference) by moving orchestration into `ChainedIndex` where "tag cached, manifest missing" becomes a concrete state the walk can address.

**Gap 4 — `--remote` is a type switch, not a behaviour hint.** `context.rs:65-75` has three branches: `--remote` → `Index::from_remote(remote_index)` (no cache, no write-through), default online → `Index::from_cached_remote(local, remote)` (ChainedIndex with write-through), `--offline` → `Index::from_local(local)` (no fallback). In a content-addressed world, digest-addressed reads return identical bytes whether cache or source — the only meaningful axis is whether mutable lookups (tags, catalog) should bypass the cache. The `--remote` branch loses write-through entirely, which means a user running `ocx --remote install cmake:3.28` gets no blob caching at all, even for digest-addressed fetches. That's wrong in principle and becomes more wrong after #35 because the pull pipeline's `link_blobs_batch` call would find nothing persisted to link against.

## Design Revisions (applied 2026-04-13)

Five corrections on review of the original plan — the shape below supersedes any conflicting detail in later sections:

1. **`BlobStore::acquire_write` / `acquire_read` are public** and take a `&oci::PinnedIdentifier` (registry + digest already paired), not `(registry, digest)` positional args. `BlobGuard` is `pub`, not `pub(in crate::file_structure)`. Rationale: widely-scoped visibility qualifiers are a code smell; per `quality-rust.md` public vs private is the only valid split.
2. **`ReferenceManager::link_blobs_batch` is renamed to `link_blobs`** and is the *only* blob-link entry point. Any single-blob use case passes a one-element slice. Old `link_blobs_in_temp` is deleted entirely.
3. **No `pub(super)` / `pub(in …)` anywhere in this change.** Every new item is either `pub` (crate-module boundary) or private to its own module. If visibility needs a middle tier, the module hierarchy is wrong and gets fixed.
4. **No public `lock_tags` primitive.** `LocalIndex` owns tag writes atomically via a single private writer. `ocx index update <pkg>` becomes a high-level method on `Index` (or `LocalIndex`) — something like `refresh_tags(identifier, source: &Index) -> Result<()>` — that fetches from source + writes atomically, with no intermediate primitive exposed to callers. The old `update_tags` / `update_manifest` duplication is removed by consolidating into this single entry, not by adding a second one.
5. **`chain_walk` is deleted.** Chain accumulation becomes a byproduct of `PackageManager::resolve` itself. `resolve` returns a new `ResolvedChain { pinned: PinnedIdentifier, chain: Vec<(String, Digest)>, final_manifest: ImageManifest }` instead of bare `PinnedIdentifier`. The two existing resolve callers (`pull.rs:177`, `find.rs:41`) consume the chain as part of their existing resolve call. The chain is accumulated inside `Index::fetch_candidates` (or `select`) where the tag → image-index → platform-manifest walk already happens. `IndexImpl` trait stays unchanged; only the `Index` wrapper's `select`/`fetch_candidates` return shape is enriched (or a sibling method is added). No separate module, no public `walk_chain` function, no third task module.

All subsequent sections of this plan should be read through these corrections. Where the older text below disagrees, these revisions win.

## Scope

### Fix

Unified ChainedIndex + two-layer responsibility model with chain capture as a byproduct of resolution.

- **Layer 1 — ChainedIndex (automatic write-through):** owns "is the blob on disk?" `walk_chain` is rewritten to explicitly fetch the tag digest and manifest chain from the source, persist both via new low-level LocalIndex primitives, and handle "tag cached / manifest missing" as a concrete state. Each successful `fetch_manifest` call guarantees the returned digest is backed by an on-disk blob. Intra-process `singleflight` dedup (same pattern already used for layer extraction in `pull.rs`) prevents duplicate concurrent fetches.
- **Layer 2 — ChainWalker (package manager helper):** owns "which blobs did we walk?" New `package_manager/tasks/chain_walk.rs` with `walk_chain` helper that calls `Index::fetch_manifest` twice at most (top-level → platform child) and accumulates `(registry, digest)` pairs into a `ChainWalk` struct. Self-contained traversal logic; the `IndexImpl` trait signature stays unchanged.
- **Layer 3 — pull / find callers (reachability via `refs/blobs/`):** owns "is the chain held against GC?" Takes the walker's `Vec<(String, Digest)>` and calls `ReferenceManager::link_blobs_batch` — a new method on the existing `ReferenceManager` at `crates/ocx_lib/src/reference_manager.rs` — for an idempotent forward-ref upsert.

Separation rationale. Trace accumulation is not resolution; it's traversal bookkeeping. Keeping it out of `IndexImpl` makes the trait simple, keeps the walker alongside other task helpers, and means ChainedIndex's job is just "answer `fetch_manifest(id)` correctly and persist what you fetched."

Fail-safe. If Layer 1 writes a blob but Layer 3 never runs (crash between walk and link), the blob is orphaned. Once the hardcoded GC skip is removed, the next `ocx clean` collects it. No correctness hazard, just a retry cost on re-install.

### In scope

**Storage primitive (new):**
- `BlobGuard` in a new file `crates/ocx_lib/src/file_structure/blob_store/blob_guard.rs`, directly modelled on `TagGuard` at `crates/ocx_lib/src/oci/index/local_index/tag_guard.rs`. Shared-for-reads / exclusive-for-writes `fs2` advisory lock on the blob's `data` file itself. No sidecar file. Crash trade: kill-9 mid-write leaves a truncated file; reader sees `Manifest::read_json` parse error; caller treats as cache miss and re-fetches.
- `BlobStore::acquire_write(registry, digest)` and `acquire_read(registry, digest)` wrappers that construct the CAS path (`self.path(registry, digest).join("data")`), ensure parent dirs exist, and call `BlobGuard::acquire_exclusive` / `acquire_shared`. A successful locked write also writes the sibling `digest` marker file via `file_structure::write_digest_file`.

**Index layer — LocalIndex:**
- Add primitive `pub(super) persist_manifest_chain(source: &Index, identifier: &Identifier, digest: &Digest)`: fetches the manifest from `source`, locks + writes to the blob store via `BlobGuard`, recurses for `ImageIndex` children (same algorithm as today's `update_manifest` but with locking and explicit recursion). Used only by `ChainedIndex::walk_chain`.
- Add primitive `pub lock_tags(identifier: &Identifier, fetched: HashMap<String, Digest>)`: exclusive `TagGuard`, read-modify-write merge of `fetched` into the existing on-disk tag map, cache update. Factored out of today's `update_tags:95-112` tail.
- `get_manifest` at `local_index.rs:176-218` gains `BlobStore::acquire_read` around the manifest read. On parse failure (truncated `data` from kill-9), log warn and return `Ok(None)` — treat as cache miss for graceful recovery.
- **Delete** `LocalIndex::update` (`:46-52`), `update_tags` (`:61-113`), `update_manifest` (`:115-142`). Their responsibilities move to ChainedIndex (orchestration + walk-chain logic) and `ocx index update` CLI (tag locking). The latent bug at `:77-85` vanishes because the rewrite has no "skip manifest if seed matches" short-circuit — the tag and manifest paths are independent.

**Index layer — ChainedIndex:**
- Rewrite `walk_chain` at `chained_index.rs:44-78` to own the full orchestration:
  1. Short-circuit digest-only identifiers (unchanged).
  2. Normalise to `tagged = identifier.clone_with_tag(identifier.tag_or_latest())`.
  3. For each source in `self.sources`: fetch the manifest digest; if not present in the cache's blob store OR the cache's tag file, call `cache.persist_manifest_chain(source, &tagged, &digest)` then `cache.lock_tags(&tagged, {tag: digest})`. On first successful source, return. On all-sources-fail, propagate the last error.
  4. Wrap the per-identifier work in `utility::singleflight` keyed by `(registry_slug, resolved_identifier_string)` so two concurrent tasks in one process share the result.
- Add `mode: ChainMode` field (new enum, see below). `list_repositories`, `list_tags`, and `fetch_manifest_digest`-on-tag check the mode before consulting the cache; `Default` and `Offline` behave as today, `Remote` skips the cache read for mutable lookups and falls straight through to the source path.
- `fetch_manifest` and `fetch_manifest_digest` on digest-addressed identifiers ignore the mode (always cache-first with write-through), because digest-addressed content is immutable by construction and the cache can never be wrong about it.

**Index layer — public `Index` wrapper and context wiring:**
- New `ChainMode` enum in `crates/ocx_lib/src/oci/index.rs` (or a sibling file), `#[non_exhaustive]`:
  ```rust
  pub enum ChainMode { Default, Remote, Offline }
  ```
- `Index::from_chained` gains a `mode: ChainMode` parameter. `Index::from_cached_remote` becomes sugar for `from_chained(local, vec![from_remote(remote)], ChainMode::Default)`.
- **Delete** `Index::from_local`. Only used in one place (`context.rs:74` for the `--offline` branch) — replaced by `from_chained(local, vec![], ChainMode::Offline)`.
- **Keep** `Index::from_remote`. Still the right primitive for constructing a single remote source wrapper; used by `ocx index update` CLI (the narrowed version) to pass a source into `lock_tags`.
- `Context::try_init` at `crates/ocx_cli/src/app/context.rs:65-75` collapses to one `Index::from_chained(...)` call site where the mode is derived from `options.offline` and `options.remote`. The three-branch `if/else if/else` at 65-75 is replaced with a single `ChainMode` match and one constructor call. The `remote_index: Option<RemoteIndex>` field and its accessors stay — `ocx index update` still uses them directly.

**CLI layer — `ocx index update`:**
- `crates/ocx_cli/src/command/index_update.rs:18-40` currently calls `context.local_index().update(&remote_index, &identifier)`. Rewrite to: for each tag the user requested, `remote_index.fetch_manifest_digest(&tagged)`, accumulate into a `HashMap<String, Digest>`, then call `local_index.lock_tags(&identifier, fetched)`. No manifest walk. No `blobs/` writes.
- Preserve the tagged-vs-bare semantics documented at `subsystem-cli-commands.md:136`: tagged identifier = that tag's digest only, bare identifier = all tags via `remote_index.list_tags`.

**Package-manager layer — new `chain_walk` helper:**
- New file `crates/ocx_lib/src/package_manager/tasks/chain_walk.rs` with `pub(super) struct ChainWalk` and `pub(super) async fn walk_chain(index: &Index, identifier: &Identifier, platform: &Platform) -> Result<ChainWalk, PackageErrorKind>`. See the type sketch below.
- Walker behaviour: fetch the identifier → push `(registry, top_digest)` to the chain. If top manifest is `Manifest::Image`, done. If `Manifest::ImageIndex`, select the platform child via the existing `fetch_candidates` / `select` machinery (or inline the manifest-descriptor filter), fetch the child, push `(registry, child_digest)`. Return `ChainWalk { chain, final_digest, final_manifest: ImageManifest }`. Reject nested image indexes with a clear error.

**Reference manager — new `link_blobs_batch`:**
- Add method to `ReferenceManager` at `crates/ocx_lib/src/reference_manager.rs`: `pub fn link_blobs_batch(&self, content_path: &Path, chain: &[(String, oci::Digest)]) -> Result<()>`. Implementation: for each `(registry, digest)`, compute `target = self.file_structure.blobs.data(registry, digest)` and `ref_name = cas_ref_name(digest)`, construct `link_path = self.file_structure.packages.refs_blobs_dir_for_content(content_path)? .join(ref_name)`. Read any existing symlink at `link_path` — if present and matches `target`, no-op; else `symlink::update`. On `EEXIST` from a racing peer, re-read and verify target matches; if yes, treat as success; if no (impossible by construction because ref_name is digest-derived), propagate the error.
- `link_blobs_in_temp` at `pull.rs:762-774` is deleted. Its one caller at `pull.rs:354` becomes a call to `ReferenceManager::link_blobs_batch`.

**Pull pipeline:**
- Delete `cache_manifest_blob` at `pull.rs:488-512`. Its single caller at `pull.rs:293` becomes a call to `walk_chain`, whose result is passed to `link_blobs_batch` at the `pull.rs:354` site. The pull flow now: resolve → `walk_chain` (which goes through ChainedIndex which auto-persists) → use `walk.final_manifest` for layer extraction → `link_blobs_batch(&pkg.content(), &walk.chain)` → rename temp to packages.
- Subtle: `cache_manifest_blob` today writes the final-manifest blob AND its `digest` marker file as a side effect of pull — but this work is now done inside ChainedIndex during `walk_chain`. No pull-side persistence remains.

**Find family (`find`, `find_symlink`, `find_or_install`):**
- On every successful resolve of an already-installed package, call `walk_chain(...)` → `ReferenceManager::link_blobs_batch(&package.content(), &walk.chain)`. This covers the "different chain" case where a package was installed via one tag and is later resolved via another. `link_blobs_batch` is idempotent; if the chain is already current, zero symlinks are written.

**GC:**
- Delete the `tier != CasTier::Blob` filter at `garbage_collection.rs:48`. Rename / rewrite the test `unreachable_blobs_skipped_by_clean` at `:200` to `unreachable_blob_is_collected` asserting the positive.

**Docs:**
- `website/src/docs/user-guide.md` — index section, `--remote` semantics.
- `website/src/docs/reference/command-line.md` — `--remote` entry.
- `website/src/docs/reference/environment.md` — `OCX_REMOTE` entry.
- `website/src/docs/getting-started.md` — any `--remote` usage examples still work (semantic narrowing is user-invisible for installs; only the "no cache touched" claim goes away).
- `.claude/rules/subsystem-oci.md` — LocalIndex primitives (`persist_manifest_chain`, `lock_tags`); `ChainMode`; `walk_chain` orchestration.
- `.claude/rules/subsystem-file-structure.md` — `BlobGuard` in the module map; GC Safety section (blobs are first-class BFS entries).
- `.claude/rules/subsystem-package-manager.md` — new `chain_walk` task helper.
- `.claude/rules/arch-principles.md` Utility Catalog — add `BlobGuard` row next to the existing `FileLock` entry.
- `CHANGELOG.md` — two breaking-change lines for `--remote` and `ocx index update`.

### Out of scope

- Policy-based retention (TTL / size-cap / pinning) — #50.
- OCI referrers (signatures, SBOMs) — separate lifecycle, not required for re-resolve.
- Byte-exact manifest persistence. Today's `manifest.write_json` re-serialises a parsed `Manifest`, which is not byte-identical to the registry response — digest verification on read would fail if it were ever added. Pre-existing gap. Future tracking item if byte-exact persistence becomes a hard requirement.
- Proactive migration of existing installed packages' refs. New installs get full chains immediately; existing installs top up on the first `find` against them. Users who want to force the migration can `ocx find <pkg>` each installed package after upgrade.
- `ocx prefetch` command — deferred to whenever an explicit pre-fetch need emerges.

## Acceptance Criteria

1. After `ocx install <pkg>`, the package's `refs/blobs/` contains a forward-ref for every OCI blob the resolver read (image index + platform manifest at minimum).
2. After `ocx find <pkg>` via a tag path that walks blobs not yet linked to the existing package, those blobs are appended to `refs/blobs/` — no duplicate entries, no changed targets, idempotent on re-run.
3. `ocx clean` does not delete any blob reachable via any installed package's `refs/blobs/`.
4. `ocx clean` does delete blobs not reachable from any installed package (e.g., orphans left by a crashed install, or the old chain after `uninstall --purge`).
5. Offline re-resolve of an installed package succeeds for any tag path the package has ever been resolved via.
6. The hardcoded `tier != CasTier::Blob` exemption at `garbage_collection.rs:48` is removed.
7. `ocx index update <pkg>` writes only to `$OCX_HOME/tags/`, never to `$OCX_HOME/blobs/`. Verified by walking `blobs/` before and after and asserting it is unchanged.
8. `ocx --remote install <pkg>` persists the resolution chain into `blobs/` and links it into `refs/blobs/`. `--remote` no longer disables the cache write-through.
9. `ocx --remote index list <pkg>` still refreshes tag data from the source on every invocation.
10. `ocx --offline install <pkg>` after a bare `ocx index update <pkg>` fails with a clear error naming the missing manifest digest.
11. Two concurrent `ocx install` processes against the same `$OCX_HOME` both complete successfully, neither corrupts blob files, both produce full `refs/blobs/`.
12. After any successful install, no sidecar `.lock`, `.log`, or `.tmp` files remain anywhere under `$OCX_HOME/blobs/`.
13. The `update_tags` latent bug is fixed: deleting a manifest `data` file from `blobs/` (leaving the tag file in place) and then running `ocx install <pkg>` re-fetches the manifest and completes successfully (not infinite-loop or `NotFound`).

## Architecture

> **SUPERSEDED** — see [Design Revisions](#design-revisions-applied-2026-04-13). The three-layer model collapses to two (ChainedIndex + callers); `chain_walk` is deleted and chain accumulation is a byproduct of `PackageManager::resolve`. The sketches below are retained for historical context but are no longer the build target.

### Three-layer responsibility model

```
┌──────── Layer 3: callers (pull / find / find_symlink / find_or_install) ────────┐
│                                                                                 │
│  let walk = chain_walk::walk_chain(index, id, platform).await?;                 │
│  reference_manager.link_blobs_batch(&package.content(), &walk.chain)?;          │
│  // use walk.final_digest / walk.final_manifest for downstream pull work        │
│                                                                                 │
└───────────────────────────────────▲─────────────────────────────────────────────┘
                                    │ ChainWalk { chain, final_digest, final_manifest }
┌───────────────────────────────────┴─────────────────────────────────────────────┐
│       Layer 2: ChainWalker — package_manager/tasks/chain_walk.rs (new)          │
│                                                                                 │
│  walk_chain(index, id, platform):                                               │
│    let (top_d, top_m) = index.fetch_manifest(id).await?;                        │
│    chain.push((id.registry(), top_d.clone()));                                  │
│    match top_m {                                                                │
│      Manifest::Image(img) => return (chain, top_d, img),                        │
│      Manifest::ImageIndex(idx) => {                                             │
│        let child_id = id.clone_with_digest(select_platform(&idx, platform)?);  │
│        let (child_d, child_m) = index.fetch_manifest(&child_id).await?;         │
│        chain.push((child_id.registry(), child_d.clone()));                      │
│        return (chain, child_d, child_m.into_image_manifest()?);                 │
│      }                                                                          │
│    }                                                                            │
│                                                                                 │
└───────────────────────────────────▲─────────────────────────────────────────────┘
                                    │ Index::fetch_manifest — trait unchanged
┌───────────────────────────────────┴─────────────────────────────────────────────┐
│             Layer 1: ChainedIndex — automatic write-through                     │
│                                                                                 │
│  fetch_manifest(id):                                                            │
│    if let Some(hit) = self.cache.fetch_manifest(id).await? { return Ok(hit); }  │
│    if self.mode == ChainMode::Offline { return Ok(None); }                      │
│    singleflight.run((registry, id_key), async {                                 │
│      for source in &self.sources {                                              │
│        let Some(digest) = source.fetch_manifest_digest(&tagged).await? else { continue; };│
│        self.cache.persist_manifest_chain(source, &tagged, &digest).await?;     │
│        self.cache.lock_tags(&tagged, {tag → digest}).await?;                    │
│        return Ok(());                                                           │
│      }                                                                          │
│      Err(last_error_from_sources)                                              │
│    }).await?;                                                                   │
│    self.cache.fetch_manifest(id).await                                          │
│                                                                                 │
└─────────────────────────────────────────────────────────────────────────────────┘
```

Key invariants:

1. **`IndexImpl::fetch_manifest` is self-contained.** Same signature, no trace. Resolving an identifier returns `Result<Option<(Digest, Manifest)>>`, nothing more.
2. **Every digest in a `ChainWalk` is guaranteed on disk.** Every `fetch_manifest` call the walker makes goes through ChainedIndex, which either found the blob in cache or persisted it via `persist_manifest_chain` before returning.
3. **`link_blobs_batch` never creates dangling symlinks.** Its input is a `ChainWalk::chain` whose entries are all backed by real `blobs/.../data` files at call time.

### `ChainMode`

```rust
// crates/ocx_lib/src/oci/index.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChainMode {
    /// Cache-first for all lookups. Write-through on cache-miss source fetches.
    /// Used for default (no flag) online operation.
    Default,
    /// Mutable lookups (tags, catalog) bypass cache and go straight to source.
    /// Digest-addressed (immutable) lookups still use cache + write-through.
    /// Used for --remote.
    Remote,
    /// Cache only. Source list is empty or consulted never. Cache misses
    /// return `None` from `fetch_manifest`. Used for --offline.
    Offline,
}
```

### `BlobGuard` — mirror of `TagGuard`

New file `crates/ocx_lib/src/file_structure/blob_store/blob_guard.rs`:

```rust
use std::path::PathBuf;
use std::time::Duration;

use tokio::io::AsyncWriteExt;

use crate::file_lock::FileLock;
use crate::{Result, error::file_error, prelude::*};

const LOCK_TIMEOUT: Duration = Duration::from_secs(60);

/// Per-blob reader/writer guard over a content-addressed `data` file.
///
/// Holds an `fs2` advisory lock — shared for reads, exclusive for writes —
/// directly on the `data` file itself. No sidecar `.lock`, no temp sibling,
/// no atomic rename: writers lock the file and update it in place
/// (truncate + write + `sync_all`).
///
/// Modelled exactly on `TagGuard`. Crash trade: kill-9 mid-write can leave
/// the `data` file truncated; the next `Manifest::read_json` attempt will
/// fail to parse, `LocalIndex::get_manifest` logs at warn and returns
/// `None`, and `ChainedIndex::walk_chain` re-fetches the blob via
/// `persist_manifest_chain`. Safe because blob content is immutable by
/// digest — any re-fetch produces identical bytes.
pub(in crate::file_structure) struct BlobGuard {
    _lock: FileLock,
    target_path: PathBuf,
}

impl BlobGuard {
    pub async fn acquire_exclusive(target_path: PathBuf) -> Result<Self> { /* spawn_blocking open + fs2 exclusive, 60s timeout */ }
    pub async fn acquire_shared(target_path: PathBuf) -> Result<Option<Self>> { /* None if file missing */ }
    pub async fn write_bytes(&self, bytes: &[u8]) -> Result<()> { /* truncate(true) + write_all + sync_all */ }
    pub async fn read_bytes(&self) -> Result<Vec<u8>> { /* read_to_end */ }
}
```

Exposed from `BlobStore` via wrapper methods:

```rust
impl BlobStore {
    /// Acquire an exclusive lock on the blob `data` file for the given
    /// registry + digest, creating parent directories. Writers must hold
    /// this guard while calling `write_bytes`.
    pub async fn acquire_write(&self, registry: &str, digest: &oci::Digest) -> Result<BlobGuard> { /* ... */ }

    /// Acquire a shared lock on the blob `data` file. Returns `None` if
    /// the file does not exist.
    pub async fn acquire_read(&self, registry: &str, digest: &oci::Digest) -> Result<Option<BlobGuard>> { /* ... */ }
}
```

**SUPERSEDED** — `BlobGuard` is `pub` and re-exported from `file_structure` (see revision §1). `BlobStore::acquire_write` / `acquire_read` take `&PinnedIdentifier` (not `(registry, digest)`) and are `pub`.

### ~~`ChainWalk` and the walker~~ (SUPERSEDED — see revision §5)

The block below is historical. `chain_walk` is deleted; `PackageManager::resolve` returns a `ResolvedChain { pinned, chain, final_manifest }` that subsumes the walker.

### `ChainWalk` and the walker (historical)

```rust
// crates/ocx_lib/src/package_manager/tasks/chain_walk.rs
use crate::oci;
use super::super::error::PackageErrorKind;

pub(super) struct ChainWalk {
    /// (registry, digest) pairs in walk order. First entry is the top-level
    /// manifest (image or image index); for image indexes, the second entry
    /// is the platform-selected child manifest. Every entry is backed by a
    /// real blob file at `file_structure.blobs.data(registry, digest)` on
    /// disk at the moment this struct is returned.
    pub chain: Vec<(String, oci::Digest)>,
    pub final_digest: oci::Digest,
    pub final_manifest: oci::ImageManifest,
}

pub(super) async fn walk_chain(
    index: &oci::index::Index,
    identifier: &oci::Identifier,
    platform: &oci::Platform,
) -> Result<ChainWalk, PackageErrorKind>;
```

Visibility: `pub(super)` — visible to sibling task modules under `package_manager/tasks/`, not exposed publicly.

### ~~New `LocalIndex` primitives~~ (SUPERSEDED — see revision §4)

After revision §4, `LocalIndex` exposes a single high-level primitive:

```rust
impl LocalIndex {
    /// Atomic tag refresh: fetches from `source` and writes under the
    /// existing `TagGuard` in one call. No intermediate `persist_manifest_chain`
    /// or `lock_tags` surface is exposed.
    pub async fn refresh_tags(
        &self,
        identifier: &oci::Identifier,
        source: &super::Index,
    ) -> Result<()>;
}
```

Manifest persistence during a `ChainedIndex::fetch_manifest` cache miss
becomes a private detail of the chained index; no separate public API.

### New `LocalIndex` primitives (historical)

```rust
impl LocalIndex {
    /// Fetches the manifest for `identifier` from `source` and persists it
    /// (and, for image indexes, every child manifest) into the blob store
    /// under per-file `BlobGuard` exclusive locks.
    ///
    /// Callers must pre-resolve `digest` via `source.fetch_manifest_digest`
    /// so this method does not double-dispatch the manifest lookup.
    ///
    /// Caller contract: on `Ok(())`, every digest in the persisted chain
    /// has a readable `data` file at the expected CAS-sharded path.
    pub(super) async fn persist_manifest_chain(
        &self,
        source: &super::Index,
        identifier: &oci::Identifier,
        digest: &oci::Digest,
    ) -> Result<()>;

    /// Merges `fetched` into the on-disk tag file under an exclusive
    /// `TagGuard`. Preserves existing disk entries not present in
    /// `fetched`. Updates the in-memory cache to reflect the merged state.
    ///
    /// This is the only public tag write path after the refactor. Used by
    /// `ChainedIndex::walk_chain` (single-tag writes) and by the `ocx
    /// index update` CLI command (batched writes).
    pub async fn lock_tags(
        &self,
        identifier: &oci::Identifier,
        fetched: std::collections::HashMap<String, oci::Digest>,
    ) -> Result<()>;
}
```

### `ReferenceManager::link_blobs` (renamed — see revision §2)

```rust
impl ReferenceManager {
    /// Idempotently upserts a `refs/blobs/` forward-ref for every entry in
    /// `chain`. The link name is derived from the digest via `cas_ref_name`
    /// so concurrent peers producing the same chain produce identical
    /// symlinks — races resolve to the correct state.
    ///
    /// Caller contract: every `(registry, digest)` in `chain` must already
    /// be backed by an on-disk `blobs/{registry}/.../data` file. Violation
    /// produces `Error::InternalFile` at link time.
    pub fn link_blobs(
        &self,
        content_path: &std::path::Path,
        chain: &[(String, oci::Digest)],
    ) -> Result<()>;
}
```

### Concurrency safety — four hazards

**H1 — intra-process duplicate fetches.** Two async tasks in one process both want the same manifest on cache miss. Mitigation: ChainedIndex wraps the source-walk branch in `utility::singleflight` (see `arch-principles.md` Utility Catalog) keyed by `(registry_slug, resolved_id_string)`. Same pattern already used for layer extraction in `pull.rs:590-611` and for dependency setup. First task fetches + persists; peers wait on the watch channel and re-read from cache.

**H2 — inter-process blob writes.** Two `ocx` invocations writing the same blob concurrently. Mitigation: `BlobGuard` exclusive `fs2` lock on the `data` file itself — directly modelled on `TagGuard`. Crash trade documented above. No sidecar file.

**H3 — inter-process `refs/blobs/` upsert races.** Two finds upserting refs on the same installed package concurrently. Mitigation: `link_blobs` is idempotent per entry (read existing symlink, compare target, no-op if correct). On `EEXIST` from a racing peer on `symlink::create`, re-read and verify the target matches; if yes, treat as success. Targets are deterministic from the digest-derived `cas_ref_name`, so "same digest, different target" is structurally impossible.

**H4 — `ocx clean` concurrent with install / find.** Out of scope. Inherits the existing convention from `garbage_collection.rs:94`: *"No guard against concurrent installs. Do not run `clean` while other OCX operations are in progress."* Documented in the user guide under `ocx clean`.

**No-sidecar policy.** `BlobGuard` locks the target `data` file directly. `TagGuard` locks the target tag file directly. `TempStore` has a sibling `.lock` only because it needs atomic `temp/ → packages/` rename (which would rotate the inode); that sidecar is auto-deleted on `Drop`. Any future lock location must follow one of these two patterns — direct-lock or auto-cleaned-sibling — never a dangling sidecar. Enforced structurally by acceptance test AC12.

### Integration points

| Site | File:line (merged main) | Change |
|---|---|---|
| New primitive | `crates/ocx_lib/src/file_structure/blob_store/blob_guard.rs` (new) | Create `BlobGuard`, modelled on `tag_guard.rs` |
| BlobStore wrappers | `crates/ocx_lib/src/file_structure/blob_store.rs` | Add `acquire_write` / `acquire_read`; write sibling `digest` file after successful `write_bytes` |
| New type | `crates/ocx_lib/src/oci/index.rs` (or sibling) | `ChainMode` enum |
| `Index` wrapper | `crates/ocx_lib/src/oci/index.rs:41-77` | Add `mode` parameter to `from_chained` / `from_cached_remote`; delete `from_local` (line 48-52). Keep `from_remote` (used by `index update` CLI) |
| `LocalIndex` primitive | `crates/ocx_lib/src/oci/index/local_index.rs` | Add single `refresh_tags(identifier, source)` entry point (revision §4). `get_manifest:176-218` gains `BlobStore::acquire_read` + graceful parse-failure recovery |
| `LocalIndex` deletions | `crates/ocx_lib/src/oci/index/local_index.rs:46-52, 61-113, 115-142` | Delete `update`, `update_tags`, `update_manifest` |
| `ChainedIndex` | `crates/ocx_lib/src/oci/index/chained_index.rs:22-79` | Add `mode: ChainMode` field; rewrite `fetch_manifest`'s cache-miss path to persist fetched manifest + children via `BlobStore::acquire_write` and call `cache.refresh_tags` atomically; wrap per-identifier work in `singleflight` |
| `ChainedIndex` modes | `crates/ocx_lib/src/oci/index/chained_index.rs:82-130` | `list_repositories`, `list_tags`, `fetch_manifest_digest`-on-tag check `self.mode`; `Remote` skips cache read for mutable lookups; `Offline` short-circuits source walk |
| `Context::try_init` | `crates/ocx_cli/src/app/context.rs:65-75` | Collapse three-branch `if/else if/else` into one `ChainMode` match + one `Index::from_chained(...)` call |
| `CLI ocx index update` | `crates/ocx_cli/src/command/index_update.rs:18-40` | Rewrite to call `local_index.refresh_tags(&identifier, remote_wrapped_as_index)` — single atomic call, no manifest walk (revision §4) |
| Resolve return shape | `crates/ocx_lib/src/package_manager/tasks/resolve.rs` | `PackageManager::resolve` returns `ResolvedChain { pinned, chain, final_manifest }` (revision §5); chain accumulation is a byproduct of the existing `select` path, no separate `chain_walk` module |
| New ref method | `crates/ocx_lib/src/reference_manager.rs` | Add `link_blobs` method — idempotent upsert via `symlink::update`, `EEXIST` tolerated (revision §2) |
| Pull pipeline | `crates/ocx_lib/src/package_manager/tasks/pull.rs:293, 354, 488-512, 762-774` | Replace `cache_manifest_blob` + `link_blobs_in_temp` with `ReferenceManager::link_blobs` over `ResolvedChain::chain`. Delete the two free functions |
| Find | `crates/ocx_lib/src/package_manager/tasks/find.rs` | After successful resolve of an already-installed package, `ReferenceManager::link_blobs` over `ResolvedChain::chain` against the existing package's `content/` |
| Find-symlink | `crates/ocx_lib/src/package_manager/tasks/find_symlink.rs` | Same |
| Find-or-install | `crates/ocx_lib/src/package_manager/tasks/find_or_install.rs` | On the already-installed branch: `ReferenceManager::link_blobs` over `ResolvedChain::chain`. On the install branch: pull pipeline already covers it |
| GC skip removal | `crates/ocx_lib/src/package_manager/tasks/garbage_collection.rs:48` | Delete `**tier != CasTier::Blob &&` from the filter |
| GC test inversion | `crates/ocx_lib/src/package_manager/tasks/garbage_collection.rs:200-204` | `unreachable_blobs_skipped_by_clean` → `unreachable_blob_is_collected`, assert positive |
| Subsystem rule | `.claude/rules/subsystem-oci.md` | Document `refresh_tags`, `ChainMode`, `singleflight` in `ChainedIndex::fetch_manifest` cache-miss path (revision §4) |
| Subsystem rule | `.claude/rules/subsystem-file-structure.md` | `BlobGuard` (pub) in module map; GC Safety section updated |
| Subsystem rule | `.claude/rules/subsystem-package-manager.md` | `PackageManager::resolve` returns `ResolvedChain` (revision §5) — no `chain_walk` module |
| Arch rule | `.claude/rules/arch-principles.md` Utility Catalog | Row for `BlobGuard` alongside `FileLock` |
| User docs | `website/src/docs/user-guide.md`, `reference/command-line.md`, `reference/environment.md`, `getting-started.md` | `--remote` semantic change, `ocx index update` narrowing, offline install requirement |
| CHANGELOG | `CHANGELOG.md` | Two breaking-change entries |

### Error / edge cases

| Condition | Behaviour |
|---|---|
| `walk_chain` input has nested image index (image index pointing at another image index) | Return `PackageErrorKind::Internal` with a clear error — not a supported OCI shape, rejected early |
| `walk_chain` input is digest-only, identifier points at an `ImageIndex` | Fetch the index, `select_platform` picks a child, fetch the child. Same code path as tag-based, because `Index::fetch_manifest` returns the manifest for whatever identifier you pass |
| `persist_manifest_chain` succeeds for parent index but fails for a child | Parent blob is persisted but child blob is not; `walk_chain` re-tries on next invocation. Parent blob is orphaned until the retry or until `ocx clean` collects it. Acceptable retry cost |
| `BlobGuard::acquire_exclusive` times out (60 s) | Returns `Error::InternalFile(path, TimedOut)`. Surface to user with a clear message — another process is holding the lock |
| `get_manifest` parse failure on truncated `data` file (post-kill-9) | Log warn, return `Ok(None)`. `ChainedIndex` sees cache miss, falls through to `walk_chain` which re-fetches |
| `link_blobs_batch` encounters a chain entry whose blob file is missing | Returns `Error::InternalFile(missing_path, NotFound)`. Violates the walker's invariant — hard error |
| `ocx index update` in `--offline` mode | Error at CLI entry (today's behaviour — `remote_index()` returns `Err(OfflineMode)`). Unchanged |
| Concurrent `ocx install foo` + `ocx install bar` sharing an image index blob | Both go through ChainedIndex; `singleflight` deduplicates the fetch if identifiers happen to collide, otherwise `BlobGuard` serialises the file write. Either way final state is correct |

## User Experience Scenarios

### UX1 — Fresh install captures full chain

```
$ ocx install cmake:3.28
# ChainedIndex walks tag → image index (sha256:idx) → platform manifest (sha256:M).
# Both blobs are persisted to ~/.ocx/blobs/ocx.sh/... and linked into
# the package's refs/blobs/.

$ ls ~/.ocx/packages/ocx.sh/sha256/.../refs/blobs/
<cas_ref_name(idx)>
<cas_ref_name(M)>
```

### UX2 — `find` via different tag appends refs

```
$ ocx install cmake:latest      # chain {idxA, M}
$ ocx find cmake:3.28           # walks {idxB, M} — same M, different image index
# refs/blobs/ now contains {idxA, idxB, M}. No dup M. idxB appended idempotently.
```

### UX3 — `--remote` forces tag lookup but still caches blobs

```
$ ocx install cmake:3.28           # cache miss on tag → walk_chain → persist + link
$ ocx --remote install cmake:3.28  # forces remote tag re-fetch; digest matches cached M;
                                   # blob cache hit; zero blob download
$ ocx --remote install cmake:latest
                                   # remote tag lookup returns new digest; walk_chain
                                   # persists the new chain + links refs/blobs/
```

### UX4 — `ocx index update` writes only tags

```
$ ocx index update cmake
# Writes ~/.ocx/tags/ocx.sh/cmake.json with the latest tag→digest map.
# Writes NOTHING under ~/.ocx/blobs/.

$ ocx --offline install cmake:3.28
Error: manifest sha256:M not in local cache. Run `ocx install cmake:3.28` online to populate the blob cache.
```

### UX5 — Orphan cleanup after failed install

```
$ ocx install cmake:3.28        # network fails mid-chain; image index blob is persisted
                                # but install aborts before link_blobs_batch runs
$ ocx clean
Removed 1 unreferenced blob.
$ ocx install cmake:3.28        # retry re-downloads the chain, succeeds
```

### UX6 — Offline re-resolve survives clean

```
$ ocx install cmake:3.28        # full chain persisted + linked
$ ocx --offline find cmake:3.28 # succeeds, reads from cache
$ ocx clean                     # zero collections; everything reachable via refs/blobs/
$ ocx --offline find cmake:3.28 # still succeeds
```

### UX7 — Latent-bug fix: missing manifest after tag lock

```
$ ocx index update cmake        # locks cmake:3.28 → sha256:M in tags/
$ rm -rf ~/.ocx/blobs/          # user nukes the blob store (or GC collected it)
$ ocx install cmake:3.28        # tag file says M, manifest missing — walk_chain
                                # re-fetches via source, persists, installs successfully
```

## Testing Strategy

### Unit tests — `file_structure/blob_store/blob_guard.rs` (new)

Mirror `tag_guard.rs` tests line-for-line structurally:

1. `acquire_exclusive_creates_blob_file_and_parent_dirs`
2. `acquire_shared_on_missing_file_returns_none`
3. `acquire_shared_returns_some_when_present`
4. `shared_locks_can_coexist`
5. `shared_blocks_behind_exclusive`
6. `second_exclusive_blocks_behind_first`
7. `write_bytes_truncates_and_syncs`
8. `read_bytes_round_trips_written_content`
9. `no_sidecar_lock_file_created_after_acquire_write_drop`
10. `kill_9_simulation_leaves_file_readable_but_manifest_parse_fails` (writes partial JSON, asserts `Manifest::read_json` errors)

### Unit tests — `file_structure/blob_store.rs` (revision §1 — `&PinnedIdentifier` API)

11. `acquire_write_writes_sibling_digest_marker_file` (via `BlobStore::acquire_write(&pinned)`)
12. `acquire_write_then_acquire_read_returns_bytes`
13. `concurrent_acquire_write_on_same_digest_serialises` (8 tasks race; final file is correct)

### Unit tests — `oci/index/local_index.rs` (revision §4 — retargeted at `refresh_tags` + ChainedIndex write-through)

14. `refresh_tags_merges_new_tags_with_existing_disk_entries`
15. `refresh_tags_preserves_tags_not_in_source`
16. `refresh_tags_concurrent_callers_both_visible_on_disk`
17. `chained_fetch_manifest_persists_image_blob_at_expected_cas_path`
18. `chained_fetch_manifest_recurses_for_image_index_children`
19. `chained_fetch_manifest_writes_sibling_digest_marker_for_every_blob`
20. `get_manifest_on_truncated_blob_file_returns_none_and_logs_warn`
21. `latent_bug_fix_missing_manifest_triggers_refetch_via_chain` — integration-style: seed a tag file with a digest whose blob is absent; `fetch_manifest` via ChainedIndex must re-fetch and return `Some`

### Unit tests — `oci/index/chained_index.rs`

22. `default_mode_cache_hit_returns_without_touching_sources`
23. `default_mode_cache_miss_walks_source_and_persists_chain_on_disk` — property: after call, `blob_store.data(registry, digest)` file exists for every chain entry
24. `remote_mode_bypasses_cache_for_tag_lookup_but_still_persists_blobs`
25. `remote_mode_digest_addressed_lookup_uses_cache`
26. `offline_mode_cache_miss_returns_none_without_consulting_sources`
27. `offline_mode_cache_hit_returns_from_disk`
28. `singleflight_dedups_concurrent_identical_cache_miss_fetches` — 4 concurrent tasks on same identifier; exactly 1 source fetch recorded by test transport
29. `singleflight_broadcasts_source_error_to_waiters`
30. `list_tags_respects_chain_mode` — Default uses cache; Remote hits source and persists; Offline is cache-only
31. `list_repositories_respects_chain_mode`
32. `fetch_manifest_post_persist_is_guaranteed_on_disk` — property-style: for any mode, after a successful `fetch_manifest(id)` returning `Some((digest, _))`, `blob_store.data(registry, digest)` exists

### Unit tests — `package_manager/tasks/resolve.rs` (revision §5 — replaces deleted `chain_walk.rs`)

33. `resolve_single_image_returns_one_chain_entry`
34. `resolve_image_index_returns_two_chain_entries`
35. `resolve_rejects_nested_image_index`
36. `resolve_result_every_entry_has_on_disk_blob_file` — property guarantee

(Note: revised from the 6 chain_walk tests to 4 resolve tests. The
"unsupported platform" and "fetch_manifest not found" cases are covered by
the existing `select` path and its pre-existing tests — nothing new to
assert at the revised `resolve` boundary.)

### Unit tests — `reference_manager.rs` (revision §2 — renamed `link_blobs_batch` → `link_blobs`)

37. `link_blobs_creates_symlinks_for_all_chain_entries`
38. `link_blobs_idempotent_on_existing_correct_symlinks`
39. `link_blobs_tolerates_eexist_when_target_matches`
40. `link_blobs_updates_stale_symlink_target` (impossible by construction but test the recovery path)
41. `link_blobs_missing_blob_file_returns_error`

### Unit tests — `package_manager/tasks/garbage_collection.rs`

44. `unreachable_blob_is_collected` (replaces `unreachable_blobs_skipped_by_clean`)
45. `reachable_blob_via_refs_blobs_survives_gc`
46. `purge_cascades_through_intermediate_chain_blobs` (existing `purge_cascades_through_blobs` at `:302` generalised to full chain)

### Acceptance tests — `test/tests/test_resolution_chain_refs.py` (new)

47. `test_install_creates_full_chain_refs` — AC1
48. `test_find_via_different_tag_appends_refs` — AC2
49. `test_clean_retains_reachable_blobs` — AC3
50. `test_clean_collects_orphaned_chain_after_uninstall_purge` — AC4
51. `test_offline_reresolve_survives_clean_after_full_chain_capture` — AC5
52. `test_index_update_writes_only_tag_files_not_blobs` — AC7 (walks `blobs/` before and after)
53. `test_remote_flag_install_persists_and_links_chain` — AC8
54. `test_remote_flag_index_list_refreshes_tags_from_source` — AC9
55. `test_offline_install_after_bare_index_update_fails_cleanly` — AC10
56. `test_failed_install_leaves_collectable_orphans` — UX5
57. `test_parallel_install_races_preserve_full_chain` — AC11, two real `ocx install` subprocesses against one `$OCX_HOME`
58. `test_no_sidecar_lock_files_in_blobs_dir_after_install` — AC12, walks `$OCX_HOME/blobs/` and asserts no `.lock`, `.log`, `.tmp` files
59. `test_missing_manifest_after_index_update_recovers_on_install` — AC13, UX7, the latent-bug fix
60. `test_find_read_only_against_matching_chain_makes_no_writes` — fast-path proof

## Executable Phases

### Phase A — Prerequisite

PR #45 is merged (commits 89b0b90 / 5569658 / 68f341c). `ChainedIndex`, `TagGuard`, and `Index::from_chained` / `from_cached_remote` already exist on `main`. This plan rewrites and extends them.

### Phase B — Stub

B.1 Create `file_structure/blob_store/blob_guard.rs` with `BlobGuard` struct + method stubs.
B.2 Add `BlobStore::acquire_write` / `acquire_read` wrapper stubs in `blob_store.rs`.
B.3 Add `ChainMode` enum in `oci/index.rs`. Thread an unused `mode` parameter through `from_chained` construction sites. Keep the compiler happy.
B.4 Add `LocalIndex::persist_manifest_chain` and `LocalIndex::lock_tags` stubs alongside the existing methods (don't delete the existing methods yet — Phase E does that).
B.5 Create `package_manager/tasks/chain_walk.rs` with `ChainWalk` struct and `walk_chain` stub.
B.6 Add `ReferenceManager::link_blobs_batch` stub in `reference_manager.rs`.
B.7 Touch `Context::try_init`, `command/index_update.rs`, `pull.rs`, `find.rs`, `find_symlink.rs`, `find_or_install.rs` with `// TODO(#35)` comments at the known call sites.

Gate: `cargo check --workspace` passes. The `IndexImpl` trait is unchanged, so no ripple into test transports.

### Phase C — Verify stubs

`task rust:verify` clean.

### Phase D — Specify tests

Write all 60 tests against stubs. Each must fail with `unimplemented!()` or the expected "not yet wired" assertion. The existing `unreachable_blobs_skipped_by_clean` test (`garbage_collection.rs:200-204`) is renamed and inverted in this phase as part of test 44.

Gate: `cargo nextest run -p ocx_lib` — new tests fail with `unimplemented!()`; pre-existing tests still green.

### Phase E — Implement

E.1 `BlobGuard` + `BlobStore::acquire_write` / `acquire_read` + sibling `digest` marker write (tests 1-13).

E.2 `LocalIndex::persist_manifest_chain` using `BlobStore::acquire_write`, handling the `ImageIndex` recursion. `LocalIndex::lock_tags` factored from the existing `update_tags` tail (tests 14-19).

E.3 `LocalIndex::get_manifest` gains `BlobStore::acquire_read` wrapping + graceful parse-failure recovery (tests 20-21).

E.4 `ChainedIndex` rewrite: `mode: ChainMode` field, `walk_chain` owns orchestration, per-identifier `singleflight`, mode-aware routing for mutable-lookup methods (tests 22-32).

E.5 Delete `LocalIndex::update`, `update_tags`, `update_manifest`. Compiler guides remaining callers to `persist_manifest_chain` + `lock_tags`. Delete `Index::from_local`.

E.6 `Context::try_init` collapses to one `from_chained` call with `ChainMode` derived from flags. `CLI ocx index update` rewired to `fetch_manifest_digest` + `lock_tags` loop (test 52).

E.7 `package_manager/tasks/chain_walk.rs` implementation (tests 33-38).

E.8 `ReferenceManager::link_blobs_batch` (tests 39-43).

E.9 Pull pipeline: replace `cache_manifest_blob` + `link_blobs_in_temp` with `walk_chain` + `link_blobs_batch`. Delete the two free functions (acceptance test 47).

E.10 Find-family upserts (`find`, `find_symlink`, `find_or_install`) on the already-installed branch (acceptance tests 48, 60).

E.11 Delete `garbage_collection.rs:48` skip; invert the test (tests 44-46; acceptance tests 49-50).

E.12 Acceptance tests for `--remote`, `--offline`, parallel install, sidecar check, latent-bug fix (acceptance tests 51, 53-59).

### Phase F — Review-fix loop

Round 1 (parallel):

- `worker-reviewer` spec-compliance — AC1-13 coverage; "every walker chain entry is on disk" invariant upheld across all modes; find-family writes are minimal and idempotent.
- `worker-reviewer` rust-quality — `quality-rust.md` checklist. Special attention: no `MutexGuard` across `.await`; `fs2::FileExt` calls wrapped in `spawn_blocking` (matches `TagGuard`); `#[non_exhaustive]` on `ChainMode`; `thiserror` for new error variants if any.
- `worker-reviewer` concurrency — `BlobGuard` parallels `TagGuard` faithfully; no sidecar files anywhere; `EEXIST` handling on symlinks correct; `singleflight` key scheme prevents same-identifier-different-tag collision; `ocx clean` concurrent-with-install convention inherited and documented.
- `worker-architect` — three-layer separation clean; `IndexImpl` trait genuinely unchanged; the latent bug fix lands without reintroducing the "tag match skip" short-circuit; migration note for existing installed packages is accurate.

Deferred-finding bar: any policy-retention suggestion is pushed to #50. Any byte-exact manifest persistence concern is out of scope.

Optional: one Codex adversarial pass on the diff after convergence.

### Phase G — Docs + commit

- `website/src/docs/user-guide.md` — index section (remote semantics, offline install requirement).
- `website/src/docs/reference/command-line.md` — `--remote` flag section.
- `website/src/docs/reference/environment.md` — `OCX_REMOTE` section.
- `website/src/docs/getting-started.md` — `--remote` example updates.
- `.claude/rules/subsystem-oci.md` — `persist_manifest_chain`, `lock_tags`, `ChainMode`, `singleflight` in `walk_chain`.
- `.claude/rules/subsystem-file-structure.md` — `BlobGuard` in module map; GC Safety section (blobs are first-class BFS entries).
- `.claude/rules/subsystem-package-manager.md` — `chain_walk` task helper.
- `.claude/rules/arch-principles.md` Utility Catalog — `BlobGuard` row.
- `CHANGELOG.md` — breaking-change lines for `--remote` and `ocx index update`.

Commits (conventional, split by concern):

1. `feat(store): BlobGuard — fs2-locked direct writes to CAS blob files`
2. `refactor(oci): LocalIndex exposes persist_manifest_chain + lock_tags primitives`
3. `refactor(oci): ChainedIndex owns walk_chain orchestration with singleflight`
4. `feat(oci): ChainMode unifies --remote / --offline / default handling`
5. `refactor(cli): narrow ocx index update to tag-locking`
6. `feat(package-manager): chain_walk helper for resolution chain traversal`
7. `feat(store): ReferenceManager::link_blobs_batch idempotent refs/blobs/ upsert`
8. `feat(store): link full resolution chain into package refs/blobs/`
9. `feat(gc): collect unreferenced blobs now that chains are linked`
10. `fix(oci): re-fetch manifest when tag is cached but blob is missing`
11. `docs: --remote semantic change and index update narrowing`

## Dependencies

- **#27** — three-tier CAS (landed).
- **#41 / PR #45** — ChainedIndex + TagGuard (landed as commits 89b0b90 / 5569658 / 68f341c). This plan rewrites parts of ChainedIndex and LocalIndex on top of that base.

## Risks

| Risk | Severity | Mitigation |
|---|---|---|
| Trait change accidentally introduced via `LocalIndex` public API | Low | `persist_manifest_chain` is `pub(super)`; `lock_tags` is `pub` but doesn't cross trait boundary. `IndexImpl` trait signature genuinely unchanged |
| Existing installed packages ship without full chains; `clean` prunes their idx blobs on first run after upgrade | Medium | First `find` against each existing package upserts the chain. Document in release notes: "run `ocx find <pkg>` at least once per installed package before the first `clean` after upgrade". Alternative: ship a one-shot migration in Phase G — rejected because it spreads the cost across every command |
| `--remote` behaviour change confuses users | Low | CLI reference + user guide updated in Phase G. Net-beneficial (write-through now applies) |
| `ocx index update` narrowing breaks CI workflows that relied on eager manifest caching | Medium | CHANGELOG calls it out. Workaround: run `ocx install` once online. This IS a breaking change; the user has explicitly accepted breaking changes for v0.3 |
| `ChainedIndex::walk_chain` rewrite introduces a subtle regression in the tag-fallback behaviour covered by PR #45's tests | Medium | PR #45 ships `test_tag_fallback.py` + extensive ChainedIndex unit tests in `chained_index.rs` tests module; Phase F runs both. Phase E.4 explicitly calls out running the existing tag-fallback tests as the regression guard |
| `LocalIndex::update_manifest` removal breaks a caller I haven't found | Low | Compiler catches all call sites; any remaining usage redirects to `persist_manifest_chain` |
| Kill-9 mid-write leaves a truncated blob and `Manifest::read_json` fails in an unexpected path | Low | Test 10 simulates partial writes; test 21 covers the full recovery round-trip through ChainedIndex |
| Pre-existing non-byte-exact manifest persistence (`write_json` re-serialises) | Low | Out of scope; future tracking item if byte-exact persistence becomes required |
| The `singleflight` key scheme collides for two different tags on the same repo | Low | Key is `(registry_slug, resolved_id_string)` — the resolved identifier string includes the tag. Test 28 guards this |

## Out-of-Scope Follow-Ups

- **#50** — policy-based retention for orphan blobs in shared-`$OCX_HOME` CI scenarios. Depends on this issue.
- **Byte-exact manifest persistence** — `write_json` re-serialises parsed manifests; the stored bytes are not byte-identical to the registry response. No digest re-verification on read. File a tracking issue if this becomes a hard requirement.
- **`ocx prefetch` command** — deferred to whenever an explicit pre-fetch need emerges.
