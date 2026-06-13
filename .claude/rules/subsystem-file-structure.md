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

## Zone Model (P1 — landed)

`StoreLayout` (`store_layout.rs`) resolves zone roots once; `FileStructure::with_layout` maps each store to the correct zone. `with_root(root)` is a thin shim over `with_layout(StoreLayout::from_root(root))`.

| Env var | Zone | Stores | Default |
|---------|------|--------|---------|
| `OCX_CACHE_DIR` | cache | `blobs/`, `layers/`, `layer_temp/` | `$OCX_HOME` |
| `OCX_PACKAGES_DIR` | packages | `packages/`, `temp/` | resolved `OCX_CACHE_DIR` |
| `OCX_STATE_DIR` | state | `symlinks/`, `state/`, `projects/` | `$OCX_HOME` |
| `OCX_INDEX` | tags override | `tags/` | `{cache}/tags` |

All zones collapse to `$OCX_HOME` by default — byte-identical to the pre-P1 layout. **Two temp stores:** `temp` (packages zone) and `layer_temp` (cache zone) are co-located with their tier for intra-volume atomic renames; they are the same directory in the unified layout. **`state_zone_root()` accessor** returns `OCX_STATE_DIR` (default `$OCX_HOME`) — `ProjectRegistry` must use this, not `root()`, so the `projects/` ledger stays per-instance when zones diverge.

## Module Map

| File | Purpose | Key Types |
|------|---------|-----------|
| `file_structure.rs` | Composite root; `slugify()`, `repository_path()` | `FileStructure` |
| `store_layout.rs` | Zone resolver; `from_root` (single-root), `resolve` (zone overrides), `resolve_from_env` | `StoreLayout` |
| `blob_store.rs` | Raw OCI blob storage; stateless `write_blob` / `read_blob` (tempfile + atomic rename + Windows-cfg retry-with-backoff) | `BlobStore`, `BlobDir` |
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
| `hardlink.rs` | Hardlink create/update — THE ONE place `std::fs::hard_link` lives; same-filesystem only | assemble walker, codesign |
| `reflink.rs` | Cross-filesystem file placement: CoW reflink or full-copy fallback; independent inode | assemble walker (`AssemblyMode::Reflink` path) |
| `utility/fs/path.rs` | Lexical path helpers: `lexical_normalize`, `escapes_root`, `validate_symlinks_in_dir` | symlink, archive extractor, assemble walker |

## FileStructure (composite root)

```rust
pub struct FileStructure {
    pub blobs: BlobStore,
    pub layers: LayerStore,
    pub packages: PackageStore,
    pub tags: TagStore,
    pub symlinks: SymlinkStore,
    pub state: StateStore,
    pub temp: TempStore,       // packages zone — co-located with packages/
    pub layer_temp: TempStore, // cache zone — co-located with layers/
}
```

One instance per session. Sub-stores public fields. `root()` returns `$OCX_HOME`. `state_zone_root()` returns the per-instance state-zone root (see Zone Model above).

**State-zone files (under `OCX_STATE_DIR`, default `$OCX_HOME`):**

| Path | Purpose |
|------|---------|
| `projects/` | Project GC ledger — flat directory of one symlink per registered project. Name = first 16 hex chars of `SHA-256(canonical_abs_project_dir)`; target = the project directory. Updated by `ProjectRegistry::register` after every `ocx lock` save. Read by `ocx clean` via `ProjectRegistry::live_projects()` to retain cross-project packages. Lives in the STATE zone (not the cache zone) so each fleet member keeps its own GC roots when `OCX_STATE_DIR` is set. ADR: `adr_project_gc_symlink_ledger.md`. |
| `state/` | Persistent runtime state. Distinct from `cache/` (regenerable bulk). Subdirectory entries are small files whose existence or mtime IS the data — no content structure required. Never regenerated; persisted across sessions. |
| `state/update-check/<slug>` | Update-check throttle state file. `<slug>` = `to_slug(identifier)` — strict no-dot slug, for example `ocx_sh_ocx_cli` for `ocx.sh/ocx/cli`. File is always zero-byte; mtime is the only datum (time of last registry probe). Touched after every registry probe (success or error); NOT touched on throttle short-circuit. Parent directory created lazily on first touch. Written atomically via PID-suffixed temp file + `std::fs::rename`. |

`$OCX_HOME/projects.json` and `$OCX_HOME/.projects.lock` from the prior JSON ledger are obsolete — safe to delete. `ocx clean` removes them opportunistically with a single debug log if encountered.

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

GC (`garbage_collection/reachability_graph.rs`) build `ReachabilityGraph` covering all three CAS tiers in single BFS pass. Packages with live `refs/symlinks/` entries or digests pinned by any registered project's `ocx.lock` = roots. BFS follow four edge types from each package: `refs/deps/` (dependent packages), `refs/layers/` (extracted layers), `refs/blobs/` (raw blobs). Layers and blobs no outgoing edges — reachable only via package refs. Unreachable = collected.

Project-registry roots read via `ProjectRegistry::live_projects()` at the start of `ocx clean`. The ledger is the flat symlink store at `$OCX_HOME/projects/` (ADR: `adr_project_gc_symlink_ledger.md`). Each symlink's liveness is a three-state probe (`Live`/`Dead`/`Unknown`): a broken or lock-absent link is pruned silently at debug; a transiently unreachable link (`Unknown` — NFS/automount) is retained with one WARN and excluded as a root this run. There is no parse surface to corrupt — the prior `projects.json` JSON ledger and its corrupt-registry exit-78 branch are eliminated. Pass `--force` to `ocx clean` to ignore the registry and collect all packages not held by live symlinks.

Blobs first-class BFS entries: every `CasTier` variant (`Package`, `Layer`, `Blob`) included in reachability walk. Previous `tier != CasTier::Blob` skip removed; blobs retained only when live `refs/blobs/` symlink points to them.

`BlobStore::write_blob` / `read_blob` (stateless) are the only blob-IO entry points. Writes use `tempfile::NamedTempFile::new_in(parent)` + `sync_data` + atomic rename to the CAS path; content-addressed invariant (same digest ⟹ same bytes) means concurrent writers produce byte-equivalent output and the rename is idempotent. On Windows, `persist_with_windows_retry` wraps the rename with exponential backoff (3 retries: 100/400/800 ms ±25 % jitter) on `ERROR_SHARING_VIOLATION` (32) and `ERROR_ACCESS_DENIED` (5) — Windows Defender real-time scanning on `windows-latest` GitHub Actions runners is the dominant source of these transients (rattler `rename_with_retry` precedent). After retry exhaustion the function re-checks existence and returns Ok if the path now exists (idempotent recovery).

Same-process dedup for concurrent same-digest fan-out lives in `package_manager::tasks::pull_local::PullCoordinator`, which owns a `singleflight::Group<oci::Digest, ()>` scoped per pull operation. The OCI manifest layer-download path is the only caller with concurrent same-digest writes; index-layer callers (`write_manifest_blob`, `local_index::stage_blob_bytes`) call `BlobStore::write_blob` directly without dedup — they are sequential and the content-addressed invariant covers any rare cross-process race.

There is no `LockFileEx` on blob `data` files. The F1 class (cross-process raw reader of a locked range → `ERROR_LOCK_VIOLATION`) is structurally impossible — see ADR `adr_file_lock_unification.md` §Decision 2.

## symlink Module

Low-level primitives. Use only for non-package symlinks or inside `ReferenceManager`:
- `create(target, link)` — create symlink + parent dirs
- `update(target, link)` — create or replace
- `remove(link)` — remove; no-op if absent
- `is_link(path)` — use instead of `is_symlink()` — handle Windows NTFS junctions

Windows: use NTFS junction points (no privilege escalation needed).

**ARCH-4b — sanctioned exception for `$OCX_HOME/projects/`:** `ProjectRegistry` uses `symlink::create`/`update` (via `symlink::replace_atomic`) directly, bypassing `ReferenceManager`. This is intentional and must not be "fixed":

- `projects/` links target an absolute path **outside** `$OCX_HOME` (the project directory). `ReferenceManager::link()` validates containment inside `refs/` — that check is correct for install back-refs and must reject external targets.
- `projects/` links are categorically not install back-refs. They are GC-root registrations with a flat-symlink liveness model (ADR: `adr_project_gc_symlink_ledger.md`).
- Any future reviewer who sees raw `symlink::` calls in `project/registry.rs` should recognise this carve-out, not re-flag it as a violation of the "always use `ReferenceManager`" rule.

## hardlink Module

Low-level primitives for file-level dedup during layer assembly:
- `create(source, link)` — hardlink; create parent dirs; fail if link exists or cross-device
- `update(source, link)` — create or replace

Cross-device hardlinks fail with `io::ErrorKind::CrossesDevices`. The `temp → packages/` atomic rename within the packages zone requires same-filesystem. When `OCX_PACKAGES_DIR` and `OCX_CACHE_DIR` are on different volumes, the assembly walker falls back to `reflink::create` — see reflink Module below.

**Assembly walker**: `utility/fs/assemble_from_layer(source_content, dest_content)` mirror layer's `content/` tree into package's `content/` dir. Per-layer assembly mode (`AssemblyMode`) is computed by probing `utility::fs::same_filesystem(source, dest)`:
- **Same filesystem** → `hardlink::create` (shared inode, zero extra disk)
- **Different filesystem** → `reflink::create` (independent inode, CoW clone or byte-copy fallback)

Probe failure defaults to `Reflink` (safe across devices). Walker fan out dir-level tasks through semaphore-bounded `JoinSet`; per-task stats return-and-summed as `AssemblyStats`. `is_fully_independent()` on stats gates macOS re-signing after cross-volume assembly. Windows layer symlinks return `io::ErrorKind::Unsupported`.

## reflink Module

`crates/ocx_lib/src/reflink.rs` — THE ONE place `reflink_copy::reflink_or_copy` is called. Mirrors the `hardlink` module for the cross-device case.

- `create(source, link)` — reflink (CoW clone) or full byte-copy fallback; independent inode; create parent dirs; fail if link exists
- `Ok(None)` from underlying `reflink_or_copy` = CoW reflink succeeded (btrfs cross-subvolume, APFS); `Ok(Some(bytes))` = full copy performed (ext4, tmpfs, no CoW support)

**When to use vs `hardlink::create`:** same-filesystem → `hardlink::create` (shared inode, zero-copy). Different filesystem → `reflink::create` (independent inode, CoW or copy). The assembly walker probes `same_filesystem` and dispatches through `AssemblyMode`.

**macOS re-signing:** packages assembled cross-volume have `AssemblyStats::is_fully_independent() == true`; the pull pipeline gates ad-hoc codesign on this flag. Re-signing is suppressed by `OCX_NO_CODESIGN`.