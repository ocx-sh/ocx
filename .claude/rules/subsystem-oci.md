---
paths:
  - crates/ocx_lib/src/oci/**
  - external/rust-oci-client/**
---

# OCI Subsystem

OCI registry client, index management, identifiers, platform matching at `crates/ocx_lib/src/oci/`.

## Design Rationale

Trait dispatch (`IndexImpl`) swap local/remote index impls + inject test transports without changing callers. `RemoteIndex` cache aggressive (RwLock per clone) avoid redundant registry calls in batch ops. `IndexImpl` methods return `Option` (None = not found) — absence normal query result at index layer, not error. See `arch-principles.md` for full pattern catalog.

## Module Map

| Path | Purpose |
|------|---------|
| `oci/index.rs` | Public `Index` wrapper; `ChainMode`; `SelectResult`; `fetch_candidates()`, `select()` |
| `oci/index/index_impl.rs` | Private `IndexImpl` async trait (4 core methods) |
| `oci/index/chained_index.rs` | `ChainedIndex`: cache + ordered sources + `ChainMode` routing |
| `oci/index/local_index.rs` | `LocalIndex`: file-backed snapshot; `refresh_tags`, `persist_manifest_chain` |
| `oci/index/remote_index.rs` | `RemoteIndex`: wraps `Client`, in-memory cache only |
| `oci/identifier.rs` | `Identifier`: parsed OCI reference with validation |
| `oci/digest.rs` | `Digest` enum: Sha256, Sha384, Sha512 |
| `oci/platform.rs` | `Platform`: os/arch matching, `any()` for platform-agnostic packages |
| `oci/client.rs` | `Client`: registry operations (list, fetch, push, pull) |
| `oci/client/transport.rs` | `OciTransport` async trait (abstract HTTP transport) |
| `oci/client/native_transport.rs` | Native transport using `oci_client` library |
| `oci/client/hashing_reader.rs` | `HashingAsyncReader`: digest tee over sha256/sha384/sha512 |
| `oci/client/progress_reader.rs` | `ProgressReader`: cumulative download progress callback |

## Key Types

### ChainMode

```rust
pub enum ChainMode {
    Default,  // Local index first for queries. `Op::Resolve` walks chain on miss; `Op::Query` returns None. Normal online operation.
    Remote,   // Mutable lookups (tag list, catalog, tag-addressed manifest) hit source directly. Digest-addressed lookups still consult local index. Used for `--remote`.
    Offline,  // Local index only; source never consulted. Digest miss → None; unpinned-tag `Op::Resolve` miss → PolicyResolutionBlocked (exit 81). Used for `--offline`.
    Frozen,   // Freeze resolution to the local index: unpinned-tag `Op::Resolve` miss → PolicyResolutionBlocked (exit 81); digest-addressed content still walks the source like Default. Used for `--frozen`.
}
```

`ChainMode::policy_label(self) -> &'static str` returns the lowercase flag name (`"offline"` / `"frozen"`) embedded in the `PolicyResolutionBlocked` message.

**Deferred: composed RoutingPolicy.** A struct form (`RoutingPolicy { resolution: Allowed|LocalOnly, network: Allowed|Banned }`) was considered and deferred in favor of the flat `ChainMode` enum — YAGNI: only one policy axis exists today, and four variants enumerate the space exactly. Revisit trigger: when a *second orthogonal* policy flag appears, compose instead of adding a fifth flattened variant (combinatorial growth is the signal).

### IndexOperation

`IndexImpl::fetch_manifest{,_digest}` (and the `Index` wrapper's `select` / `fetch_candidates`) take an `IndexOperation` argument that declares caller intent:

```rust
#[non_exhaustive]
pub enum IndexOperation {
    Query,    // pure read; ChainedIndex returns None on miss, never walks the chain
    Resolve,  // install / pull; ChainedIndex walks the chain + persists on miss
}
```

The enum exists because the trait used to conflate query and update — a cache miss in `ChainedIndex::fetch_manifest` would silently walk the source chain and persist the result, leaking writes through query paths. Making intent explicit at every call site rules out that class of bug. See `adr_index_routing_semantics.md`.

### Routing matrix

| Operation | `--remote` | `--offline` | `--frozen` | `--offline --remote` | Default |
|-----------|-----------|-------------|------------|----------------------|---------|
| `list_repositories`, `list_tags`, `fetch_manifest` tag+`Op::Query` | source only, no write | local only | local only | local only (info log) | local only |
| `fetch_manifest` tag+`Op::Resolve` | source only, write blobs+tag | local only; unpinned miss → **PolicyBlocked (81)** | local only; unpinned miss → **PolicyBlocked (81)** | local only (→ 81) | local first, miss → fetch+write |
| `fetch_manifest` digest, any op | local first | local only | local first | local only | local first |
| `fetch_manifest` digest+`Op::Resolve` (pinned-id pull) | source on miss, write blobs only, **no tag** | local only | source on miss, write blobs only, **no tag** | local only | local first, miss → fetch blobs only |

**No-resolve policy block (offline + frozen).** Both `Offline` and `Frozen` refuse to resolve an unpinned (tag-only) reference from a source. The shared gate at the top of `ChainedIndex::walk_chain` is an exhaustive `match self.mode` — the `Offline | Frozen` arm with an `identifier.digest().is_none()` guard raises `oci::index::error::Error::PolicyResolutionBlocked { identifier, policy }` → `ExitCode::PolicyBlocked` (81); adding a new `ChainMode` variant forces a compile error at this routing decision. This is a deliberate behaviour change for offline: an unpinned-tag `Op::Resolve` miss now surfaces as `PolicyBlocked` (81), not `TagNotFound` (79) — realizing offline's documented "errors if missing" contract and aligning it with frozen. `TagNotFound` (79) now means strictly "a source *was* consulted and the tag genuinely does not exist" (Default / Remote). The two policies still differ on the digest axis: offline blocks the pinned digest's *content* fetch, frozen lets it through (only unpinned-tag *resolution* is refused). The project-lock layer mirrors this with `ProjectErrorKind::PolicyBlocked` (terminal, no retry).

**Design note — write paths.** Local index mutation is owned by exactly three entry points: `LocalIndex::refresh_tags` (called from `ocx index update`), `LocalIndex::persist_manifest_chain` (content-addressed blob writes from install/pull), and `LocalIndex::commit_tag` (`pub(super)`, the only tag-pointer writer outside `refresh_tags`; called only from `ChainedIndex::fetch_and_persist_chain`). Pure query paths must never reach any of them. The structural test `chain_refs_tests::op_query_never_writes_local_index_in_any_mode` enforces this for `Op::Query` (Default/Offline → `None`, no source; `--remote` → read-through to source via `query_sources_manifest{,_digest}`, returns `Some`, tag store untouched). Pinned-id pulls (`tag+digest`) skip the `commit_tag` step because `ocx.lock` is canonical.

### Identifier

Parsed OCI reference: `registry/repository[:tag][@digest]`.

- `parse_with_default_registry(s, default)` — main entry point
- `tag()` returns `Option<&str>` — does NOT inject "latest" (unlike `oci_spec::Reference`)
- `tag_or_latest()` — returns tag or "latest" fallback
- `clone_with_tag(tag)` — new identifier with tag, drops digest (tag change invalidates digest)
- Tags with `+` normalized to `_` on parse (OCI spec forbids `+`)
- Repository must be lowercase (validated on parse)

### Index (public wrapper)

Type-erased wrapper over `Box<dyn IndexImpl>`. Construction:
- `from_chained(cache: LocalIndex, sources: Vec<Index>, mode: ChainMode)` — standard constructor; wraps `ChainedIndex` orchestrating cache + source routing per `ChainMode`
- `from_remote(remote_index)` — wraps bare `RemoteIndex` (no caching)
- Clone shares in-memory cache (via `Arc<RwLock>`)

Key methods: `list_tags()`, `fetch_manifest()`, `fetch_candidates()`, `select(identifier, platforms) → SelectResult`

### SelectResult

```rust
#[non_exhaustive]
pub enum SelectResult {
    Found(Identifier),           // Exactly one match
    Ambiguous(Vec<Identifier>),  // Multiple matches
    NotFound,                    // No candidates
}
```

### IndexImpl Trait (private)

```rust
async fn list_repositories(&self, registry: &str) -> Result<Vec<String>>;
async fn list_tags(&self, id: &Identifier) -> Result<Option<Vec<String>>>;
async fn fetch_manifest(&self, id: &Identifier, op: IndexOperation) -> Result<Option<(Digest, Manifest)>>;
async fn fetch_manifest_digest(&self, id: &Identifier, op: IndexOperation) -> Result<Option<Digest>>;
```

`list_tags` / `list_repositories` are query-only by definition and do **not** take `op`. `fetch_manifest{,_digest}` callers must pass `Op::Query` for pure reads or `Op::Resolve` for install/pull paths.

**Return convention**: `Result<Option<T>>` — `None` = not found (not error), `Err` = network/IO failure.

### LocalIndex

File-backed snapshot of registry metadata. Three public entry points, each narrowly scoped:

- `refresh_tags(source, identifier)` — explicit refresh path; only `ocx index update` calls it.
- `persist_manifest_chain(source, identifier)` — content-addressed write of the manifest chain (image index + per-platform manifests). Returns the head digest. Used by both tag- and digest-addressed pulls.
- `commit_tag(identifier, digest)` — `pub(super)`. The single tag-pointer writer outside `refresh_tags`. Visibility narrowed so `ChainedIndex::fetch_and_persist_chain` is the sole caller; pinned-id pulls (`tag+digest`) skip it because `ocx.lock` is canonical.

**Write-through semantics** (via `ChainedIndex`):
- Tagged identifier (`cmake:3.28`): fetches only that tag; preserves other tags locally
- Bare identifier (`cmake`): fetches all tags; does not remove local-only tags
- Always merges, never overwrites; safe for parallel updates to different tags

### Platform

- `Platform::any()` — platform-agnostic packages (Java, text tools)
- `Platform::current()` — auto-detect OS/arch
- `platform.matches(other)` — currently strict equality (no fuzzy matching)
- Supported: linux/amd64, linux/arm64, darwin/amd64, darwin/arm64, windows/amd64

### Manifest Types

- `Manifest::Image` — single platform; `fetch_candidates()` returns one entry with `Platform::any()`
- `Manifest::ImageIndex` — multi-platform; one entry per child manifest with platform annotation

## Invariants

1. **Cache never invalidated** — both index types cache aggressive in memory. For fresh data, create new instance or call `update()`.
2. **Internal tags filtered** — tags prefixed `__ocx.` stripped by every `IndexImpl::list_tags()` auto.
3. **Digest overrides tag** — when identifier has both, `fetch_manifest()` uses digest direct.
4. **Auth at Client level** — index impls don't handle auth; `Client::ensure_auth()` called before operations.

## Pull Path (streaming single-pass pipeline) {#pull-path}

`Client::pull_layer` assembles a single-pass pipeline per layer:

```
transport.pull_blob_streaming → .take(layer.size) → HashingAsyncReader(algo)
  → ProgressReader → XzDecoder/GzDecoder → SyncIoBridge → tar::Archive::unpack()
```

After stream end, `HashingAsyncReader::finalize()` compares the computed digest against
the descriptor digest **before** returning any extraction error. Wrong bytes (CWE-345)
cause a tar format error, but the digest mismatch is surfaced first (`DigestMismatch`,
not `Internal`) — retrying usually heals transient corruption.

`NativeTransport::pull_blob_streaming` calls the fork's public `pull_blob_stream`, which
wraps the response in `VerifyingStream` (mismatch → `io::Error(DigestError::VerificationError)`
at stream end). `HashingAsyncReader` is canonical and covers all paths including
`StubTransport`; `VerifyingStream` is secondary.

**Decompression-bomb caps (CWE-400):**

| Cap | Limit | Applied to |
|----|-------|-----------|
| Compressed | `layer.size` bytes via `.take()` | Raw stream, before `HashingAsyncReader` |
| Decompressed | `max(1 GiB, 100 × layer.size)` | `SyncIoBridge` output inside `spawn_blocking` |

Exceeding either cap terminates the stream; the digest check fires as usual.

No blob file is written to disk during pull — there is no `DropFile` guard to drop.

**`SyncIoBridge` occupancy:** `spawn_blocking` thread is held for the full
download + extract duration (previously extract only). At 10 Mbps × 200 MB ≈ 160 s.
Tokio blocking pool cap is 512. Deferred: add semaphore if install parallelism grows
unbounded. Note: `SyncIoBridge` uses `Handle::block_on` per read (not `block_in_place`);
creating it inside the closure is idiomatic (tokio issue #6795).

## Gotchas {#gotchas}

- **OCI tags mutable.** Never assume tag "frozen" or "pinned." Only digests immutable.
- **Cache coherence issue**: Some commands call `context.remote_client()` directly instead of going through `default_index`. Bypasses cache, produces inconsistent results. All index ops should route through `default_index`.
- **Submodule at `external/rust-oci-client/`** patched fork. Changes need upstream PRs. Only format new code (upstream uses 100-char rustfmt).
- **When unsure about current `oci-client` API**, query Context7 MCP (`mcp__context7__resolve-library-id` → `mcp__context7__get-library-docs`) before guessing. Upstream crate evolves independently of patched fork; training-data knowledge of API shape decays fast.

## Quality Gate

During review-fix loops, run `task rust:verify` — not full `task verify`.
Full `task verify` is final gate before commit.