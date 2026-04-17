---
paths:
  - crates/ocx_lib/src/oci/**
  - external/rust-oci-client/**
---

# OCI Subsystem

OCI registry client, index management, identifiers, and platform matching at `crates/ocx_lib/src/oci/`.

## Design Rationale

Trait-based dispatch (`IndexImpl`) enables swapping local/remote index implementations and injecting test transports without changing callers. `RemoteIndex` caches aggressively (RwLock per clone) to avoid redundant registry calls in batch operations. `IndexImpl` methods return `Option` (None = not found) because absence is a normal query result at the index layer, not an error. See `arch-principles.md` for the full pattern catalog.

## Module Map

| Path | Purpose |
|------|---------|
| `oci.rs` | Root module; re-exports public types |
| `oci/index.rs` | Public `Index` wrapper; `ChainMode` enum; `SelectResult` enum; `fetch_candidates()`, `select()` |
| `oci/index/index_impl.rs` | Private `IndexImpl` async trait (4 core methods) |
| `oci/index/chained_index.rs` | `ChainedIndex`: cache + ordered sources + `ChainMode` routing |
| `oci/index/local_index.rs` | `LocalIndex`: file-backed snapshot; high-level entry points `refresh_tags`, `write_chain_and_commit_tag` |
| `oci/index/local_index/cache.rs` | In-memory shared cache (tags + manifests) |
| `oci/index/local_index/tag_manager.rs` | Tag read/write helpers used by `LocalIndex` |
| `oci/index/remote_index.rs` | `RemoteIndex`: wraps `Client`, in-memory cache only |
| `oci/index/remote_index/cache.rs` | In-memory shared cache (repositories, tags, digests) |
| `oci/index/snapshot.rs` | `Snapshot` struct: tag → [(digest, platform)] (orphan, not yet wired as IndexImpl) |
| `oci/identifier.rs` | `Identifier` struct: parsed OCI reference with validation |
| `oci/digest.rs` | `Digest` enum: Sha256, Sha384, Sha512 |
| `oci/platform.rs` | `Platform` struct: os/arch matching, `any()` for platform-agnostic packages |
| `oci/client.rs` | `Client`: registry operations (list, fetch, push, pull) |
| `oci/client/builder.rs` | `ClientBuilder`: configures transport, auth, chunk sizes |
| `oci/client/transport.rs` | `OciTransport` async trait (abstract HTTP transport) |
| `oci/client/native_transport.rs` | Native transport using `oci_client` library |
| `oci/client/test_transport.rs` | Mock transport for unit tests |
| `oci/manifest.rs` | `has_platform()` utility |
| `oci/annotations.rs` | OCI annotation keys + OCX-specific `KEYWORDS` |

## Key Types

### ChainMode

```rust
#[non_exhaustive]
pub enum ChainMode {
    Default,  // Cache-first; write-through on source fetch. Normal online operation.
    Remote,   // Tag/catalog bypass cache, go straight to source. Immutable (digest) lookups still cache. Used for `--remote`.
    Offline,  // Cache only; source never consulted; cache miss returns None. Used for `--offline`.
}
```

| Mode | Tag/catalog lookup | Blob/manifest (digest-addressed) | `$OCX_HOME/tags/` updated? |
|------|-------------------|----------------------------------|---------------------------|
| `Default` | Local cache first, then source | Cache + write-through | Yes |
| `Remote` | Source always (bypass cache) | Cache + write-through | No |
| `Offline` | Cache only | Cache only | No |

### Identifier

Parsed OCI reference: `registry/repository[:tag][@digest]`.

- `parse_with_default_registry(s, default)` — main entry point
- `tag()` returns `Option<&str>` — does NOT inject "latest" (unlike `oci_spec::Reference`)
- `tag_or_latest()` — returns tag or "latest" as fallback
- `clone_with_tag(tag)` — new identifier with tag, drops digest (tag change invalidates digest)
- Tags with `+` normalized to `_` on parse (OCI spec forbids `+`)
- Repository must be lowercase (validated on parse)

### Index (public wrapper)

Type-erased wrapper over `Box<dyn IndexImpl>`. Construction:
- `from_chained(cache: LocalIndex, sources: Vec<Index>, mode: ChainMode)` — the standard constructor; wraps a `ChainedIndex` that orchestrates cache + source routing per `ChainMode`
- `from_remote(remote_index)` — wraps a bare `RemoteIndex` (no caching)
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
async fn fetch_manifest(&self, id: &Identifier) -> Result<Option<(Digest, Manifest)>>;
async fn fetch_manifest_digest(&self, id: &Identifier) -> Result<Option<Digest>>;
```

**Return convention**: `Result<Option<T>>` — `None` = not found (not an error), `Err` = network/IO failure.

### LocalIndex

File-backed snapshot of registry metadata. High-level public entry points:

- `refresh_tags(source, identifier)` — fetch tags from `source`, persist to `$OCX_HOME/tags/`; used by `ChainedIndex` for tag/catalog operations
- `write_chain_and_commit_tag(source, identifier)` — orchestrate a full chain walk (image index → manifest), persist all blobs to `$OCX_HOME/blobs/`, then commit the tag pointer; called by `ChainedIndex` after a source fetch

Internal helpers `persist_manifest_chain` and `commit_tag` are private — callers always go through these two high-level methods.

### LocalIndex vs RemoteIndex

| Aspect | LocalIndex | RemoteIndex |
|--------|-----------|-------------|
| Storage | Disk JSON + in-memory cache | In-memory cache only |
| Population | Via `ChainedIndex` write-through | Lazy on access |
| Manifest cache | Yes (disk + memory) | No (re-fetches each time) |
| Offline support | Yes | No |
| Clone behavior | Shares in-memory cache | Shares in-memory cache |

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

1. **Cache never invalidated** — both index types cache aggressively in memory. For fresh data, create new instance or call `update()`.
2. **Internal tags filtered** — tags prefixed `__ocx.` are stripped by every `IndexImpl::list_tags()` automatically.
3. **Digest overrides tag** — when identifier has both, `fetch_manifest()` uses digest directly.
4. **Auth at Client level** — index implementations don't handle auth; `Client::ensure_auth()` is called before operations.

## Gotchas

- **OCI tags are mutable.** Never assume a tag is "frozen" or "pinned." Only digests are immutable.
- **Cache coherence issue**: Some commands call `context.remote_client()` directly instead of going through `default_index`. This bypasses cache and can produce inconsistent results. All index operations should route through `default_index`.
- **`oci-client` flush audit**: `pull_blob` was missing `out.flush().await?` causing truncated files. Fixed in `pull_blob`, but audit other `AsyncWrite` methods.
- **Submodule at `external/rust-oci-client/`** is a patched fork. Changes need upstream PRs. Only format new code (upstream uses 100-char rustfmt).
- **When unsure about the current `oci-client` API**, query Context7 MCP (`mcp__context7__resolve-library-id` → `mcp__context7__get-library-docs`) before guessing. The upstream crate evolves independently of our patched fork, and training-data knowledge of its API shape decays quickly.

## Quality Gate

During review-fix loops, run `task rust:verify` — not full `task verify`.
Full `task verify` is the final gate before commit.
