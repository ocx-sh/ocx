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
| `tasks/common.rs` | Shared free functions for task modules; `load_config_metadata` enforces `MAX_METADATA_BLOB_BYTES` (4 MiB) on every config blob it loads, in two steps: pre-fetch rejection of an over-cap declared descriptor size (no network/cache touch), then a post-fetch re-check of the actual fetched length — defends against a registry declaring small but serving large; `stage_chain_blobs` extraction (staging loop shared with install/pull; `stage_and_link_chain_blobs` = staging + ref-linking wrapper) |
| `tasks/resolve.rs` | `resolve()`, `resolve_all()`, `resolve_env(packages, self_view: bool)` — index + env resolution; `self_view` bool selects interface surface (`false`) or private surface (`true`) via `composer::compose`; `AdmittedBinaries { binaries: Vec<(PinnedIdentifier, BinaryName)>, entrypoints: Vec<(PinnedIdentifier, EntrypointName)> }` + `resolve_env_with_attribution(packages, self_view, scope)` — sibling accessor to `resolve_env_with_patch_boundary` (unchanged signature) that additionally surfaces the admitted-set `binaries`/`entrypoints` claim attribution consumed by `ocx env` / `ocx package env`. ADR `adr_declared_binaries_metadata.md` §4. |
| `tasks/find.rs` | `find()`, `find_plain()`, `find_all()` — resolve installed packages |
| `tasks/find_symlink.rs` | `find_symlink()`, `find_symlink_all()` — resolve via candidate/current |
| `tasks/find_or_install.rs` | `find_or_install()`, `find_or_install_all()` — auto-install on miss |
| `tasks/pull.rs` | `pull()`, `pull_all()` — download + transitive deps (PullTracker module-private) |
| `tasks/install.rs` | `install()`, `install_all()` — pull + create symlinks |
| `tasks/uninstall.rs` | `uninstall()`, `uninstall_all()` — remove symlinks, optional purge |
| `tasks/deselect.rs` | `deselect()`, `deselect_all()` — remove current symlink |
| `tasks/inspect.rs` | `inspect()`, `inspect_all()` — read-only inspection (candidates / metadata+layers / resolution chain); no install or symlink side effects, and **never grows the local index** (resolves through `PackageManager::read_only_view` → a `LocalWritePolicy::ReadOnly` index; content warms only the GC-able `fs.blobs`). `InspectOptions { resolve, closure }` selects mode (`closure` implies platform-selection on an image-index root, same as `resolve`). `closure: true` additionally walks the metadata-only dependency closure (module-private `walk_closure`/`gather_closure_nodes`/`fold_effective_visibility`) and returns an `InspectClosure { nodes: Vec<ClosureNode>, interface: Surface, private: Surface, conflicts: ClosureConflicts }`. `Surface { binaries, entrypoints, env: Vec<(PinnedIdentifier, ClosureEnvVar{key,kind,visibility})>, binaries_complete }` is the aggregate on one axis, built by `project_surface(nodes, self_view)`. **Every admission / crossing decision is delegated to the shared `composer::{dep_admitted, carrier_crosses}` surface algebra — the SAME code `ocx env` (`self_view=false`) / `ocx env --self` (`self_view=true`) use — so a surface here equals what the composer emits; inspect never re-derives the rule** (the effective-visibility fold is likewise the shared `ResolvedPackage::with_dependencies`). `admitted_on_surface(node, self_view)` = root unconditionally (no edge visibility) ∨ `composer::dep_admitted(effective, self_view)`; each carrier then crosses via `composer::carrier_crosses(vis, node.is_root, self_view)` under its visibility — env vars their declared one, entry points `metadata::Entrypoints::IMPLICIT_VISIBILITY` (INTERFACE: root's launchers interface-only, a dep's cross onto both surfaces), binaries claims `metadata::Binaries::IMPLICIT_VISIBILITY` (PUBLIC: cross wherever the node is admitted). No per-kind structural rules — membership comes from visibility alone; `self_view` only selects which surface is emitted. `binaries_complete` is the one inspect-only add-on. `ClosureNode` carries `effective_visibility`/`binaries`/`entrypoints`/`env: Vec<ClosureEnvVar>` (unfiltered; per-axis crossing applied at aggregate time)/`dependencies: Vec<ClosureEdge>`/`is_root`; fail-closed — any node error aborts the whole closure, never a partial render. `closure` warms `fs.blobs` (root chain + dep leaf manifests + configs) as unreferenced cache entries via `stage_chain_blobs`/`stage_leaf_manifest` (`common::blob_needs_fetch` check-and-heal); plain inspect without `--closure` performs zero index writes |
| `tasks/select.rs` | `select_all()` — parallel resolve (`find_all`) + sequential `current` wire-up; aggregates per-package failures into `SelectFailed` |
| `tasks/clean.rs` | `clean()` — GC unreferenced objects + stale temps; `collect_project_roots` free function — calls `ProjectRegistry::live_projects()` (flat symlink ledger, no JSON parse), resolves each live project dir's `ocx.lock` pinned digests into `Vec<ProjectRootDigests>`, **plus an implicit `$OCX_HOME/ocx.lock` root** (global toolchain — its project dir is `$OCX_HOME`, barred from the ledger by `adr_project_gc_symlink_ledger.md`, so added unconditionally; absent lock → `Ok(None)` → no-op; ADR `adr_global_toolchain_tier.md` D5 amended 2026-05-19); opportunistically removes legacy `projects.json`/`.projects.lock` if found; no corrupt-registry exit-78 branch (eliminated with the JSON parse surface) |
| `tasks/update_check.rs` | Update-check task methods + throttle machinery. Exports `SkippedReason` enum (`Bootstrap`, `Offline`, `Throttled`, `RegistryProbeFailed(String)`, `NotFound`, `UnparseableCurrent(String)`, `UnparseableLatest`, `NoReleaseTag`) with `Display` + JSON serialization. Exports `UpdateCheckResult` (`AlreadyUpToDate`, `Skipped(SkippedReason)`, `UpdateAvailable(Identifier)`), `SelfUpdateResult` (`AlreadyUpToDate`, `Installed { from: Option<String>, to }`, `Skipped(SkippedReason)`), and `TagProbe` (`Index` / `Remote` — selects where the version-discovery probe lists tags). Public methods: `check_update(id, throttle, probe)`, `self_check_update(throttle, probe)`, `self_update()`. Private helpers: `query_installed_version` (subprocess-based, `tokio::process::Command`), `find_latest_version`. Throttle primitives (`is_throttled`, `touch`) live on `StateStore` (`file_structure/state_store.rs`), not here. `installed_version` / `installed_version_from_paths` deleted (2026-05-27). Full throttle contract and touch policy in the module `//!` doc. |
| `tasks/managed_config.rs` | Managed-config tier task methods (managed-config v2 — config-as-package). `update_managed_config(resolved, expected_digest)` — full fetch+persist (`expected_digest` = `tag@digest` fail-closed assertion, exit 65, snapshot untouched on mismatch); `probe_managed_config_digest(resolved)` — HEAD-based top-digest probe, errors debug-logged then discarded; `check_managed_config_refresh(resolved)` — throttled background tick: pause file short-circuit (`Paused`, zero transport calls) -> throttle -> digest probe -> drift = `!snapshot_matches_source \|\| digest != probed` -> notify advisory / apply fetch+persist. Outcomes: `ManagedConfigUpdateResult`, `ManagedConfigRefreshOutcome` (incl. `Paused`). Fetch/persist/pause primitives live in `crate::managed_config`, not here. |
| `composer.rs` | Two-env composition: `compose(roots, store, self_view: bool) -> ComposeOutput` (flat iteration over each root's pre-built TC with cross-root dedup, surface-gated via the shared predicates below); **the surface algebra `dep_admitted(effective, self_view)` + `carrier_crosses(vis, is_root, self_view)` is the single source of truth for what a surface contains — `compose` AND `package_manager::tasks::inspect::project_surface` both route every admission / crossing decision through it, so `ocx env`/`--self` and `ocx package inspect --closure` can never disagree** (the env-asymmetry class of bug came from inspect re-deriving the env rule). Recursive definition (module comment): `surface(P, axis)` = P's own carriers with `vis.has(axis)` ∪ interface surfaces of deps whose edge reaches the axis; below the root only a dep's INTERFACE surface crosses (Algorithm v3 step 5), edge composition precomputed by `through_edge`/`merge` (`ResolvedPackage::with_dependencies`). Flattened: `dep_admitted` = `has_private()` under `self_view` else `has_interface()` (root always admitted — depth 0); `carrier_crosses` = root's carriers on the surface axis, a dep's only their `has_interface()` side on either surface. Carriers with no declared visibility carry an implicit one on their metadata type: `Entrypoints::IMPLICIT_VISIBILITY` = INTERFACE (launchers are consumer-facing; gates the claim AND the synth-`entrypoints/` PATH push in `emit_root_path_block`/`emit_dep_path_block`, so a claim can never contradict PATH — root's launchers absent under `--self`), `Binaries::IMPLICIT_VISIBILITY` = PUBLIC (raw executables serve consumers and the package's own shims). `self_view` only selects which surface is emitted — never membership; `ComposeOutput { entries, admitted, admitted_binaries: Vec<(PinnedIdentifier, BinaryName)>, admitted_entrypoints: Vec<(PinnedIdentifier, EntrypointName)> }` — the latter two are zero-extra-I/O projections of `.binaries()`/`.entrypoints()` off metadata already loaded during the same admission walk, admitted iff the owning package was admitted to this compose call AND the carrier crosses under its implicit visibility (root packages at depth 0, dep packages iff their TC entry passes the active surface gate — same algebra already gating `entries`); `check_entrypoints(roots, store)` (interface-projection collision gate over 1..N roots, reports all N owners); `check_repo_digest_conflicts(roots, self_view)` (**fatal** version-conflict gate — same `registry/repo` resolved to ≥2 distinct digests on the active surface → `Err(DependencyError::Conflict { repository, identifiers })`, exit 65; two tags → same digest tolerated; sealed/private edges excluded; called by `compose`). The diagnostic `deps` command instead calls the non-fatal `warn_repo_digest_conflicts` so the conflicting tree stays inspectable — both share `collect_repo_digest_conflicts`. ADR `adr_declared_binaries_metadata.md` §4 Decision A. |

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
| `inspect()` / `inspect_all()` | No | `InspectResult` | Read-only: candidates / metadata+layers / resolution chain; no install or symlink side effects; `_all` drains via `drain_package_tasks` (input-order results, index-sorted errors) |
| `select_all()` | No | `Vec<(InstallInfo, WireSelectionOutcome)>` | Parallel resolve (`find_all`) then sequential `current` wire-up; aggregates per-package failures into `SelectFailed` |
| `clean(dry_run, force)` | N/A | `CleanResult` | Removes unreferenced objects + stale temps; `force=true` bypasses project registry |
| `check_update(identifier, throttle, probe)` | N/A | `UpdateCheckResult` | Generic update-check for any identifier. Throttle: `None` = 24h, `Some(ZERO)` = bypass, `Some(d)` = custom. `probe`: `TagProbe::Index` lists tags through `self.index()` (ChainMode-aware — honours `--offline`/`--frozen`/`--remote` + `OCX_INDEX`); `TagProbe::Remote` builds a throwaway remote-only index (offline → `Skipped(Offline)`). Returns `Skipped` on throttle short-circuit (no state touch). |
| `self_check_update(throttle, probe)` | N/A | `UpdateCheckResult` | Convenience wrapper for the canonical `ocx.sh/ocx/cli`. Resolves the running version via subprocess (`ocx --format json version` on the current symlink binary); returns `Skipped(SkippedReason::Bootstrap)` when subprocess fails (binary absent / exec fail / non-zero exit / malformed JSON). Otherwise compares the probed latest against installed; returns `AlreadyUpToDate` when latest ≤ current. Same throttle + `probe` convention as `check_update`. |
| `self_update()` | N/A | `SelfUpdateResult` | Checks via `self_check_update(ZERO, TagProbe::Remote)` (queries the registry live for the newest release, matching the `self setup` bootstrap and the auto-check; `--offline` → `Skipped(Offline)`) then installs via `install_all(candidate=false, select=true)` if newer. Bootstrap mode no longer short-circuits — install always proceeds. Returns `Installed { from: Option<String>, to }` on success (`from` is `None` when subprocess version query fails), `AlreadyUpToDate` if current, `Skipped(SkippedReason)` on soft failure. Wraps check errors in `Error::SelfCheckFailed`. |

**`_all` methods must preserve input order** — caller zips results with original identifiers.

## Update-Check Throttle Convention

All three update-check methods share one `throttle: Option<Duration>` contract:

| Value | Behaviour |
|-------|-----------|
| `None` | Default 24-hour interval (auto-check path from `app.rs`) |
| `Some(Duration::ZERO)` | Bypass; always run the probe (source decided by `TagProbe`) |
| `Some(d)` | Custom interval |

`self_update` always bypasses (explicit user intent — throttle parameter is not exposed).

**State-file touch policy** (in `$OCX_HOME/state/update-check/<slug>`):

- Touch on successful probe (any `UpdateCheckResult` variant returned cleanly).
- Touch on probe error (avoids hammering a broken registry on every command).
- **Do NOT touch** on throttle short-circuit — touching on short-circuit would extend the window indefinitely.

The slug is `to_slug(identifier.to_string())` — replaces all non-alphanumeric characters with `_`. For `ocx.sh/ocx/cli` this produces `ocx_sh_ocx_cli` (no dots). File content is always zero bytes; mtime is the data. See `subsystem-file-structure.md` for the directory contract.

## Parallel vs Sequential

- **Parallel** (via `JoinSet`): `pull_all`, `install_all` (all three phases), `find_all`, `find_or_install_all`, `inspect_all`, `resolve_all`; `resolve_lock`/`resolve_lock_touched` (project-tier, via `resolve_work`); `inspect`'s `--closure` closure gather (`gather_closure_nodes`, bounded to `CLOSURE_FETCH_CONCURRENCY = 8` concurrent fetches, digest-deduped frontier, results indexed for deterministic ordering)
- **Sequential**: `find_symlink_all`, `uninstall_all`, `deselect_all`, `clean`, `select_all` (resolve phase is parallel via `find_all`; only the `current` wire-up loops sequentially) — all local-only (no per-item network), so sequential is correct
- **CLI-layer fan-outs** (index-tagged `JoinSet`, input order preserved): `package info`, `index update`, `pull --dry-run`. Any new multi-package CLI command doing per-item network work MUST fan out this way, not loop `for … { …await }`.

## Garbage Collection: canonical-path keying

`ReachabilityGraph::build` keys `all_entries` and every edge by **canonical**
paths (`canonicalize_or_keep` → `dunce`); `GarbageCollector::build` canonicalizes
each `patch_roots` seed before the membership guard. GC tests that seed real
`tempfile::TempDir` dirs must therefore canonicalize their expected paths before
asserting on `unreachable_objects()` / `orphaned_by_seeds()` — a raw `store.path(..)`
value never matches a canonical key behind a symlinked `/tmp`. See `quality-rust.md`
→ Cross-Platform Path Handling for the general rule (incl. the `!contains` trap).

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

Generated entrypoint launchers re-enter ocx via `ocx launcher exec '<pkg-root>' -- <argv0> [args...]`. Inside `launcher exec`, baked entrypoint `args` (if any) are resolved — with `${installPath}` substituted to the package content directory — and prepended before the user-supplied arguments before the command is executed (wire ABI unchanged). Any subprocess spawn site that may chain back into ocx MUST forward the running ocx's resolution-affecting config onto the child env via `env::Env::apply_ocx_config(ctx.config_view())`. Full rule + Block-tier review criteria live in `subsystem-cli.md` "Cross-Cutting: OCX Configuration Forwarding".

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