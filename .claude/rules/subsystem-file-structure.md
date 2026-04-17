---
paths:
  - crates/ocx_lib/src/file_structure/**
  - crates/ocx_lib/src/file_structure.rs
  - crates/ocx_lib/src/reference_manager.rs
  - crates/ocx_lib/src/symlink.rs
---

# File Structure Subsystem

## Design Rationale

The old `objects/` tree conflated three concerns: raw OCI blobs, extracted layer files, and assembled packages. That conflation broke cross-repo dedup (repository was part of the CAS path), made shared layers impossible (one layer per package assumed), and had no place for non-extracted blobs (signatures, SBOMs). The three-tier split solves each problem with a clear lifecycle per tier:

- **`blobs/`** — raw OCI content (manifests, compressed layer archives); keyed by registry + digest; never extracted
- **`layers/`** — extracted layer trees shared across packages; keyed by registry + digest; content deduplication unit for multi-layer packages
- **`packages/`** — assembled packages; keyed by registry + digest only (no repo in path, enabling cross-repo dedup); contains hardlinked content assembled from one or more layers

Separating `refs/` into four named subdirs (`symlinks/`, `deps/`, `layers/`, `blobs/`) gives GC a single BFS pass over all three tiers. Only packages can be roots or have outgoing edges; layers and blobs are reachable only through package `refs/layers/` and `refs/blobs/` links.

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

These modules live at `crates/ocx_lib/src/` root because they are consumed across subsystems.

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

One instance per session. Sub-stores are public fields. `root()` returns the OCX home path. `profile_manifest()` returns `$OCX_HOME/profile.json`.

## Six Stores

### BlobStore — Raw OCI blobs

Layout: `{root}/blobs/{registry_slug}/{algorithm}/{2hex}/{30hex}/data`

Each blob directory contains: `data` (raw blob bytes), `digest` (full digest string for recovery).

Key methods: `path(registry, digest)`, `data(registry, digest)`, `digest_file(registry, digest)`, `list_all() → Vec<BlobDir>`.

`BlobDir` accessors: `data()`, `digest_file()`.

### LayerStore — Extracted layers

Layout: `{root}/layers/{registry_slug}/{algorithm}/{2hex}/{30hex}/content/`

Each layer directory contains: `content/` (extracted files), `digest`.

Key methods: `path(registry, digest)`, `content(registry, digest)`, `digest_file(registry, digest)`, `list_all() → Vec<LayerDir>`.

`LayerDir` accessors: `content()`, `digest_file()`.

### PackageStore — Assembled packages

Layout: `{root}/packages/{registry_slug}/{algorithm}/{2hex}/{30hex}/`

**Repository is NOT part of the path.** Only registry + digest determine location, enabling cross-repo dedup.

Each package directory contains: `content/`, `metadata.json`, `manifest.json`, `resolve.json`, `install.json`, `digest`, `refs/symlinks/`, `refs/deps/`, `refs/layers/`, `refs/blobs/`.

Key store methods: `path(pinned_id)`, `content(pinned_id)`, `metadata(pinned_id)`, `manifest(pinned_id)`, `resolve(pinned_id)`, `install_status(pinned_id)`, `digest_file(pinned_id)`, `metadata_for_content(content_path)`, `refs_symlinks_dir_for_content(content_path)`, `refs_deps_dir_for_content(content_path)`, `refs_layers_dir_for_content(content_path)`, `refs_blobs_dir_for_content(content_path)`, `resolve_for_content(content_path)`, `list_all() → Vec<PackageDir>`.

`PackageDir` accessors: `content()`, `metadata()`, `manifest()`, `resolve()`, `install_status()`, `digest_file()`, `refs_symlinks_dir()`, `refs_deps_dir()`, `refs_layers_dir()`, `refs_blobs_dir()`.

`*_for_content()` methods canonicalize the path (following install symlinks) then navigate to the sibling file/dir.

### TagStore — Local tag index

Layout: `{root}/tags/{registry_slug}/{repo_path}.json`

Key methods: `tags(identifier) → PathBuf`, `list_repositories(registry) → Vec<String>`.

### SymlinkStore — Install symlinks

Layout: `{root}/symlinks/{registry_slug}/{repo_path}/candidates/{tag}` + `current`

```rust
pub enum SymlinkKind { Candidate, Current }
```

Key methods: `candidate(identifier)`, `current(identifier)`, `candidates(identifier)`, `symlink(identifier, kind)`.

`candidate()` uses `identifier.tag_or_latest()` (falls back to `"latest"` if no tag).

### TempStore — Download staging

Layout: `{root}/temp/{32-hex-hash}/` — deterministic SHA-256 hash of the full identifier string.

Lock file lives as a sibling: `{32-hex-hash}.lock`. `try_acquire()` is non-blocking; stale artifacts cleaned on acquire. The temp dir is atomically renamed into `packages/` on completion.

## Path Construction

### Slugification

All OCI identifier components are slugified via `to_relaxed_slug()`: preserves `[a-zA-Z0-9._-]`, replaces everything else with `_`. Example: `localhost:5000` → `localhost_5000`.

### Repository Path Splitting

**Never use `.join("org/project/tool")`** — embeds literal `/`, causing mixed separators on Windows. Use `repository_path()` which splits on `/`:

```rust
pub(crate) fn repository_path(repository: &str) -> PathBuf {
    repository.split('/').collect()
}
```

### Digest Sharding (`cas_path.rs`)

`cas_shard_path(digest)` produces `{algorithm}/{hex[0..2]}/{hex[2..32]}` — a 2-level shard with 32 total hex chars encoded in the path. The full digest is NOT recoverable from the path alone; it is written to the sibling `digest` file by `write_digest_file()`.

`CAS_SHARD_DEPTH = 3` (algorithm + prefix + suffix). Store walkers set `max_depth` to `1 + CAS_SHARD_DEPTH` (the extra 1 is the registry slug level).

`CasTier` enum: `Package`, `Layer`, `Blob` — used by GC and reporting.

## ReferenceManager

Manages install symlinks and cross-tier forward-refs for safe GC. **Always use this for install symlinks, never raw `symlink::update/create`.**

```rust
pub fn link(&self, forward_path: &Path, content_path: &Path) -> Result<()>
pub fn unlink(&self, forward_path: &Path) -> Result<()>
pub fn link_dependency(&self, dependent_content: &Path, dependency_content: &Path, dependency_digest: &oci::Digest) -> Result<()>
pub fn unlink_dependency(&self, dependent_content: &Path, dependency_digest: &oci::Digest) -> Result<()>
pub fn broken_refs(&self) -> Result<Vec<PathBuf>>
```

**Arg order for `link()` is `(link, target)` — opposite of `symlink::update(target, link)`.** This is a common source of confusion.

Install back-reference: `packages/.../refs/symlinks/{16_hex}` → forward symlink path. Name derived via `name_for_path(forward_path)` = first 16 hex chars of SHA-256(path bytes).

Dependency forward-ref: `packages/.../refs/deps/{algorithm}_{32_hex}` → dependency's `content/`. Name derived via `cas_ref_name(digest)` (in `cas_path` module, re-exported from `file_structure`) = `"{algorithm}_{first_32_hex}"`. No back-ref in the dependency's `refs/symlinks/`.

Layer forward-ref: created by `pull` directly (not via `ReferenceManager`). Symlink in `refs/layers/` targets `layers/.../content/`. GC recovers the layer entry dir via `.parent()` on the target.

Blob forward-ref: created by `pull` directly. Symlink in `refs/blobs/` targets `blobs/.../data`. GC recovers the blob entry dir via `.parent()` on the target.

`broken_refs()` checks only `refs/symlinks/` — not `refs/deps/`, `refs/layers/`, or `refs/blobs/`.

**Idempotent**: `link()` is no-op if forward already points to content. `unlink()` is no-op if forward does not exist.

## GC Safety

GC (`garbage_collection/reachability_graph.rs`) builds a `ReachabilityGraph` covering all three CAS tiers in a single BFS pass. Packages with live `refs/symlinks/` entries or profile content-mode references are roots. BFS follows four edge types from each package: `refs/deps/` (dependent packages), `refs/layers/` (extracted layers), `refs/blobs/` (raw blobs). Layers and blobs have no outgoing edges — they are reachable only through package refs. Everything unreachable is collected.

Blobs are first-class BFS entries: every `CasTier` variant (`Package`, `Layer`, `Blob`) is included in the reachability walk. The previous `tier != CasTier::Blob` skip has been removed; blobs are retained only when a live `refs/blobs/` symlink points to them.

`BlobGuard` (`blob_store/blob_guard.rs`) provides RAII shared/exclusive advisory locking for individual blob data files. Acquire a read lock before reading, a write lock before writing. Internals use `file_lock::FileLock` (which wraps `fs2` in `spawn_blocking`) — do not call `BlobStore::data()` directly in concurrent paths; always go through `BlobGuard::acquire_read` / `acquire_write`.

## symlink Module

Low-level primitives. Use only for non-package symlinks or within `ReferenceManager`:
- `create(target, link)` — create symlink + parent dirs
- `update(target, link)` — create or replace
- `remove(link)` — remove; no-op if absent
- `is_link(path)` — use instead of `is_symlink()` — handles Windows NTFS junctions

Windows: uses NTFS junction points (no privilege escalation needed).

## hardlink Module

Low-level primitives for file-level dedup during layer assembly:
- `create(source, link)` — hardlink; creates parent dirs; fails if link exists or cross-device
- `update(source, link)` — create or replace

Cross-device hardlinks fail with `io::ErrorKind::CrossesDevices`. `$OCX_HOME` must live on a single volume — required by the `temp → packages/` atomic rename.

**Assembly walker**: `utility/fs/assemble_from_layer(source_content, dest_content)` mirrors a layer's `content/` tree into a package's `content/` directory — hardlinking regular files via `hardlink::create`, creating real subdirectories, recreating intra-layer symlinks verbatim. `packages/{P}/content/` is a real directory, not a symlink into `layers/`. The walker fans out directory-level tasks through a semaphore-bounded `JoinSet`; per-task stats are return-and-summed (no shared mutex). Windows layer symlinks return `io::ErrorKind::Unsupported`.
