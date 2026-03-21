---
paths:
  - crates/ocx_lib/src/oci/**
  - external/rust-oci-client/**
---

# OCI Subsystem

OCI registry client, index management, identifiers, and platform matching at `crates/ocx_lib/src/oci/`.

## Design Rationale

Trait-based dispatch (`IndexImpl`) enables swapping local/remote index implementations and injecting test transports without changing callers. `RemoteIndex` caches aggressively (RwLock per clone) to avoid redundant registry calls in batch operations. `IndexImpl` methods return `Option` (None = not found) because absence is a normal query result at the index layer, not an error. See `architecture-principles.md` for the full pattern catalog.

## Module Map

| Path | Purpose |
|------|---------|
| `oci.rs` | Root module; re-exports public types |
| `oci/index.rs` | Public `Index` wrapper; `SelectResult` enum; `fetch_candidates()`, `select()` |
| `oci/index/index_impl.rs` | Private `IndexImpl` async trait (4 core methods) |
| `oci/index/local_index.rs` | `LocalIndex`: file-backed snapshot of registry metadata |
| `oci/index/local_index/cache.rs` | In-memory shared cache (tags + manifests) |
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
- `from_local(local_index)` or `from_remote(remote_index)`
- Clone shares in-memory cache (via `Arc<RwLock>`)

Key methods: `list_tags()`, `fetch_manifest()`, `fetch_candidates()`, `select(identifier, platforms) → SelectResult`

### SelectResult

```rust
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

### LocalIndex vs RemoteIndex

| Aspect | LocalIndex | RemoteIndex |
|--------|-----------|-------------|
| Storage | Disk JSON + in-memory cache | In-memory cache only |
| Population | Explicit `update()` call | Lazy on access |
| Manifest cache | Yes (disk + memory) | No (re-fetches each time) |
| Offline support | Yes | No |
| Clone behavior | Shares in-memory cache | Shares in-memory cache |

**LocalIndex update semantics:**
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
