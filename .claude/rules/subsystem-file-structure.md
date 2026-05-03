---
paths:
  - crates/ocx_lib/src/file_structure/**
  - crates/ocx_lib/src/file_structure.rs
  - crates/ocx_lib/src/reference_manager.rs
  - crates/ocx_lib/src/symlink.rs
---

# File Structure Subsystem

## Design Rationale

Old `objects/` tree mix three concerns: raw OCI blobs, extracted layer files, assembled packages. Mix break cross-repo dedup (repo in CAS path), block shared layers (one layer per package), no spot for non-extracted blobs (signatures, SBOMs). Three-tier split fix each problem, clear lifecycle per tier:

- **`blobs/`** — raw OCI content (manifests, compressed layer archives); key by registry + digest; never extracted
- **`layers/`** — extracted layer trees shared across packages; key by registry + digest; dedup unit for multi-layer packages
- **`packages/`** — assembled packages; key by registry + digest only (no repo in path → cross-repo dedup); hardlinked content from one or more layers

Split `refs/` into four named subdirs (`symlinks/`, `deps/`, `layers/`, `blobs/`) → GC do single BFS pass over all three tiers. Only packages can be roots or have outgoing edges; layers and blobs reachable only via package `refs/layers/` and `refs/blobs/` links.

## Module Map

| File | Purpose | Key Types |
|------|---------|-----------|
| `file_structure.rs` | Composite root; `slugify()`, `repository_path()` | `FileStructure` |
| `blob_store.rs` | Raw OCI blob storage | `BlobStore`, `BlobDir` |
| `blob_store/blob_guard.rs` | RAII read/write lock on individual blob data files | `BlobGuard` |
| `layer_store.rs` | Extracted layer storage | `LayerStore`, `LayerDir` |
| `package_store.rs` | Assembled package storage | `PackageStore`, `PackageDir` |
| `tag_store.rs` | Local tag→digest index | `TagStore` |
| `symlink_store.rs` | Install symlinks (candidate/current) | `SymlinkStore`, `SymlinkKind` |
| `temp_store.rs` | Temp dirs for in-progress downloads | `TempStore`, `TempDir`, `TempAcquireResult`, `StaleEntry`, `TempEntry` |
| `cas_path.rs` | Digest sharding; `CasTier` enum | `cas_shard_path()`, `is_valid_cas_path()`, `write_digest_file()` |
| `reference_manager.rs` | Forward symlinks + back-references for GC | `ReferenceManager` |

### Cross-cutting link primitives

These modules sit at `crates/ocx_lib/src/` root — consumed across subsystems.

| Module | Purpose | Used by |
|--------|---------|---------|
| `symlink.rs` | Symlink create/update/remove/is_link; Windows junction aware | ReferenceManager, pull, assemble walker, archive extractor |
| `hardlink.rs` | Hardlink create/update — THE ONE place `std::fs::hard_link` lives | assemble walker, codesign |
| `utility/fs/path.rs` | Lexical path helpers: `lexical_normalize`, `escapes_root`, `validate_symlinks_in_dir` | symlink, archive extractor, assemble walker |

## FileStructure (composite root)

```rust
pub struct FileStructure {
    pub blobs: BlobStore,
    pub layers: LayerStore,
    pub packages: PackageStore,
    pub tags: TagStore,
    pub symlinks: SymlinkStore,
    pub temp: TempStore,
}
```

One instance per session. Sub-stores public fields. `root()` return OCX home path.

**Root-level state files under `$OCX_HOME`:**

| File | Purpose |
|------|---------|
| `projects.json` | Project registry — list of `ocx.lock` paths registered on this machine. Updated by `ProjectRegistry::register` after every `ocx lock` save. Read by `ocx clean` to retain cross-project packages. |
| `.projects.lock` | Advisory lock sentinel for `projects.json`. Same-volume sibling pattern as `.ocx-lock`; never contended long — held only during register/rewrite. |

## Six Stores

### BlobStore — Raw OCI blobs

Layout: `{root}/blobs/{registry_slug}/{algorithm}/{2hex}/{30hex}/data`

Each blob dir contain: `data` (raw blob bytes), `digest` (full digest string for recovery).

Key methods: `path(registry, digest)`, `data(registry, digest)`, `digest_file(registry, digest)`, `list_all() → Vec<BlobDir>`.

`BlobDir` accessors: `data()`, `digest_file()`.

### LayerStore — Extracted layers

Layout: `{root}/layers/{registry_slug}/{algorithm}/{2hex}/{30hex}/content/`

Each layer dir contain: `content/` (extracted files), `digest`.

Key methods: `path(registry, digest)`, `content(registry, digest)`, `digest_file(registry, digest)`, `list_all() → Vec<LayerDir>`.

`LayerDir` accessors: `content()`, `digest_file()`.

### PackageStore — Assembled packages

Layout: `{root}/packages/{registry_slug}/{algorithm}/{2hex}/{30hex}/`

**Repository NOT in path.** Only registry + digest set location → cross-repo dedup.

Each package dir contain: `content/`, `metadata.json`, `manifest.json`, `resolve.json`, `install.json`, `digest`, `refs/symlinks/`, `refs/deps/`, `refs/layers/`, `refs/blobs/`.

Key store methods: `path(pinned_id)`, `content(pinned_id)`, `metadata(pinned_id)`, `manifest(pinned_id)`, `resolve(pinned_id)`, `install_status(pinned_id)`, `digest_file(pinned_id)`, `metadata_for_content(content_path)`, `refs_symlinks_dir_for_content(content_path)`, `refs_deps_dir_for_content(content_path)`, `refs_layers_dir_for_content(content_path)`, `refs_blobs_dir_for_content(content_path)`, `resolve_for_content(content_path)`, `list_all() → Vec<PackageDir>`.

`PackageDir` accessors: `content()`, `metadata()`, `manifest()`, `resolve()`, `install_status()`, `digest_file()`, `refs_symlinks_dir()`, `refs_deps_dir()`, `refs_layers_dir()`, `refs_blobs_dir()`.

`*_for_content()` methods canonicalize path (follow install symlinks), navigate to sibling file/dir.

### TagStore — Local tag index

Layout: `{root}/tags/{registry_slug}/{repo_path}.json`

Key methods: `tags(identifier) → PathBuf`, `list_repositories(registry) → Vec<String>`.

### SymlinkStore — Install symlinks

Layout: `{root}/symlinks/{registry_slug}/{repo_path}/candidates/{tag}` + `current`

```rust
pub enum SymlinkKind { Candidate, Current }
```

Key methods: `candidate(identifier)`, `current(identifier)`, `candidates(identifier)`, `symlink(identifier, kind)`.

`candidate()` use `identifier.tag_or_latest()` (fall back to `"latest"` if no tag).

### TempStore — Download staging

Layout: `{root}/temp/{32-hex-hash}/` — deterministic SHA-256 hash of full identifier string.

Lock file sits as sibling: `{32-hex-hash}.lock`. `try_acquire()` non-blocking; stale artifacts cleaned on acquire. Temp dir atomically renamed into `packages/` on completion.

## Path Construction

### Slugification

All OCI identifier parts slugified via `to_relaxed_slug()`: keep `[a-zA-Z0-9._-]`, replace rest with `_`. Example: `localhost:5000` → `localhost_5000`.

### Repository Path Splitting

**Never use `.join("org/project/tool")`** — embed literal `/`, cause mixed separators on Windows. Use `repository_path()` — split on `/`:

```rust
pub(crate) fn repository_path(repository: &str) -> PathBuf {
    repository.split('/').collect()
}
```

### Digest Sharding (`cas_path.rs`)

`cas_shard_path(digest)` produce `{algorithm}/{hex[0..2]}/{hex[2..32]}` — 2-level shard, 32 hex chars total in path. Full digest NOT recoverable from path alone; written to sibling `digest` file by `write_digest_file()`.

`CAS_SHARD_DEPTH = 3` (algorithm + prefix + suffix). Store walkers set `max_depth` to `1 + CAS_SHARD_DEPTH` (extra 1 = registry slug level).

`CasTier` enum: `Package`, `Layer`, `Blob` — used by GC and reporting.

## ReferenceManager

Manage install symlinks and cross-tier forward-refs for safe GC. **Always use this for install symlinks, never raw `symlink::update/create`.**

```rust
pub fn link(&self, forward_path: &Path, content_path: &Path) -> Result<()>
pub fn unlink(&self, forward_path: &Path) -> Result<()>
pub fn link_dependency(&self, dependent_content: &Path, dependency_content: &Path, dependency_digest: &oci::Digest) -> Result<()>
pub fn unlink_dependency(&self, dependent_content: &Path, dependency_digest: &oci::Digest) -> Result<()>
pub fn broken_refs(&self) -> Result<Vec<PathBuf>>
```

**Arg order for `link()` is `(link, target)` — opposite of `symlink::update(target, link)`.** Common confusion source.

Install back-reference: `packages/.../refs/symlinks/{16_hex}` → forward symlink path. Name from `name_for_path(forward_path)` = first 16 hex chars of SHA-256(path bytes).

Dependency forward-ref: `packages/.../refs/deps/{algorithm}_{32_hex}` → dependency's `content/`. Name from `cas_ref_name(digest)` (in `cas_path` module, re-exported from `file_structure`) = `"{algorithm}_{first_32_hex}"`. No back-ref in dependency's `refs/symlinks/`.

Layer forward-ref: created by `pull` directly (not via `ReferenceManager`). Symlink in `refs/layers/` target `layers/.../content/`. GC recover layer entry dir via `.parent()` on target.

Blob forward-ref: created by `pull` directly. Symlink in `refs/blobs/` target `blobs/.../data`. GC recover blob entry dir via `.parent()` on target.

`broken_refs()` check only `refs/symlinks/` — not `refs/deps/`, `refs/layers/`, `refs/blobs/`.

**Idempotent**: `link()` no-op if forward already points to content. `unlink()` no-op if forward absent.

## GC Safety

GC (`garbage_collection/reachability_graph.rs`) build `ReachabilityGraph` covering all three CAS tiers in single BFS pass. Packages with live `refs/symlinks/` entries or digests pinned by any registered project's `ocx.lock` (from `projects.json`) = roots. BFS follow four edge types from each package: `refs/deps/` (dependent packages), `refs/layers/` (extracted layers), `refs/blobs/` (raw blobs). Layers and blobs no outgoing edges — reachable only via package refs. Unreachable = collected.

Project-registry roots read via `ProjectRegistry::load_and_prune` at start of `ocx clean`. Entries whose `ocx_lock_path` no longer exists on disk are silently dropped from the registry before the GC walk. Pass `--force` to `ocx clean` to ignore the registry and collect all packages not held by live symlinks.

Blobs first-class BFS entries: every `CasTier` variant (`Package`, `Layer`, `Blob`) included in reachability walk. Previous `tier != CasTier::Blob` skip removed; blobs retained only when live `refs/blobs/` symlink points to them.

`BlobGuard` (`blob_store/blob_guard.rs`) provide RAII shared/exclusive advisory locking for individual blob data files. Acquire read lock before read, write lock before write. Internals use `file_lock::FileLock` (wraps `fs2` in `spawn_blocking`) — do not call `BlobStore::data()` directly in concurrent paths; always go through `BlobGuard::acquire_read` / `acquire_write`.

## symlink Module

Low-level primitives. Use only for non-package symlinks or inside `ReferenceManager`:
- `create(target, link)` — create symlink + parent dirs
- `update(target, link)` — create or replace
- `remove(link)` — remove; no-op if absent
- `is_link(path)` — use instead of `is_symlink()` — handle Windows NTFS junctions

Windows: use NTFS junction points (no privilege escalation needed).

## hardlink Module

Low-level primitives for file-level dedup during layer assembly:
- `create(source, link)` — hardlink; create parent dirs; fail if link exists or cross-device
- `update(source, link)` — create or replace

Cross-device hardlinks fail with `io::ErrorKind::CrossesDevices`. `$OCX_HOME` must sit on single volume — required by `temp → packages/` atomic rename.

**Assembly walker**: `utility/fs/assemble_from_layer(source_content, dest_content)` mirror layer's `content/` tree into package's `content/` dir — hardlink regular files via `hardlink::create`, create real subdirs, recreate intra-layer symlinks verbatim. `packages/{P}/content/` is real dir, not symlink into `layers/`. Walker fan out dir-level tasks through semaphore-bounded `JoinSet`; per-task stats return-and-summed (no shared mutex). Windows layer symlinks return `io::ErrorKind::Unsupported`.