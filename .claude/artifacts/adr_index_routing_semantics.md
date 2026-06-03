# ADR: Index Routing Semantics

## Metadata

**Status:** Accepted
**Date:** 2026-04-27
**Deciders:** mherwig
**Related Issues:** #33 (project toolchain config + pin lock)
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md`
**Domain Tags:** architecture | oci | cli
**Supersedes:** N/A
**Working note:** [`feedback_index_routing_semantics.md`](./feedback_index_routing_semantics.md)

## Context

OCX has three index roles: a remote registry (source of truth), a curated local index at `$OCX_HOME/{tags,blobs}/`, and a CLI surface that combines them. Pre-Phase-11, the `IndexImpl` trait conflated query and update intent on a single `fetch_manifest` method. A pure-query command (`ocx index list --platforms`) and an install-path resolver both invoked the same surface; on cache miss, `ChainedIndex::fetch_manifest` walked the source chain and persisted the result, leaking writes through query paths. The catalog generator workaround `unset OCX_INDEX` in `website/catalog.taskfile.yml` existed only because of this leak.

The `sion` branch added project-level pin locking (`ocx.lock`). Pinned-id pulls (`cmake:1.0@sha256:…`) are content-addressed and authoritative — the lock file is the canonical record of tag→digest. Persisting a tag pointer at pull time silently shadows the lock with a registry's current view of the tag, defeating reproducibility guarantees.

Three gaps surfaced as a single class of routing defect:

1. **`fetch_manifest` (tag-addressed) auto-mutates local index on query.** A pure-query command silently triggered a chain walk and tag-pointer commit.
2. **Pinned-id `ocx pull` writes a redundant tag pointer.** Even with the digest pinned, the cache-miss path committed `tags/{repo}.json`.
3. **No structural guard against future query→write leaks.** A new query caller could reintroduce the bug without any test catching it.

The user-stated mental model:

| Mode | Mutable lookups | Content-addressed lookups | Local index writes |
|------|------------------|----------------------------|---------------------|
| `--remote` | Source only, no local read | Cache-friendly (immutable) | Never on query |
| `--offline` | Local only | Local only | Never |
| Default | Local only on query | Local only on query | Only on `Resolve` (install/pull) |

## Decision

Make caller intent explicit at every `IndexImpl` call site by introducing an `IndexOperation::{Query, Resolve}` enum, and split the local-index write surface into two narrowly-scoped functions.

### 1. `IndexOperation` enum on existing trait signatures

```rust
#[non_exhaustive]
pub enum IndexOperation {
    Query,    // pure read; never walks source chain on miss
    Resolve,  // install / pull; walks chain + persists on miss
}
```

Threaded through `IndexImpl::fetch_manifest{,_digest}`, `Index::select`, `Index::fetch_candidates`. `list_tags` / `list_repositories` are query-only by definition and do **not** take `op`.

The enum is preferred over a trait split (read-only `IndexImpl` + write-only `IndexWriter`) because it forces the call site to declare intent every single time without restructuring the trait hierarchy. A reviewer scanning a diff sees `Op::Query` / `Op::Resolve` literally next to the call.

Naming: `Resolve` (not `Persist`) describes caller intent — "resolve this identifier for use" — rather than the side effect, because not every `Resolve` actually persists (`--offline` resolves without a fetch; digest-only Resolve writes blobs without committing a tag pointer).

### 2. Split `LocalIndex` write surface

`LocalIndex::write_chain_and_commit_tag` → two narrower functions:

- `persist_manifest_chain(source, id) -> Result<Option<Digest>>` — `pub`. Content-addressed write of the manifest chain (image index + per-platform manifests). Returns the head digest so callers don't need a follow-up round-trip. Used by both tag- and digest-addressed pulls.
- `commit_tag(id, digest) -> Result<()>` — `pub(super)`. The single tag-pointer writer outside `refresh_tags`. Visibility narrowed so `ChainedIndex::fetch_and_persist_chain` is the sole caller.

`fetch_and_persist_chain` composes: persist unconditionally; commit the tag pointer only when `identifier.tag().is_some() && identifier.digest().is_none()`. The `tag+digest` skip is the post-pin contract change — pinned-id pulls leave `ocx.lock` as the canonical record.

### 3. Structural invariant test

`chained_index::chain_refs_tests::op_query_never_writes_local_index_in_any_mode`
asserts the real invariant this split protects: a `Query` never **writes** the
local index in any `ChainMode`. In `Default` / `Offline` a tag-addressed cache
miss returns `None` without invoking the source; in `Remote` the query reads
*through* to the source (returning `Some`) but the tag store stays untouched. A
spy with a call counter catches regressions through any future write path.

> **Amendment 2026-06-02.** The original test
> (`op_query_never_walks_source_in_any_mode`) over-constrained the invariant: it
> asserted `None` + zero source calls in **every** mode, including `--remote`.
> That contradicted both the user mental model (`--remote` = "source only" for
> mutable lookups) and the routing matrix below (which already lists
> `fetch_manifest` tag+`Op::Query` under `--remote` as "source only, no write",
> in the same row as `list_tags`). The defect was user-visible: `ocx index list
> --platforms --remote` always returned an empty platform set because
> `ChainedIndex::fetch_manifest` short-circuited every `Query` to `None`, so the
> website catalog generator could never populate platform data. The protected
> invariant was always *no query-path **write***, not *no query-path source
> read* — Remote-mode `list_tags` already read through to the source without
> writing. `ChainedIndex::{fetch_manifest,fetch_manifest_digest}` now do the same
> for tag-addressed `Query` via `query_sources_manifest{,_digest}` (read-through,
> no persist), and the test was renamed and split to match.

### 4. `--offline --remote` accepted as pinned-only mode

The combination is not rejected at clap parse time. Both flags set means: no source contact (`--offline` wins), pure-query lookups still local-only, any tag-addressed `Resolve` that cannot be satisfied locally errors instead of silently falling back. An `info!` log fires at `Context::try_init` when both are set, documenting the semantics. Useful in CI to assert every project dependency is digest-pinned.

### 5. `index list` rejects digest-bearing identifiers

`ocx index list <pkg>@<digest>` exits non-zero with a usage error pointing users to `ocx package info`. `index list` enumerates tags; a digest narrows nothing. Tag-only identifiers (`<pkg>:<tag>`) still filter the returned list.

## Consequences

### Behaviour changes (contract breaks)

- **Pure-query `index list --platforms` no longer fills the local tag store.** With `--remote` it reads the manifest through to the source live (no write); without `--remote` it reads the persisted local index only, so use `ocx index update` (or a prior install/pull) to populate it for offline/default use. The catalog generator workaround `unset OCX_INDEX` is now dead code. *(Amended 2026-06-02 — see Decision #3 amendment: the `--remote` read-through was the documented behaviour all along but was not implemented; it is now.)*
- **Pinned-id pull (`tag+digest`) no longer commits a tag pointer.** `ocx.lock` is the canonical record. The local tag store is optional once a project is locked. Existing tag pointers are preserved (no destructive migration).
- **`ocx index list <pkg>@<digest>` is now a usage error.** Migration: drop the `@digest` suffix, or use `ocx package info`.

### Carry-overs

- The `ChainMode` enum is unchanged; `Default` / `Remote` / `Offline` continue to gate routing.
- `LocalIndex::refresh_tags` is unchanged; `ocx index update` still owns the explicit refresh path.
- Singleflight deduplication of concurrent cache misses is unchanged.

### Routing matrix (post-Phase-11)

| Operation | `--remote` | `--offline` | `--offline --remote` | Default |
|-----------|-----------|-------------|----------------------|---------|
| `list_repositories`, `list_tags`, `fetch_manifest` tag+`Op::Query` | source only, no write | local only | local only (info log) | local only |
| `fetch_manifest` tag+`Op::Resolve` | source only, write blobs+tag | local only (errors if missing) | local only (errors) | local first, miss → fetch+write |
| `fetch_manifest` digest, any op | local first | local only | local only | local first |
| `fetch_manifest` digest+`Op::Resolve` (pinned-id pull) | source on miss, write blobs only, **no tag** | local only | local only | local first, miss → fetch blobs only |

## Alternatives Considered

### Trait split (read-only `IndexReader` + write `IndexWriter`)

Cleaner type-safety story than the enum (the writer simply isn't visible to query callers). Rejected: the caller-site clarity benefit of the enum — `Op::Query` literally written next to every call — survives diff review better than a trait choice that lives only in the type signature. Splitting also forces every IndexImpl impl into two trait impls, which compounds the boilerplate cost across the four impl sites.

### Bool flag instead of enum

`fetch_manifest(id, persist: bool)`. Rejected: `bool` parameters are a documented Warn-tier anti-pattern in `quality-core.md`. The enum gives a meaningful name at every call site; the bool would be ambiguous (`true` = persist? walk? both?).

### Strict pinned-only error mode (out of scope)

`--offline --remote` could surface a typed `Error::PinnedOnlyTagResolutionAttempted(identifier)` when a tag-addressed `Resolve` lookup cannot be satisfied locally, instead of returning `Ok(None)`. Tracked as a follow-up; not implemented in Phase 11.

## References

- Working note: `.claude/artifacts/feedback_index_routing_semantics.md` (superseded by this ADR)
- Plan: `.claude/state/plans/plan_project_toolchain.md` Phase 11
- Subsystem rule: `.claude/rules/subsystem-oci.md` (`IndexOperation × ChainMode` table)
- CLI rule: `.claude/rules/subsystem-cli.md` (routing intent at command level)
- User docs: `website/src/docs/user-guide.md` (Routing, Pinned-only mode)
- User docs: `website/src/docs/reference/command-line.md` (`--remote`, `--offline`, `index list` rejection)
