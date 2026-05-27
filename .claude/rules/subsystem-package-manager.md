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
| `tasks/resolve.rs` | `resolve()`, `resolve_all()`, `resolve_env(packages, self_view: bool)` — index + env resolution; `self_view` bool selects interface surface (`false`) or private surface (`true`) via `composer::compose` |
| `tasks/find.rs` | `find()`, `find_plain()`, `find_all()` — resolve installed packages |
| `tasks/find_symlink.rs` | `find_symlink()`, `find_symlink_all()` — resolve via candidate/current |
| `tasks/find_or_install.rs` | `find_or_install()`, `find_or_install_all()` — auto-install on miss |
| `tasks/pull.rs` | `pull()`, `pull_all()` — download + transitive deps (PullTracker module-private) |
| `tasks/install.rs` | `install()`, `install_all()` — pull + create symlinks |
| `tasks/uninstall.rs` | `uninstall()`, `uninstall_all()` — remove symlinks, optional purge |
| `tasks/deselect.rs` | `deselect()`, `deselect_all()` — remove current symlink |
| `tasks/clean.rs` | `clean()` — GC unreferenced objects + stale temps; `collect_project_roots` free function — calls `ProjectRegistry::live_projects()` (flat symlink ledger, no JSON parse), resolves each live project dir's `ocx.lock` pinned digests into `Vec<ProjectRootDigests>`, **plus an implicit `$OCX_HOME/ocx.lock` root** (global toolchain — its project dir is `$OCX_HOME`, barred from the ledger by `adr_project_gc_symlink_ledger.md`, so added unconditionally; absent lock → `Ok(None)` → no-op; ADR `adr_global_toolchain_tier.md` D5 amended 2026-05-19); opportunistically removes legacy `projects.json`/`.projects.lock` if found; no corrupt-registry exit-78 branch (eliminated with the JSON parse surface) |
| `tasks/update_check.rs` | Update-check task methods + throttle machinery. Exports `SkippedReason` enum (`Bootstrap`, `Offline`, `Throttled`, `RegistryProbeFailed(String)`, `NotFound`, `UnparseableCurrent(String)`, `UnparseableLatest`, `NoReleaseTag`) with `Display` + JSON serialization. Exports `UpdateCheckResult` (`AlreadyUpToDate`, `Skipped(SkippedReason)`, `UpdateAvailable(Identifier)`) and `SelfUpdateResult` (`AlreadyUpToDate`, `Installed { from: Option<String>, to }`, `Skipped(SkippedReason)`). Public methods: `check_update`, `self_check_update(throttle)`, `self_update()`. Private helpers: `query_installed_version` (subprocess-based, `tokio::process::Command`), `is_throttled`, `touch_state_atomic`, `find_latest_version`. `installed_version` / `installed_version_from_paths` deleted (2026-05-27). Full throttle contract and touch policy in the module `//!` doc. |
| `composer.rs` | Two-env composition: `compose(roots, store, self_view: bool) -> Vec<Entry>` (flat iteration over each root's pre-built TC with cross-root dedup, surface-gated via `has_interface()`/`has_private()`); `check_entrypoints(roots, store)` (interface-projection collision gate over 1..N roots, reports all N owners) |

## Facade Pattern

```rust
pub struct PackageManager {
    file_structure: FileStructure,
    index: oci::index::Index,
    client: Option<oci::Client>,  // None when offline
    default_registry: String,
}
```

All fields cheap to clone. `is_offline()` returns `client.is_none()`. `client()` returns `Option<&oci::Client>` (use when missing client should fall back). `require_client()` returns `Err(OfflineMode)` if no client (use at sites that need network).

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
| `pull_local(info, layers, dest_override)` | **Yes** (deps) | `InstallInfo` | Materialize from local metadata + layers; `dest_override: Option<&Path>` is a sanctioned hook through `setup_owned` / `move_temp_to_object_store`; bypasses `setup_impl` singleflight gate so concurrent calls with different `dest_override` paths each get their own materialization |
| `find()` / `find_all()` | No | `InstallInfo` | Resolves locally only; calls `link_blobs` |
| `find_symlink()` / `find_symlink_all()` | No | `InstallInfo` | Via candidate/current symlink; calls `link_blobs` |
| `find_or_install()` / `find_or_install_all()` | **Yes** (if online) | `InstallInfo` | Falls through to install on NotFound |
| `install()` / `install_all()` | N/A | `InstallInfo` | Downloads; `candidate` flag creates symlink; `select` flag sets current |
| `uninstall()` / `uninstall_all()` | N/A | `Option<UninstallResult>` | None = candidate already absent |
| `deselect()` / `deselect_all()` | N/A | `Option<PathBuf>` | None = current already absent |
| `clean(dry_run, force)` | N/A | `CleanResult` | Removes unreferenced objects + stale temps; `force=true` bypasses project registry |
| `check_update(identifier, throttle)` | N/A | `UpdateCheckResult` | Generic update-check for any identifier. Throttle: `None` = 24h, `Some(ZERO)` = bypass, `Some(d)` = custom. Returns `Skipped` on short-circuit (no state touch). |
| `self_check_update(throttle)` | N/A | `UpdateCheckResult` | Convenience wrapper for the canonical `ocx.sh/ocx/cli`. Resolves the running version via subprocess (`ocx --format json version` on the current symlink binary); returns `Skipped(SkippedReason::Bootstrap)` when subprocess fails (binary absent / exec fail / non-zero exit / malformed JSON). Otherwise compares remote latest against installed; returns `AlreadyUpToDate` when remote ≤ current. Same throttle convention. |
| `self_update()` | N/A | `SelfUpdateResult` | Checks (always bypasses throttle) then installs via `install_all(candidate=false, select=true)` if newer. Bootstrap mode no longer short-circuits — install always proceeds. Returns `Installed { from: Option<String>, to }` on success (`from` is `None` when subprocess version query fails), `AlreadyUpToDate` if current, `Skipped(SkippedReason)` on soft failure. Wraps check errors in `Error::SelfCheckFailed`. |

**`_all` methods must preserve input order** — caller zips results with original identifiers.

## Update-Check Throttle Convention

All three update-check methods share one `throttle: Option<Duration>` contract:

| Value | Behaviour |
|-------|-----------|
| `None` | Default 24-hour interval (auto-check path from `app.rs`) |
| `Some(Duration::ZERO)` | Bypass; always query the registry |
| `Some(d)` | Custom interval |

`self_update` always bypasses (explicit user intent — throttle parameter is not exposed).

**State-file touch policy** (in `$OCX_HOME/state/update-check/<slug>`):

- Touch on successful probe (any `UpdateCheckResult` variant returned cleanly).
- Touch on probe error (avoids hammering a broken registry on every command).
- **Do NOT touch** on throttle short-circuit — touching on short-circuit would extend the window indefinitely.

The slug is `to_slug(identifier.to_string())` — replaces all non-alphanumeric characters with `_`. For `ocx.sh/ocx/cli` this produces `ocx_sh_ocx_cli` (no dots). File content is always zero bytes; mtime is the data. See `subsystem-file-structure.md` for the directory contract.

## Parallel vs Sequential

- **Parallel** (via `JoinSet`): `find_all`, `find_or_install_all`, `install_all`
- **Sequential**: `find_symlink_all`, `uninstall_all`, `deselect_all`, `clean`

## Progress Pattern

Span-free. Progress is rendered through `crate::cli::progress::ProgressManager`
(owns one `indicatif::MultiProgress`), **not** `tracing` spans. ADR:
`adr_progress_architecture.md`. `PackageManager` carries a `progress` field
(`with_progress`, default `disabled()` for library/test consumers); CLI
`Context` injects the shared stderr manager.

- Parallel tasks (`JoinSet`): each spawned task creates its own
  `progress.spinner(label)` guard inside the async block; the guard clears
  on task completion. No `.instrument()`.
- Sequential tasks: hold a `Spinner` guard for the loop body.
- `ProgressManager`/guards are `indicatif`-backed (`Send + Sync + Clone`,
  no span registry) so concurrent create/use/drop cannot hit the
  `tracing_subscriber` sharded-registry clone-after-close panic.
- Disabled manager (non-TTY) → guards are cheap no-ops.

## OCX Configuration Forwarding

Generated entrypoint launchers re-enter ocx via `ocx launcher exec '<pkg-root>' -- <argv0> [args...]`. Any subprocess spawn site that may chain back into ocx MUST forward the running ocx's resolution-affecting config onto the child env via `env::Env::apply_ocx_config(ctx.config_view())`. Full rule + Block-tier review criteria live in `subsystem-cli.md` "Cross-Cutting: OCX Configuration Forwarding".

### Hermetic subprocess pattern (env_clear + envs + timeout)

A distinct second subprocess invocation pattern is used by self-update's version
query in `tasks/update_check.rs::query_installed_version`:

- Builds an `env::Env` from `resolve_env(..., self_view=false)` for the queried
  identifier.
- Calls `tokio::process::Command::new(bin).env_clear().envs(env)` — the child
  receives **only** the `resolve_env`-composed entries; no `HOME`, no inherited
  `PATH`, no `OCX_*`.
- Wraps the `.output()` future in `tokio::time::timeout(Duration::from_secs(5),
  …)` — a hung installed binary must not stall update-check.
- Treats any failure (resolve fail, exec error, non-zero exit, timeout,
  malformed JSON) as `None` → caller routes to `Skipped(Bootstrap)`.

This pattern is **NOT** for re-entering ocx through a launcher (use
`apply_ocx_config` for that). It is the right shape only when the spawned
subcommand must produce a hermetic, side-effect-free response — currently only
`ocx --format json version`. The invoked command MUST stay pure-version
(no `HOME`/`PATH`/`OCX_*` reads) or the query silently bootstraps. The
`Version::execute` doc-comment links back to this contract.

The stable wire ABI is the `launcher` + `exec` subcommand name pair and positional shape. Byte-exact golden tests at `body.rs::tests` act as canaries — any template change that changes the launcher body must update the golden strings there.

**Wire-ABI canary rule — two producers, both must agree.** After the Windows `.cmd` cutover (`adr_windows_exe_shim.md` Axis C → C2: no `.cmd` is emitted), the `launcher exec "<pkg_root>" -- "<stem>"` wire string has exactly two independent producers that must stay in sync:

1. `body.rs::unix_launcher_body` — `.sh` template body (golden test `launcher_wire_token_is_bound_to_shim_producer`, sh-branch assertion)
2. `crates/ocx_shim/src/core.rs` `WIRE_SUBCOMMAND` — the native Windows `.exe` shim (golden test `shim_wire_token_matches_sh_body` in `ocx_shim`)

`ocx_lib` cannot depend on the `ocx_shim` binary crate, so the binding is a **paired golden**: `body.rs::tests::launcher_wire_token_is_bound_to_shim_producer` restates `WIRE_SUBCOMMAND = "launcher exec"` from the `ocx_lib` (`.sh`) side; `ocx_shim::tests::shim_wire_token_matches_sh_body` restates it from the shim side. A change to the wire vocabulary must touch **both** canaries or one fails loudly at test time. (The former third producer — `body.rs::windows_launcher_body`, the `.cmd` body — was removed with the cutover.)

## Quality Gate

During review-fix loops, run `task rust:verify` — not full `task verify`.
Full `task verify` = final gate before commit.