---
paths:
  - crates/ocx_lib/src/package_manager/**
  - crates/ocx_lib/src/package_manager.rs
---

# Package Manager Subsystem

Facade over FileStructure + Index + Client. Task impls at `crates/ocx_lib/src/package_manager/`.

## Design Rationale

Facade = single coord point for all package ops. Hide store + index + client complexity. Three-layer errors (`Error` → `PackageError` → `PackageErrorKind`) = per-package diagnosis in batch ops. `_all` methods report which package failed + why, keep going on rest. See `arch-principles.md` for full pattern catalog.

## Task Module Architecture

`PackageManager` extended via `impl PackageManager` blocks in `tasks/` submodules. All task modules share one `impl` namespace, so **keep each module's `impl PackageManager` surface minimal**:

- **Only `pub` methods** on `impl PackageManager` — facade API, called by CLI.
- **Impl details = module-private free functions** — helpers, multi-step orchestration, internal state machines. Free functions take explicit params (`&FileStructure`, `&ObjectStore`, etc.) not `&self`. Prevent accidental coupling to full facade.
- **Extract to free function when**: method has private helpers, orchestrates multi steps, or clutter shared `impl` namespace.
- **Keep inline when**: method self-contained, no sub-helpers.
- **`tasks/common.rs`** — shared free functions (`find_in_store`, `load_object_data`, `reference_manager`, `export_env`), visible only to sibling task modules. No `impl PackageManager`.
- **`package_manager.rs` stays lean** — only struct def, constructor, field accessors, `is_offline()`. All business logic in task modules.

## Module Map

| File | Purpose |
|------|---------|
| `package_manager.rs` | `PackageManager` facade struct + accessors only |
| `error.rs` | Three-layer error model |
| `tasks/common.rs` | Shared free functions for task modules |
| `tasks/resolve.rs` | `resolve()`, `resolve_all()`, `resolve_env()` — index + env resolution |
| `tasks/find.rs` | `find()`, `find_plain()`, `find_all()` — resolve installed packages |
| `tasks/find_symlink.rs` | `find_symlink()`, `find_symlink_all()` — resolve via candidate/current |
| `tasks/find_or_install.rs` | `find_or_install()`, `find_or_install_all()` — auto-install on miss |
| `tasks/pull.rs` | `pull()`, `pull_all()` — download + transitive deps (PullTracker module-private) |
| `tasks/install.rs` | `install()`, `install_all()` — pull + create symlinks |
| `tasks/uninstall.rs` | `uninstall()`, `uninstall_all()` — remove symlinks, optional purge |
| `tasks/deselect.rs` | `deselect()`, `deselect_all()` — remove current symlink |
| `tasks/clean.rs` | `clean()` — GC unreferenced objects + stale temps |
| `tasks/profile_resolve.rs` | Profile-related resolution |

## Facade Pattern

```rust
pub struct PackageManager {
    file_structure: FileStructure,
    index: oci::index::Index,
    client: Option<oci::Client>,  // None when offline
    default_registry: String,
    profile: ProfileManager,
}
```

All fields cheap to clone. `is_offline()` returns `client.is_none()`. `client()` returns `Err(OfflineMode)` if no client.

## Three-Layer Error Model

```rust
// Layer 1: Task-level (one variant per command)
enum Error {
    FindFailed(Vec<PackageError>),
    InstallFailed(Vec<PackageError>),
    UninstallFailed(Vec<PackageError>),
    DeselectFailed(Vec<PackageError>),
    ResolveFailed(Vec<PackageError>),  // returned by resolve_all()
}

// Layer 2: Package-specific
struct PackageError {
    identifier: oci::Identifier,
    kind: PackageErrorKind,
}

// Layer 3: Error cause
enum PackageErrorKind {
    NotFound,
    SelectionAmbiguous(Vec<oci::Identifier>),
    SymlinkRequiresTag,
    SymlinkNotFound(SymlinkKind),
    Internal(crate::Error),
}
```

**Convention**: Single-item methods return `Result<T, PackageErrorKind>`. `_all` batch methods return `Result<T, Error>`.

`resolve_all()` returns `Err(Error::ResolveFailed(...))` when one+ packages fail to resolve. No reuse of `FindFailed`. `find_all()` still uses `FindFailed` for failures during install-lookup phase.

## ResolvedChain

`PackageManager::resolve()` returns `ResolvedChain` struct. Carries full OCI resolution chain traversed for identifier — image index manifest, platform manifest, intermediate manifests — plus their digests. Callers (`pull`, `find`, `find_symlink`) pass struct to `ReferenceManager::link_blobs` to populate `refs/blobs/` so GC traces full chain.

## link_blobs Call Pattern

`ReferenceManager::link_blobs(content_path, chain)` = `async fn`. Creates symlink in
`refs/blobs/` per blob digest in chain, target = matching `BlobStore` data file.
Empty chain = no-op (returns `Ok(())`, no directory created).

Called by `pull`, `find`, `find_symlink` after resolving package:

```
let chain = manager.resolve(identifier).await?;
reference_manager.link_blobs(pkg.content(), chain.blobs()).await?;
```

TOCTOU `!target.exists()` pre-check intentionally absent — eventual consistency handles dangling refs, idempotent `symlink::update` makes repeated calls safe.

## Task Methods

| Method | Auto-Install | Returns | Notes |
|--------|-------------|---------|-------|
| `find()` / `find_all()` | No | `InstallInfo` | Resolves locally only; calls `link_blobs` |
| `find_symlink()` / `find_symlink_all()` | No | `InstallInfo` | Via candidate/current symlink; calls `link_blobs` |
| `find_or_install()` / `find_or_install_all()` | **Yes** (if online) | `InstallInfo` | Falls through to install on NotFound |
| `install()` / `install_all()` | N/A | `InstallInfo` | Downloads; `candidate` flag creates symlink; `select` flag sets current |
| `uninstall()` / `uninstall_all()` | N/A | `Option<UninstallResult>` | None = candidate already absent |
| `deselect()` / `deselect_all()` | N/A | `Option<PathBuf>` | None = current already absent |
| `clean()` | N/A | `CleanResult` | Removes unreferenced objects + stale temps |

**`_all` methods must preserve input order** — caller zips results with original identifiers.

## Parallel vs Sequential

- **Parallel** (via `JoinSet`): `find_all`, `find_or_install_all`, `install_all`
- **Sequential**: `find_symlink_all`, `uninstall_all`, `deselect_all`, `clean`

## Progress Pattern

`tracing` `info_span!` in `_all` methods + `tracing-indicatif` `IndicatifLayer` in CLI subscriber.

- Parallel tasks (`JoinSet`): each task spawned with `.instrument(span)` carrying package name
- Sequential tasks: `.entered()` guard inside loop
- No custom progress abstraction

## Quality Gate

During review-fix loops, run `task rust:verify` — not full `task verify`.
Full `task verify` = final gate before commit.