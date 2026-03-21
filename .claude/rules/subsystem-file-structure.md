---
paths:
  - crates/ocx_lib/src/file_structure/**
  - crates/ocx_lib/src/file_structure.rs
  - crates/ocx_lib/src/reference_manager.rs
  - crates/ocx_lib/src/symlink.rs
---

# File Structure Subsystem

## Design Rationale

Content-addressed storage (by SHA-256 digest) provides Nix-like dedup and immutable paths without Nix-like complexity. Three separate stores (objects, index, installs) enforce separation of concerns — immutable content, cached metadata, and mutable symlinks are independent lifecycles. Back-references (`refs/`) enable lock-free, concurrent-safe GC: `ocx clean` only removes objects with empty `refs/`, no graph traversal needed. See `architecture-principles.md` for the full pattern catalog.

Content-addressed local storage layout, symlink management, and reference tracking at `crates/ocx_lib/src/file_structure/`.

## Module Map

| File | Purpose | Key Types |
|------|---------|-----------|
| `file_structure.rs` | Composite root; `slugify()`, `repository_path()` | `FileStructure` |
| `object_store.rs` | Content-addressed binary storage | `ObjectStore`, `ObjectDir` |
| `index_store.rs` | Local OCI index (tags, manifests) | `IndexStore` |
| `install_store.rs` | Install symlinks (candidate/current) | `InstallStore`, `SymlinkKind` |
| `temp_store.rs` | Temp dirs for in-progress downloads | `TempStore`, `TempDir`, `TempAcquireResult` |
| `reference_manager.rs` | Forward symlinks + back-references for GC | `ReferenceManager` |
| `symlink.rs` | Low-level symlink primitives | `create()`, `update()`, `remove()` |

## FileStructure (composite root)

```rust
pub struct FileStructure {
    pub objects: ObjectStore,
    pub index: IndexStore,
    pub installs: InstallStore,
    pub temp: TempStore,
}
```

One instance per session. Sub-stores accessible as public fields.

## Four Stores

### ObjectStore — Content-addressed binaries

Layout: `{root}/{registry_slug}/{repo_path}/{algorithm}/{shard1_8hex}/{shard2_8hex}/{shard3_16hex}/`

Each object directory contains: `content/` (extracted files), `metadata.json`, `refs/` (back-references).

Key methods: `path()`, `content()`, `metadata()`, `refs_dir_for_content()`, `list_all() → Vec<ObjectDir>`.

**All paths require a digest-carrying identifier.** `list_all()` stops recursion at `content/` directories.

### IndexStore — Local metadata mirror

Layout: `{root}/{registry_slug}/tags/{repo_path}.json` + `objects/{algorithm}/{sharded_digest}.json`

Key methods: `tags()`, `manifest()`, `blob()`, `list_repositories()`.

### InstallStore — Symlinks

Layout: `{root}/{registry_slug}/{repo_path}/candidates/{tag}` + `current`

```rust
pub enum SymlinkKind { Candidate, Current }
```

Key methods: `current()`, `candidate()`, `candidates()`, `symlink(kind)`.

`candidate()` uses `identifier.tag_or_latest()` (falls back to "latest" if no tag).

### TempStore — Download staging

Layout: `{root}/{32_hex_hash}/` — deterministic hash of `"{registry}\0{repo}\0{digest}"`.

Each temp dir has `install.lock` for exclusive access. `try_acquire()` is non-blocking; `acquire_with_timeout()` blocks. Stale artifacts cleaned on successful acquire.

## Path Construction

### Slugification

All OCI components are slugified via `to_relaxed_slug()`: preserves `[a-zA-Z0-9._-]`, replaces everything else with `_`. Example: `localhost:5000` → `localhost_5000`.

### Repository Path Splitting

**Never use `.join("org/project/tool")`** — this embeds literal `/` causing mixed separators on Windows. Use `repository_path()` which splits on `/`:

```rust
pub fn repository_path(repository: &str) -> PathBuf {
    repository.split('/').collect()
}
```

### Digest Sharding

All digest types: `{algorithm}/{hex[0..8]}/{hex[8..16]}/{hex[16..32]}` (3 levels).

## ReferenceManager

Manages forward symlinks + back-references for safe garbage collection. **Always use this for install symlinks, never raw `symlink::update/create`.**

```rust
pub fn link(&self, forward_path: &Path, content_path: &Path) -> Result<()>
pub fn unlink(&self, forward_path: &Path) -> Result<()>
pub fn broken_refs(&self) -> Result<Vec<PathBuf>>
```

**Arg order for `link()` is `(link, target)` — opposite of `symlink::update(target, link)`.** This is a common source of confusion.

Back-reference: `objects/.../refs/{16_hex_hash}` → forward symlink path. Hash: first 16 hex chars of SHA256(forward_path bytes).

**Idempotent**: `link()` is no-op if forward already points to content. `unlink()` is no-op if forward doesn't exist.

## GC Safety

An object is safe to delete when its `refs/` directory is empty (no forward symlinks point to it). This is lock-free because:
- Each back-ref has a unique name (hash of forward path)
- Symlink create/delete are atomic POSIX operations
- No global coordination needed

## symlink Module

Low-level primitives. Use only for non-package symlinks:
- `create(target, link)` — create symlink + parent dirs
- `update(target, link)` — create or replace
- `remove(link)` — remove; no-op if absent

Windows: uses NTFS junction points (no privilege escalation needed).
